import {
  assertAccountsExist,
  fetchEncodedAccounts,
  type GetMultipleAccountsApi,
  type Rpc,
} from "@solana/kit";
import {
  settleSession,
  verifyConfidentialTerms,
  type EscrowKeys,
  type ExpectedConfidentialTerms,
  type SessionConfig,
  type SettleSessionInput,
} from "../../src/confidential";

/**
 * Happy path after funding both legs and applying leg B's pending credits.
 * Supply accounts derived from the agreed terms, and those terms; the settlement
 * authority re-validates every term before Settle.
 * This example assumes no transfer hooks and a current, consistent AE balance.
 */
export async function prepareSettlement(
  rpc: Rpc<GetMultipleAccountsApi>,
  config: SessionConfig,
  accounts: SettleSessionInput,
  keys: EscrowKeys,
  terms: ExpectedConfidentialTerms,
) {
  // Read the swap, escrow, receiving accounts and auditor from one RPC snapshot.
  const snapshots = await fetchEncodedAccounts(
    rpc,
    [
      accounts.swapDvp,
      accounts.dvpAtaB,
      accounts.userADestinationAtaB,
      accounts.userBAtaB,
      accounts.mintB,
    ] as const,
    { commitment: "confirmed" },
  );
  assertAccountsExist(snapshots);
  const [swap, escrow, recipient, refund, mint] = snapshots;
  if (!swap || !escrow || !recipient || !refund || !mint)
    throw new Error("RPC returned an incomplete settlement snapshot");
  const checked = await verifyConfidentialTerms(
    swap,
    escrow,
    keys,
    terms,
    config.programAddress,
  );

  // The client creates payment, optional surplus, amount-binding and zero proofs.
  // Retain the returned session for cleanup if preparation is interrupted.
  return settleSession(config, accounts, {
    source: { state: checked.state, keys },
    swap: checked.swap,
    expectedAmountB: terms.amountB,
    // The session binds both snapshots to the Settle destinations and mint B.
    // Pass expectedRecipient/expectedSurplusRecipient from the trade agreement
    // to stop trusting the RPC for the recipient keys.
    recipient,
    surplusRecipient: refund,
    // Pass expectedAuditor from application configuration (null for none) to
    // stop trusting the RPC for the mint auditor.
    mint,
  });
}
