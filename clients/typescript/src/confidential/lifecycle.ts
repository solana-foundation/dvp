import {
  downgradeRoleToNonSigner,
  getAddressEncoder,
  type AccountMeta,
  type Address,
  type EncodedAccount,
  type Instruction,
} from "@solana/kit";
import {
  getTokenDecoder,
  TOKEN_2022_PROGRAM_ADDRESS,
} from "@solana-program/token-2022";
import { getVerifyProofInstruction } from "@solana-program/zk-elgamal-proof";
import {
  CiphertextCiphertextEqualityProofData,
  PubkeyValidityProofData,
  ZeroCiphertextProofData,
} from "@solana/zk-sdk";
import {
  getCreateConfidentialDvpInstruction,
  getApplyConfidentialDvpInstruction,
  getSettleConfidentialDvpInstruction,
  getReclaimConfidentialDvpInstruction,
  getCancelConfidentialDvpInstruction,
  getRejectConfidentialDvpInstruction,
  getRecoverConfidentialDvpInstruction,
  type CreateConfidentialDvpInput,
  type ApplyConfidentialDvpInput,
  type SettleConfidentialDvpInput,
  type ReclaimConfidentialDvpInput,
  type CancelConfidentialDvpInput,
  type RejectConfidentialDvpInput,
  type RecoverConfidentialDvpInput,
} from "../generated/instructions";
import type { LegBRefundArgs } from "../generated/types";
import {
  findSwapDvpEscrowAta,
  findSwapDvpPda,
  type ConfidentialSwapDvp,
} from "../verify";
import { EscrowKeys, type ConfidentialAccountKeys } from "./keys";
import { AMOUNT_LO_BITS, MAX_TRANSFER_AMOUNT } from "./constants";
import {
  readAvailableBalance,
  readEscrowBalance,
  recoverEscrowBalance,
  checkEscrowKeys,
  confidentialState,
  mintAuditor,
  type BalanceEvent,
  type ConfidentialTransferAccount,
} from "./balance";
import { assertU64, bytesEqual, parseCiphertext } from "./math";
import { prepareTransfer, checkRecipient } from "./proofs";
import { SessionBuilder, ProofKind, type SessionConfig } from "./transaction";
import { ConfidentialError } from "./errors";

export type TransferSource<
  TKeys extends ConfidentialAccountKeys = ConfidentialAccountKeys,
> = Readonly<{
  state: ConfidentialTransferAccount;
  keys: TKeys;
  history?: readonly BalanceEvent[];
}>;
export type HookExtras = Readonly<{
  legA?: readonly AccountMeta[];
  legB?: readonly AccountMeta[];
}>;

export function hookExtras(extras: HookExtras): AccountMeta[] {
  const a = extras.legA ?? [],
    b = extras.legB ?? [];
  if (a.length > 32 || b.length > 32)
    throw new ConfidentialError("Account", "At most 32 hook extras per leg");
  return [...a, ...b].map((meta) => ({
    address: meta.address,
    role: downgradeRoleToNonSigner(meta.role),
  }));
}

function appendExtras(ix: Instruction, extras: HookExtras): Instruction {
  return { ...ix, accounts: [...(ix.accounts ?? []), ...hookExtras(extras)] };
}

export type CreateSessionInput = Omit<
  CreateConfidentialDvpInput,
  | "amountBCiphertextLo"
  | "amountBCiphertextHi"
  | "decryptableZeroBalance"
  | "pubkeyValidityProofOffset"
