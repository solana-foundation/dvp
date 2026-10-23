import {
  appendTransactionMessageInstructions,
  getTransactionMessageSizeLimit,
  blockhash,
  compileTransaction,
  compressTransactionMessageUsingAddressLookupTables,
  createTransactionMessage,
  generateKeyPairSigner,
  getTransactionEncoder,
  setTransactionMessageComputeUnitLimit,
  setTransactionMessageComputeUnitPrice,
  setTransactionMessagePriorityFeeLamports,
  setTransactionMessageLoadedAccountsDataSizeLimit,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
  type Address,
  type AddressesByLookupTableAddress,
  type Blockhash,
  type Instruction,
  type TransactionSigner,
} from "@solana/kit";
import { getCreateAccountInstruction } from "@solana-program/system";
import {
  RECORD_PROGRAM_ADDRESS,
  RECORD_META_DATA_SIZE,
  getInitializeInstruction,
  getWriteInstruction,
  getCloseAccountInstruction,
} from "@solana-program/record";
import {
  getCloseContextStateInstruction,
  getVerifyProofInstruction,
  ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS,
  CONTEXT_STATE_META_SIZE,
  ZkElGamalProofInstruction,
} from "@solana-program/zk-elgamal-proof";
import { U64_MAX } from "./constants";
import { ConfidentialError } from "./errors";

const MICRO_LAMPORTS_PER_LAMPORT = 1_000_000n;
// SPL Record header: version byte followed by the write authority.
export const RECORD_HEADER_SIZE = Number(RECORD_META_DATA_SIZE);
export const ProofKind = {
  Zero: ZkElGamalProofInstruction.VerifyZeroCiphertext,
  CiphertextEquality:
    ZkElGamalProofInstruction.VerifyCiphertextCiphertextEquality,
  CommitmentEquality:
    ZkElGamalProofInstruction.VerifyCiphertextCommitmentEquality,
  Pubkey: ZkElGamalProofInstruction.VerifyPubkeyValidity,
  Range128: ZkElGamalProofInstruction.VerifyBatchedRangeProofU128,
  GroupedValidity:
    ZkElGamalProofInstruction.VerifyBatchedGroupedCiphertext3HandlesValidity,
} as const;

// Default (mainnet) rent: lamports per byte, including the account overhead.
const ACCOUNT_STORAGE_OVERHEAD = 128n;
const DEFAULT_LAMPORTS_PER_BYTE = 6_960n;

export type SessionConfig = Readonly<{
  payer: TransactionSigner;
  format: 0 | 1;
  programAddress?: Address;
  lookupTables?: AddressesByLookupTableAddress;
  minimumBalanceForRentExemption: (space: number) => bigint | Promise<bigint>;
  computeUnitLimit?: number;
  /** Micro-lamports per CU; v1 converts this to a total fee using the CU limit. */
  computeUnitPrice?: bigint;
  loadedAccountsDataSizeLimit?: number;
}>;
export type TransactionSession = Readonly<{
  preparation: PlannedTransaction[];
  finalTransaction: PlannedTransaction;
  cleanup: Instruction[];
}>;

function transactionSizeLimit(config: SessionConfig): number {
  return getTransactionMessageSizeLimit(
    setTransactionMessageFeePayerSigner(
      config.payer,
      createTransactionMessage({ version: config.format }),
    ),
  );
}

export class PlannedTransaction {
  constructor(readonly instructions: readonly Instruction[]) {}

  message(
    config: SessionConfig,
    lifetime: { blockhash: Blockhash; lastValidBlockHeight: bigint },
  ) {
    const message = setTransactionMessageLifetimeUsingBlockhash(
      lifetime,
      setTransactionMessageFeePayerSigner(
        config.payer,
        appendTransactionMessageInstructions(
          this.instructions,
          config.format === 0
            ? createTransactionMessage({ version: 0 })
            : createTransactionMessage({ version: 1 }),
        ),
      ),
    );
    const computeUnitLimit = config.computeUnitLimit ?? 400_000;
    const budgeted = setTransactionMessageLoadedAccountsDataSizeLimit(
      config.loadedAccountsDataSizeLimit ?? 4_000_000,
      setTransactionMessageComputeUnitLimit(computeUnitLimit, message),
    );
    const price = config.computeUnitPrice;
    // Validate before conversion, including negative values that could round to zero.
    if (price !== undefined && (price < 0n || price > U64_MAX))
      throw new Error("Compute unit price must be in 0..2^64-1");
    if (budgeted.version === 0) {
      return compressTransactionMessageUsingAddressLookupTables(
        setTransactionMessageComputeUnitPrice(price, budgeted),
        config.lookupTables ?? {},
      );
    }
    // v1 stores the total lamports, rounded up like the v0 runtime calculation.
    const fee =
      price === undefined
        ? undefined
        : (price * BigInt(computeUnitLimit) + MICRO_LAMPORTS_PER_LAMPORT - 1n) /
          MICRO_LAMPORTS_PER_LAMPORT;
    return setTransactionMessagePriorityFeeLamports(fee, budgeted);
  }

  wireSize(config: SessionConfig): number {
    return getTransactionEncoder().encode(
      compileTransaction(
        this.message(config, {
          blockhash: blockhash("11111111111111111111111111111111"),
          lastValidBlockHeight: 0n,
        }),
      ),
    ).length;
  }

  checkSize(config: SessionConfig): void {
    const size = this.wireSize(config),
      limit = transactionSizeLimit(config);
    if (size > limit)
      throw new ConfidentialError(
        "TransactionTooLarge",
        `Transaction too large: ${size} > ${limit}`,
      );
  }

