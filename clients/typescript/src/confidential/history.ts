import { RECORD_PROGRAM_ADDRESS } from "@solana-program/record";
import {
  AccountRole,
  decodeTransactionFromRpcResponse,
  getAccountMetasFromCompiledTransactionMessage,
  getInstructionsFromCompiledTransactionMessage,
  getInnerInstructionsFromMeta,
  type GetTransactionApiResponseBase64,
  type Address,
  type Instruction,
} from "@solana/kit";
import {
  identifyToken2022Instruction,
  Token2022Instruction,
  TOKEN_2022_PROGRAM_ADDRESS,
  getConfidentialTransferInstructionDataDecoder,
  getConfidentialTransferWithFeeInstructionDataDecoder,
  getConfidentialMintInstructionDataDecoder,
  getConfidentialBurnInstructionDataDecoder,
  getConfidentialDepositInstructionDataDecoder,
  getConfidentialWithdrawInstructionDataDecoder,
} from "@solana-program/token-2022";
import {
  RecordInstruction,
  getWriteInstructionDataDecoder,
} from "@solana-program/record";
import {
  ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS,
  ZkElGamalProofInstruction,
  getVerifyProofInstructionDataDecoder,
  BATCHED_GROUPED_CIPHERTEXT_3_HANDLES_VALIDITY_CONTEXT_ACCOUNT_SIZE,
  CONTEXT_STATE_META_SIZE,
} from "@solana-program/zk-elgamal-proof";
import { RECORD_HEADER_SIZE, ProofKind } from "./transaction";
import type { BalanceEvent } from "./balance";
import { extractCiphertext } from "./math";
import { ConfidentialError } from "./errors";

export type ExecutedTransaction = Readonly<{
  instructions: readonly Instruction[];
  innerInstructions: ReadonlyMap<number, readonly Instruction[]>;
  succeeded: boolean;
}>;
export type ProofAccountResolver = (
  verification: Instruction,
) => Uint8Array | undefined;
// Three public keys precede the low and high three-handle ciphertexts.
const VALIDITY_KEYS_SIZE = 3 * 32;
const GROUPED_CIPHERTEXT_SIZE = 4 * 32;
const VALIDITY_CONTEXT_SIZE =
  BATCHED_GROUPED_CIPHERTEXT_3_HANDLES_VALIDITY_CONTEXT_ACCOUNT_SIZE -
  CONTEXT_STATE_META_SIZE;

export type RpcTransaction = Readonly<{
  transaction: GetTransactionApiResponseBase64<1>["transaction"];
  meta: Pick<
    NonNullable<GetTransactionApiResponseBase64<1>["meta"]>,
    "err" | "innerInstructions" | "loadedAddresses"
  > | null;
}>;

/** Pass getTransaction with encoding=base64 and maxSupportedTransactionVersion=1. */
export function executedTransactionFromRpc(
  value: RpcTransaction,
): ExecutedTransaction {
  if (!value.meta)
    throw new ConfidentialError(
      "IncompleteHistory",
      "Missing transaction metadata",
    );
  if (value.meta.err !== null)
    return { instructions: [], innerInstructions: new Map(), succeeded: false };
  if (!value.meta.innerInstructions)
    throw new ConfidentialError(
      "IncompleteHistory",
      "Missing inner instruction metadata",
    );
  const { compiledMessage: message, loadedAddresses: decoded } =
    decodeTransactionFromRpcResponse(value);
  // Only v0 messages load lookup addresses; Kit appends them for any version.
  const loaded = message.version === 0 ? decoded : undefined;
  if (
    message.version !== 0 &&
    ((decoded?.writable.length ?? 0) > 0 || (decoded?.readonly.length ?? 0) > 0)
  )
    throw new ConfidentialError(
      "IncompleteHistory",
      "Unexpected lookup addresses",
    );
  if (message.version === 0) {
    const writable =
      message.addressTableLookups?.reduce(
        (n, t) => n + t.writableIndexes.length,
        0,
      ) ?? 0;
    const readonly =
      message.addressTableLookups?.reduce(
        (n, t) => n + t.readonlyIndexes.length,
        0,
      ) ?? 0;
    if (
      (loaded?.writable.length ?? 0) !== writable ||
      (loaded?.readonly.length ?? 0) !== readonly
    )
      throw new ConfidentialError(
        "IncompleteHistory",
        "Missing historical lookup addresses",
      );
  }
  const accountMetas = getAccountMetasFromCompiledTransactionMessage(
    message,
    loaded,
  );
  const instructions = getInstructionsFromCompiledTransactionMessage(
    message,
    loaded,
  ).map(historyInstruction);
  const innerInstructions = new Map<number, Instruction[]>();
  for (const group of value.meta.innerInstructions) {
    if (
      group.index < 0 ||
      group.index >= instructions.length ||
      innerInstructions.has(group.index)
    )
      throw new ConfidentialError(
        "IncompleteHistory",
        "Invalid inner instruction group",
      );
    innerInstructions.set(group.index, []);
  }
  for (const ix of getInnerInstructionsFromMeta(value.meta, accountMetas)) {
    if (ix.trace.kind === "inner")
      innerInstructions.get(ix.trace.outerIndex)!.push(historyInstruction(ix));
  }
  return { instructions, innerInstructions, succeeded: true };
}