>;
export function createSession(
  config: SessionConfig,
  input: CreateSessionInput,
  keys: EscrowKeys,
  amount: bigint,
) {
  const ciphertext = keys.encryptAmount(amount),
    proof = new PubkeyValidityProofData(keys.elgamal),
    zero = keys.ae.encrypt(0n);
  try {
    // Rust passes settlement_authority here. Create prepares no contexts, so the
    // session authority is unused and the payer is equivalent.
    return new SessionBuilder(config, config.payer).finish([
      getVerifyProofInstruction({
        discriminator: ProofKind.Pubkey,
        proofData: proof.toBytes(),
      }),
      getCreateConfidentialDvpInstruction(
        {
          ...input,
          amountBCiphertextLo: Array.from(ciphertext.lo),
          amountBCiphertextHi: Array.from(ciphertext.hi),
          decryptableZeroBalance: Array.from(zero.toBytes()),
          pubkeyValidityProofOffset: -1,
        },
        { programAddress: config.programAddress },
      ),
    ]);
  } finally {
    proof.free();
    zero.free();
  }
}

export type ApplySessionInput = Omit<
  ApplyConfidentialDvpInput,
  "expectedPendingBalanceCreditCounter" | "newDecryptableAvailableBalance"
>;
export function applySession(
  config: SessionConfig,
  input: ApplySessionInput,
  source: TransferSource,
) {
  // The program does not check the AE that Apply writes. When history is
  // supplied, the snapshot must match it rather than only itself.
  const balance = source.history?.length
    ? recoverEscrowBalance(source.state, 0n, source.keys, source.history)
    : readEscrowBalance(source.state, 0n, source.keys);
  const appliedBalance = balance.available + balance.pending;
  assertU64(appliedBalance);
  const ae = source.keys.ae.encrypt(appliedBalance);
  try {
    return new SessionBuilder(config, input.signer).finish([
      getApplyConfidentialDvpInstruction(
        {
          ...input,
          expectedPendingBalanceCreditCounter: balance.pendingCreditCounter,
          newDecryptableAvailableBalance: Array.from(ae.toBytes()),
        },
        { programAddress: config.programAddress },
      ),
    ]);
  } finally {
    ae.free();
  }
}

type SettleProofFields =
  | "paymentEqualityContext"
  | "paymentValidityContext"
  | "paymentRangeContext"
  | "eqLoContext"
  | "eqHiContext"
  | "zeroContext"
  | "surplusEqualityContext"
  | "surplusValidityContext"
  | "surplusRangeContext"
  | "payment"
  | "surplusB"
  | "legAExtrasCount";
export type SettleSessionInput = Omit<
  SettleConfidentialDvpInput,
  SettleProofFields
>;
export type SettleRequest = Readonly<{
  source: TransferSource<EscrowKeys>;
  swap: ConfidentialSwapDvp;
  expectedAmountB: bigint;
  /** Fetched recipient token accounts, bound to the Settle destinations. */
  recipient: EncodedAccount;
  surplusRecipient: EncodedAccount;
  /**
   * Recipient ElGamal keys from the trade agreement. When omitted, the key
   * read from the recipient snapshot is trusted.
   */
  expectedRecipient?: Uint8Array;
  expectedSurplusRecipient?: Uint8Array;
  /** Fetched mint B account, the source of the optional auditor key. */
  mint: EncodedAccount;
  /**
   * Auditor from application configuration; `null` requires a mint without
   * auditor. When omitted, the mint snapshot is trusted.
   */
  expectedAuditor?: Uint8Array | null;
}>;

