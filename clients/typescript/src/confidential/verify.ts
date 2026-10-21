import { isSome, type Address, type EncodedAccount } from "@solana/kit";
import {
  decodeSwapDvpChecked,
  findSwapDvpEscrowAta,
  findSwapDvpPda,
  SwapDvpVerificationError,
} from "../verify";
import type { EscrowKeys } from "./keys";
import { readEscrowAccount, verifyConfidentialSwap } from "./balance";
import { ConfidentialError } from "./errors";

/** Verify caller-fetched snapshots before funding, without imposing an RPC transport. */
export async function verifyConfidentialFunding(
  rawSwap: EncodedAccount,
  rawEscrow: EncodedAccount,
  keys: EscrowKeys,
  expectedAmount: bigint,
  programAddress?: Address,
) {
  const account = decodeSwapDvpChecked(rawSwap, programAddress);
  if (account.data.mode !== "confidential")
    throw new SwapDvpVerificationError("Expected a confidential swap");
  const [expected] = await findSwapDvpPda({ ...account.data, programAddress });
  if (account.address !== expected)
    throw new SwapDvpVerificationError("Wrong swap PDA");
  const escrow = await readEscrowAccount(
    rawEscrow,
    expected,
    account.data.mintB,
  );
  verifyConfidentialSwap(
    account.data.confidential,
    escrow.state,
    keys,
    expectedAmount,
  );
  return { swap: account.data.confidential, ...escrow };
}

/**
 * Public terms of an agreed confidential trade. Destinations are resolved:
 * Create stores a user's own address when no destination is given.
 */
export type ExpectedConfidentialTerms = Readonly<{
  settlementAuthority: Address;
  userA: Address;
  userB: Address;
  mintA: Address;
  mintB: Address;
  tokenProgramA: Address;
  amountA: bigint;
  amountB: bigint;
  nonce: bigint;
  expiryTimestamp: bigint;
  earliestSettlementTimestamp: bigint | null;
  userASettlementDestination: Address;
  userBSettlementDestination: Address;
}>;

/**
 * Funding and settlement gate: `verifyConfidentialFunding` at the swap PDA
 * derived from `terms`, then every public term compared with the agreed
 * trade. The PDA covers the parties, mints, settlement authority and nonce;
 * the ciphertexts cover amount B.
 */
export async function verifyConfidentialTerms(
  rawSwap: EncodedAccount,
  rawEscrow: EncodedAccount,
  keys: EscrowKeys,
  terms: ExpectedConfidentialTerms,
  programAddress?: Address,
) {
  const [swapAddress] = await findSwapDvpPda({ ...terms, programAddress });
  if (rawSwap.address !== swapAddress)
    throw new SwapDvpVerificationError("Swap is not at the agreed terms' PDA");
  const checked = await verifyConfidentialFunding(
    rawSwap,
    rawEscrow,
    keys,
    terms.amountB,
    programAddress,
  );
  const base = checked.swap.base;
  const earliest = isSome(base.earliestSettlementTimestamp)
    ? base.earliestSettlementTimestamp.value
    : null;
  for (const [field, matches] of [
    ["token program A", base.tokenProgramA === terms.tokenProgramA],
    ["amount A", base.amountA === terms.amountA],
    ["expiry", base.expiryTimestamp === terms.expiryTimestamp],
    ["earliest settlement", earliest === terms.earliestSettlementTimestamp],
    [
      "user A settlement destination",
      base.userASettlementDestination === terms.userASettlementDestination,
    ],
    [
      "user B settlement destination",
      base.userBSettlementDestination === terms.userBSettlementDestination,
    ],
  ] as const)
    if (!matches) throw new ConfidentialError("Account", `Wrong ${field}`);
  // Fund these canonical escrows, not counterparty-supplied addresses.
  const [escrowA] = await findSwapDvpEscrowAta({
    swapDvp: swapAddress,
    mint: terms.mintA,
    tokenProgram: terms.tokenProgramA,
  });
  return { ...checked, swapAddress, escrowA, escrowB: rawEscrow.address };
}