function historyInstruction(ix: Instruction): Instruction {
  return {
    programAddress: ix.programAddress,
    data: new Uint8Array(ix.data ?? []),
    accounts: (ix.accounts ?? []).map(({ address }) => ({
      address,
      role: AccountRole.READONLY,
    })),
  };
}

/** Feed successful transactions in block order through the account snapshot being checked. */
export class BalanceHistory {
  private contexts = new Map<Address, Uint8Array | undefined>();
  private records = new Map<Address, Uint8Array>();
  private collected: BalanceEvent[] = [];
  constructor(readonly escrow: Address) {}
  events(): readonly BalanceEvent[] {
    return this.collected;
  }

  /** Discard this instance after a decoding error; it may contain partial transaction state. */
  push(
    tx: ExecutedTransaction,
    resolve: ProofAccountResolver = () => undefined,
  ): void {
    if (!tx.succeeded) return;
    const closedRecords = new Set<Address>();
    for (const [index, top] of tx.instructions.entries()) {
      for (const ix of [top, ...(tx.innerInstructions.get(index) ?? [])]) {
        const data = new Uint8Array(ix.data ?? []);
        if (ix.programAddress === RECORD_PROGRAM_ADDRESS) {
          const address = account(ix, 0);
          if (data[0] === RecordInstruction.Initialize)
            this.records.set(address, new Uint8Array(RECORD_HEADER_SIZE));
          else if (data[0] === RecordInstruction.Write) {
            const write = getWriteInstructionDataDecoder().decode(data);
            const start = Number(write.offset) + RECORD_HEADER_SIZE;
            // Bound untrusted offsets by Solana's maximum account data length.
            if (
              !Number.isSafeInteger(start) ||
              start + write.data.length > 10 * 1024 * 1024
            )
              throw new ConfidentialError(
                "IncompleteHistory",
                "Invalid Record write",
              );
            const previous = this.records.get(address);
            // Unknown Record contents are resolved only if a balance proof
            // needs them. Unrelated writes do not make escrow history incomplete.
            if (!previous) continue;
            const bytes = new Uint8Array(
              Math.max(previous.length, start + write.data.length),
            );
            bytes.set(previous);
            bytes.set(write.data, start);
            this.records.set(address, bytes);
          } else if (data[0] === RecordInstruction.CloseAccount)
            closedRecords.add(address);
        } else if (ix.programAddress === ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS) {
          if (data[0] === ProofKind.GroupedValidity) {
            const proof = getVerifyProofInstructionDataDecoder().decode(data);
            const contextIndex = proof.offset === undefined ? 0 : 1;
            const context = ix.accounts?.[contextIndex]?.address;
            // The proof program creates a context only when its authority
            // follows it; otherwise the verification leaves contexts as is.
            if (context && ix.accounts?.[contextIndex + 1]) {
              try {
                this.contexts.set(context, this.validity(ix, resolve));
              } catch {
                this.contexts.set(context, undefined);
              }
            }
          } else if (data[0] === ZkElGamalProofInstruction.CloseContextState)
            this.contexts.delete(account(ix, 0));
        } else if (ix.programAddress === TOKEN_2022_PROGRAM_ADDRESS)
          this.token(ix, index, tx.instructions, resolve);
      }
    }
    // A closed Record's bytes remain readable until its transaction finishes.
    for (const address of closedRecords) this.records.delete(address);
  }

  private validity(ix: Instruction, resolve: ProofAccountResolver): Uint8Array {
    const data = new Uint8Array(ix.data ?? []);
    if (
      ix.programAddress !== ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS ||
      data[0] !== ProofKind.GroupedValidity
    )
      throw new ConfidentialError("IncompleteHistory", "Wrong validity proof");
    const proof = getVerifyProofInstructionDataDecoder().decode(data);
    let bytes: Uint8Array;
    if (proof.offset !== undefined) {
      const record = this.records.get(account(ix, 0)) ?? resolve(ix);
      if (!record)
        throw new ConfidentialError(
          "IncompleteHistory",
          "Historical proof account data required",
        );
      bytes = record.subarray(proof.offset);
    } else {
      bytes = new Uint8Array(proof.proofData);
    }
    if (bytes.length < VALIDITY_CONTEXT_SIZE)
      throw new ConfidentialError(
        "IncompleteHistory",
        "Truncated validity context",
      );
    return bytes.slice(0, VALIDITY_CONTEXT_SIZE);
  }