export async function settleSession(
  config: SessionConfig,
  input: SettleSessionInput,
  request: SettleRequest,
  extras: HookExtras = {},
) {
  hookExtras(extras);
  if (
    config.format === 0 &&
    (extras.legA?.length ?? 0) + (extras.legB?.length ?? 0) > 0
  )
    throw new ConfidentialError(
      "HookedSettleRequiresV1",
      "Hooked Settle requires v1",
    );
  await checkSettleAccounts(input, request.swap, config.programAddress);
  const { source } = request;
  checkEscrowKeys(source.state, source.keys);
  const expected = source.keys.encryptAmount(request.expectedAmountB);
  if (
    !bytesEqual(expected.lo, request.swap.amountB.lo) ||
    !bytesEqual(expected.hi, request.swap.amountB.hi)
  )
    throw new ConfidentialError(
      "AmountMismatch",
      "Confidential amount mismatch",
    );
  const balance = readAvailableBalance(
    source.state,
    source.keys,
    source.history,
  );
  if (balance < request.expectedAmountB)
    throw new ConfidentialError(
      "InsufficientAvailable",
      "Insufficient available balance",
    );
  const surplus = balance - request.expectedAmountB;
  if (surplus > MAX_TRANSFER_AMOUNT)
    throw new ConfidentialError(
      "SurplusTooLarge",
      "Surplus exceeds 2^48-1; reclaim partially first",
    );
  // Payment and surplus each add one credit when they share the same recipient.
  const paymentCredits =
    surplus > 0n && input.userADestinationAtaB === input.userBAtaB ? 2n : 1n;
  const mint = request.swap.base.mintB;
  const recipient = readRecipient(
    request.recipient,
    input.userADestinationAtaB,
    mint,
    request.expectedRecipient,
  );
  checkRecipient(recipient, paymentCredits);
  const surplusRecipient =
    surplus > 0n
      ? readRecipient(
          request.surplusRecipient,
          input.userBAtaB,
          mint,
          request.expectedSurplusRecipient,
        )
      : undefined;
  if (surplusRecipient) checkRecipient(surplusRecipient, paymentCredits);
  const auditor = readAuditor(request.mint, mint, request.expectedAuditor);
  const session = new SessionBuilder(config, input.settlementAuthority);
  const payment = await prepareTransfer(
    session,
    source.keys,
    new Uint8Array(source.state.availableBalance),
    balance,
    request.expectedAmountB,
    recipient,
    auditor,
  );
  const [paymentEqualityContext, paymentValidityContext, paymentRangeContext] =
    payment.contexts;

  const binding: Address[] = [];
  const limbs = [
    request.expectedAmountB & ((1n << AMOUNT_LO_BITS) - 1n),
    request.expectedAmountB >> AMOUNT_LO_BITS,
  ];
  for (const [index, stored, opening] of [
    [0, expected.lo, source.keys.openingLo],
    [1, expected.hi, source.keys.openingHi],
  ] as const) {
    const limb = parseCiphertext(payment.limbs[index]),
      target = parseCiphertext(stored),
      pubkey = source.keys.elgamal.pubkey();
    const proof = new CiphertextCiphertextEqualityProofData(
      source.keys.elgamal,
      pubkey,
      limb,
      target,
      opening,
      limbs[index]!,
    );
    try {
      binding.push(await session.proof(ProofKind.CiphertextEquality, proof));
    } finally {
      proof.free();
      limb.free();
      target.free();
      pubkey.free();
    }
  }
  const refund = surplusRecipient
    ? await prepareTransfer(
        session,
        source.keys,
        payment.remaining,
        surplus,
        surplus,
        surplusRecipient,
        auditor,
      )
    : undefined;
  const remaining = parseCiphertext(refund?.remaining ?? payment.remaining);
  const zero = new ZeroCiphertextProofData(source.keys.elgamal, remaining);
  let zeroContext: Address;
  try {
    zeroContext = await session.proof(ProofKind.Zero, zero);
  } finally {
    zero.free();
    remaining.free();
  }
  return session.finish([
    appendExtras(
      getSettleConfidentialDvpInstruction(
        {
          ...input,
          paymentEqualityContext,
          paymentValidityContext,
          paymentRangeContext,
          eqLoContext: binding[0]!,
          eqHiContext: binding[1]!,
          zeroContext,
          surplusEqualityContext: refund?.contexts[0],
          surplusValidityContext: refund?.contexts[1],
          surplusRangeContext: refund?.contexts[2],
          payment: payment.data,
          surplusB: refund?.data ?? null,
          legAExtrasCount: extras.legA?.length ?? 0,
        },
        { programAddress: config.programAddress },
      ),
      extras,
    ),
  ]);
}

type RefundProofFields =
  | "equalityContext"
  | "validityContext"
  | "rangeContext"
  | "zeroContext"
  | "legBRefund"
  | "legAExtrasCount";