  /** Includes temporary account signers carried by the instructions. Send v1 as base64. */
  async sign(
    config: SessionConfig,
    lifetime: { blockhash: Blockhash; lastValidBlockHeight: bigint },
  ) {
    this.checkSize(config);
    return signTransactionMessageWithSigners(this.message(config, lifetime));
  }
}

type Proof = {
  toBytes(): Uint8Array;
  context(): { toBytes(): Uint8Array; free(): void };
};

export class SessionBuilder {
  private preparation: PlannedTransaction[] = [];
  private cleanup: Instruction[] = [];
  // One lookup per account size; a session reuses the same few sizes.
  private rent = new Map<number, Promise<bigint>>();
  constructor(
    private config: SessionConfig,
    private authority: TransactionSigner,
  ) {}

  /**
   * Rent comes from the caller's RPC. Proof contexts close to the authority,
   * so cap it at the default to keep the payer from overpaying.
   */
  private rentExempt(space: number): Promise<bigint> {
    let lamports = this.rent.get(space);
    if (!lamports) {
      lamports = Promise.resolve(
        this.config.minimumBalanceForRentExemption(space),
      ).then((value) => {
        if (
          typeof value !== "bigint" ||
          value < 0n ||
          value >
            (ACCOUNT_STORAGE_OVERHEAD + BigInt(space)) *
              DEFAULT_LAMPORTS_PER_BYTE
        )
          throw new ConfidentialError(
            "Transaction",
            `Rent for ${space} bytes must be between 0 and the default rent-exemption minimum`,
          );
        return value;
      });
      this.rent.set(space, lamports);
    }
    return lamports;
  }

  private append(instructions: Instruction[]): void {
    new PlannedTransaction(instructions).checkSize(this.config);
    const previous = this.preparation.at(-1);
    if (previous) {
      const combined = [...previous.instructions, ...instructions];
      // Range proofs consume about 200k CU; preserve headroom for creates and closes.
      const proofCost = combined
        .filter((ix) => ix.programAddress === ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS)
        .reduce(
          (sum, ix) =>
            sum + (ix.data?.[0] === ProofKind.Range128 ? 210_000 : 25_000),
          30_000,
        );
      const plan = new PlannedTransaction(combined);
      if (
        proofCost <= (this.config.computeUnitLimit ?? 400_000) &&
        plan.wireSize(this.config) <= transactionSizeLimit(this.config)
      ) {
        this.preparation[this.preparation.length - 1] = plan;
        return;
      }
    }
    this.preparation.push(new PlannedTransaction(instructions));
  }

  async proof(kind: number, proof: Proof): Promise<Address> {
    const context = await generateKeyPairSigner();
    const proofContext = proof.context();
    const space = CONTEXT_STATE_META_SIZE + proofContext.toBytes().length;
    proofContext.free();
    const create = getCreateAccountInstruction({
      payer: this.config.payer,
      newAccount: context,
      lamports: await this.rentExempt(space),
      space,
      programAddress: ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS,
    });
    const info = {
      discriminator: kind,
      contextState: context.address,
      contextStateAuthority: this.authority.address,
    };
    const bytes = proof.toBytes();
    const pair = [
      create,
      getVerifyProofInstruction({ ...info, proofData: bytes }),
    ];
    const useRecord =
      this.config.format === 0 &&
      new PlannedTransaction(pair).wireSize(this.config) >
        transactionSizeLimit(this.config);
    if (useRecord) {
      const record = await generateKeyPairSigner(),
        space = RECORD_HEADER_SIZE + bytes.length;
      this.append([
        getCreateAccountInstruction({
          payer: this.config.payer,
          newAccount: record,
          lamports: await this.rentExempt(space),
          space,
          programAddress: RECORD_PROGRAM_ADDRESS,
        }),
        getInitializeInstruction({
          recordAccount: record.address,
          authority: this.config.payer.address,
        }),
      ]);
      let offset = 0;
      while (offset < bytes.length) {
        let end = bytes.length;
        for (;;) {
          const write = getWriteInstruction({
            recordAccount: record.address,
            authority: this.config.payer,
            offset: BigInt(offset),
            data: bytes.subarray(offset, end),
          });
          const size = new PlannedTransaction([write]).wireSize(this.config);
          if (size <= transactionSizeLimit(this.config)) {
            this.append([write]);
            break;
          }
          end -= size - transactionSizeLimit(this.config);
          if (end <= offset)
            throw new ConfidentialError(
              "Transaction",
              "Record write cannot fit in a transaction",
            );
        }
        offset = end;
      }
      const close = getCloseAccountInstruction({
        recordAccount: record.address,
        authority: this.config.payer,
        receiver: this.config.payer.address,
      });
      this.cleanup.push(close);
      this.append([
        create,
        getVerifyProofInstruction({
          ...info,
          proofAccount: record.address,
          offset: RECORD_HEADER_SIZE,
        }),
        close,
      ]);
    } else {
      this.append(pair);
    }
    this.cleanup.push(
      getCloseContextStateInstruction({
        contextState: context.address,
        destination: this.authority.address,
        authority: this.authority,
      }),
    );
    return context.address;
  }

  closeProofContexts(): Instruction[] {
    return this.cleanup.filter(
      (ix) => ix.programAddress === ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS,
    );
  }

  finish(instructions: Instruction[]): TransactionSession {
    const finalTransaction = new PlannedTransaction(instructions);
    finalTransaction.checkSize(this.config);
    return {
      preparation: this.preparation,
      finalTransaction,
      cleanup: this.cleanup,
    };
  }
}