  private transferContext(
    ix: Instruction,
    index: number,
    top: readonly Instruction[],
    offsets: readonly number[],
    fixed: number,
    resolve: ProofAccountResolver,
  ): Uint8Array {
    const [equality, validity, range] = offsets;
    if (validity) {
      const proof = top[index + validity];
      if (!proof)
        throw new ConfidentialError(
          "IncompleteHistory",
          "Missing inline proof",
        );
      return this.validity(proof, resolve);
    }
    const position =
      fixed + Number(equality !== 0 || range !== 0) + Number(equality === 0);
    const context = this.contexts.get(account(ix, position));
    if (!context)
      throw new ConfidentialError(
        "IncompleteHistory",
        "Missing verified context history",
      );
    return context;
  }

  private amount(context: Uint8Array, handle: number, incoming: boolean): void {
    this.collected.push({
      kind: incoming ? "credit" : "debit",
      lo: extractCiphertext(
        context.slice(
          VALIDITY_KEYS_SIZE,
          VALIDITY_KEYS_SIZE + GROUPED_CIPHERTEXT_SIZE,
        ),
        handle,
      ),
      hi: extractCiphertext(
        context.slice(
          VALIDITY_KEYS_SIZE + GROUPED_CIPHERTEXT_SIZE,
          VALIDITY_CONTEXT_SIZE,
        ),
        handle,
      ),
    });
  }

  private token(
    ix: Instruction,
    index: number,
    top: readonly Instruction[],
    resolve: ProofAccountResolver,
  ): void {
    const data = ix.data ?? new Uint8Array();
    const offsets = (value: {
      equalityProofInstructionOffset: number;
      ciphertextValidityProofInstructionOffset: number;
      rangeProofInstructionOffset: number;
    }) => [
      value.equalityProofInstructionOffset,
      value.ciphertextValidityProofInstructionOffset,
      value.rangeProofInstructionOffset,
    ];
    let kind: Token2022Instruction;
    try {
      kind = identifyToken2022Instruction(data);
    } catch {
      return;
    }
    if (
      kind === Token2022Instruction.ConfidentialTransfer ||
      kind === Token2022Instruction.ConfidentialTransferWithFee
    ) {
      const source = account(ix, 0),
        destination = account(ix, 2);
      if (source !== this.escrow && destination !== this.escrow) return;
      // DvP mints have no TransferFeeConfig. Token-2022 then executes
      // TransferWithFee as Transfer: same accounts, fee proofs ignored.
      let proofOffsets: number[];
      if (kind === Token2022Instruction.ConfidentialTransfer)
        proofOffsets = offsets(
          getConfidentialTransferInstructionDataDecoder().decode(data),
        );
      else {
        const withFee =
          getConfidentialTransferWithFeeInstructionDataDecoder().decode(data);
        proofOffsets = offsets({
          ...withFee,
          ciphertextValidityProofInstructionOffset:
            withFee.transferAmountCiphertextValidityProofInstructionOffset,
        });
      }
      const context = this.transferContext(
        ix,
        index,
        top,
        proofOffsets,
        3,
        resolve,
      );
      if (source === this.escrow) this.amount(context, 0, false);
      if (destination === this.escrow) this.amount(context, 1, true);
    } else if (
      account(ix, 0) === this.escrow &&
      kind !== Token2022Instruction.ConfidentialMint &&
      kind !== Token2022Instruction.ConfidentialBurn
    ) {
      if (
        kind === Token2022Instruction.ConfigureConfidentialTransferAccount ||
        kind ===
          Token2022Instruction.ConfigureConfidentialTransferAccountWithRegistry
      )
        this.collected = [];
      else if (
        kind === Token2022Instruction.ConfidentialDeposit ||
        kind === Token2022Instruction.ConfidentialWithdraw
      ) {
        this.collected.push({
          kind:
            kind === Token2022Instruction.ConfidentialDeposit
              ? "deposit"
              : "withdraw",
          amount:
            kind === Token2022Instruction.ConfidentialDeposit
              ? getConfidentialDepositInstructionDataDecoder().decode(data)
                  .amount
              : getConfidentialWithdrawInstructionDataDecoder().decode(data)
                  .amount,
        });
      } else if (kind === Token2022Instruction.ApplyConfidentialPendingBalance)
        this.collected.push({ kind: "apply" });
      else if (kind === Token2022Instruction.EmptyConfidentialTransferAccount)
        this.collected.push({ kind: "empty" });
    } else if (
      account(ix, 0) === this.escrow &&
      (kind === Token2022Instruction.ConfidentialMint ||
        kind === Token2022Instruction.ConfidentialBurn)
    ) {
      const incoming = kind === Token2022Instruction.ConfidentialMint;
      const parsed = incoming
        ? getConfidentialMintInstructionDataDecoder().decode(data)
        : getConfidentialBurnInstructionDataDecoder().decode(data);
      this.amount(
        this.transferContext(ix, index, top, offsets(parsed), 2, resolve),
        0,
        incoming,
      );
    }
  }
}

function account(ix: Instruction, index: number): Address {
  const address = ix.accounts?.[index]?.address;
  if (!address)
    throw new ConfidentialError(
      "IncompleteHistory",
      "Missing history instruction account",
    );
  return address;
}