export type RefundInstruction =
  | {
      kind: "reclaim";
      input: Omit<ReclaimConfidentialDvpInput, RefundProofFields>;
    }
  | {
      kind: "cancel";
      input: Omit<CancelConfidentialDvpInput, RefundProofFields>;
    }
  | {
      kind: "reject";
      input: Omit<RejectConfidentialDvpInput, RefundProofFields>;
    }
  | {
      kind: "recover";
      input: Omit<RecoverConfidentialDvpInput, RefundProofFields>;
    };
export type RefundAmount =
  | { kind: "none" }
  | { kind: "full" }
  | { kind: "partial"; amount: bigint };
export type RefundRequest = Readonly<{
  source: TransferSource;
  /** Fetched refund destination token account. */
  recipient: EncodedAccount;
  /** When omitted, the key read from the recipient snapshot is trusted. */
  expectedRecipient?: Uint8Array;
  amount: RefundAmount;
  /** Fetched mint B account, the source of the optional auditor key. */
  mint: EncodedAccount;
  /** `null` requires a mint without auditor; when omitted, the snapshot is trusted. */
  expectedAuditor?: Uint8Array | null;
}>;

export async function refundSession(
  config: SessionConfig,
  instruction: RefundInstruction,
  request?: RefundRequest,
  extras: HookExtras = {},
) {
  hookExtras(extras);
  if (instruction.kind === "recover" && extras.legA?.length)
    throw new ConfidentialError("Account", "Recover has only leg B extras");
  const authority =
    instruction.kind === "cancel"
      ? instruction.input.settlementAuthority
      : instruction.input.signer;
  const session = new SessionBuilder(config, authority);
  let equalityContext: Address | undefined,
    validityContext: Address | undefined,
    rangeContext: Address | undefined,
    zeroContext: Address | undefined;
  let legBRefund: LegBRefundArgs = { __kind: "None" };
  if (request) {
    const { source } = request;
    if (request.amount.kind === "none") {
      // None needs only the all-zero ciphertext; reading the balance could
      // fail on a false AE without history.
      checkEscrowKeys(source.state, source.keys);
      if (source.state.availableBalance.some((byte) => byte !== 0))
        throw new ConfidentialError(
          "BalanceMismatch",
          "None refund requires an all-zero available ciphertext",
        );
    } else {
      const balance = readAvailableBalance(
        source.state,
        source.keys,
        source.history,
      );
      const amount =
        request.amount.kind === "full" ? balance : request.amount.amount;
      if (request.amount.kind === "partial" && amount === 0n)
        throw new ConfidentialError(
          "ZeroPartialRefund",
          "Partial refund must be nonzero",
        );
      const [destination, mint] =
        instruction.kind === "cancel" || instruction.kind === "reject"
          ? [instruction.input.userBAtaB, instruction.input.mintB]
          : [instruction.input.signerDestAta, instruction.input.mint];
      const transfer = await prepareTransfer(
        session,
        source.keys,
        new Uint8Array(source.state.availableBalance),
        balance,
        amount,
        readRecipient(
          request.recipient,
          destination,
          mint,
          request.expectedRecipient,
        ),
        readAuditor(request.mint, mint, request.expectedAuditor),
      );
      [equalityContext, validityContext, rangeContext] = transfer.contexts;
      legBRefund = {
        __kind: request.amount.kind === "full" ? "Full" : "Partial",
        fields: [transfer.data],
      };
      if (request.amount.kind === "full") {
        const remaining = parseCiphertext(transfer.remaining),
          zero = new ZeroCiphertextProofData(source.keys.elgamal, remaining);
        try {
          zeroContext = await session.proof(ProofKind.Zero, zero);
        } finally {
          zero.free();
          remaining.free();
        }
      }
    }
  }
  const proof = {
    equalityContext,
    validityContext,
    rangeContext,
    zeroContext,
    legBRefund,
    legAExtrasCount: extras.legA?.length ?? 0,
  };
  let ix: Instruction;
  switch (instruction.kind) {
    case "reclaim":
      ix = getReclaimConfidentialDvpInstruction(
        {
          ...instruction.input,
          ...proof,
        },
        { programAddress: config.programAddress },
      );
      break;
    case "cancel":
      ix = getCancelConfidentialDvpInstruction(
        {
          ...instruction.input,
          ...proof,
        },
        { programAddress: config.programAddress },
      );
      break;
    case "reject":
      ix = getRejectConfidentialDvpInstruction(
        {
          ...instruction.input,
          ...proof,
        },
        { programAddress: config.programAddress },
      );
      break;
    case "recover":
      ix = getRecoverConfidentialDvpInstruction(
        {
          ...instruction.input,
          ...proof,
        },
        { programAddress: config.programAddress },
      );
      break;
  }
  return session.finish([appendExtras(ix, extras)]);
}

function readRecipient(
  account: EncodedAccount,
  destination: Address,
  mint: Address,
  expected?: Uint8Array,
): ConfidentialTransferAccount {
  if (
    account.address !== destination ||
    account.programAddress !== TOKEN_2022_PROGRAM_ADDRESS
  )
    throw new ConfidentialError("Account", "Wrong recipient address or owner");
  const token = getTokenDecoder().decode(account.data);
  if (token.mint !== mint)
    throw new ConfidentialError("Account", "Wrong recipient mint");
  const state = confidentialState(token);
  // An unauthenticated key would receive the amount; refuse before any proof.
  if (
    expected &&
    !bytesEqual(
      new Uint8Array(getAddressEncoder().encode(state.elgamalPubkey)),
      expected,
    )
  )
    throw new ConfidentialError("Account", "Recipient ElGamal key mismatch");
  return state;
}

function readAuditor(
  account: EncodedAccount,
  mint: Address,
  expected?: Uint8Array | null,
): Uint8Array | undefined {
  const auditor = mintAuditor(account, mint);
  // An RPC must neither substitute nor add an auditor; refuse before any proof.
  if (
    expected !== undefined &&
    (expected === null
      ? auditor !== undefined
      : auditor === undefined || !bytesEqual(auditor, expected))
  )
    throw new ConfidentialError("Account", "Mint auditor mismatch");
  return auditor;
}

/**
 * The program rejects any other account set, but only after the payer has
 * funded proof contexts bound to the settlement authority; refuse before that.
 */
async function checkSettleAccounts(
  input: SettleSessionInput,
  swap: ConfidentialSwapDvp,
  programAddress?: Address,
) {
  const base = swap.base;
  const [swapDvp] = await findSwapDvpPda({ ...base, programAddress });
  const ata = async (owner: Address, mint: Address, tokenProgram: Address) =>
    (await findSwapDvpEscrowAta({ swapDvp: owner, mint, tokenProgram }))[0];
  const [programA, programB] = [base.tokenProgramA, base.tokenProgramB];
  for (const [field, actual, expected] of [
    [
      "settlement authority",
      input.settlementAuthority.address,
      base.settlementAuthority,
    ],
    ["swap", input.swapDvp, swapDvp],
    ["mint A", input.mintA, base.mintA],
    ["mint B", input.mintB, base.mintB],
    ["token program A", input.tokenProgramA, programA],
    ["token program B", input.tokenProgramB, programB],
    ["escrow A", input.dvpAtaA, await ata(swapDvp, base.mintA, programA)],
    ["escrow B", input.dvpAtaB, await ata(swapDvp, base.mintB, programB)],
    [
      "user A destination",
      input.userADestinationAtaB,
      await ata(base.userASettlementDestination, base.mintB, programB),
    ],
    [
      "user B destination",
      input.userBDestinationAtaA,
      await ata(base.userBSettlementDestination, base.mintA, programA),
    ],
    [
      "user A surplus account",
      input.userAAtaA,
      await ata(base.userA, base.mintA, programA),
    ],
    [
      "user B surplus account",
      input.userBAtaB,
      await ata(base.userB, base.mintB, programB),
    ],
  ] as const)
    if (actual !== expected)
      throw new ConfidentialError("Account", `Wrong Settle ${field}`);
}
