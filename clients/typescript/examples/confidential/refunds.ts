import {
  assertAccountsExist,
  fetchEncodedAccounts,
  type GetMultipleAccountsApi,
  type Rpc,
} from "@solana/kit";
import {
  readEscrowAccount,
  refundSession,
  type BalanceEvent,
  type EscrowKeys,
  type HookExtras,
  type RefundAmount,
  type RefundInstruction,
  type SessionConfig,
} from "../../src/confidential";

/**
 * Reclaim after expiry, Cancel by the settlement authority, Reject by a party,
 * or Recover after closure. Use generated accounts (and original seed args for
 * Recover); each operation still enforces its on-chain authorization rules.
 * Full refunds spend available only. If late credits remain, Apply and Recover
 * again from a fresh snapshot. None withdraws public tokens only after CT is empty.
 */
export async function prepareRefund(
  rpc: Rpc<GetMultipleAccountsApi>,
  config: SessionConfig,
  instruction: RefundInstruction,
  keys: EscrowKeys,
  amount: RefundAmount,
  history: readonly BalanceEvent[] = [],
  extras: HookExtras = {},
) {
  const { swapDvp, mint, escrow, destination } = refundAccounts(instruction);

  // Read the escrow, refund destination and mint from one RPC snapshot.
  const snapshots = await fetchEncodedAccounts(
    rpc,
    [escrow, destination, mint] as const,
    { commitment: "confirmed" },
  );
  assertAccountsExist(snapshots);
  const [escrowAccount, recipient, mintAccount] = snapshots;
  if (!escrowAccount || !recipient || !mintAccount)
    throw new Error("RPC returned an incomplete refund snapshot");
  const { state } = await readEscrowAccount(escrowAccount, swapDvp, mint);

  // Retain the returned session for cleanup if preparation is interrupted.
  return refundSession(
    config,
    instruction,
    {
      source: { state, keys, history },
      // Bound to the refund destination and mint; pass expectedRecipient to
      // stop trusting the RPC for the destination key.
      recipient,
      amount,
      // Pass expectedAuditor (null for none) to stop trusting the RPC for it.
      mint: mintAccount,
    },
    extras,
  );
}

function refundAccounts(instruction: RefundInstruction) {
  switch (instruction.kind) {
    case "reclaim": {
      const { swapDvp, mint, dvpSourceAta, signerDestAta } = instruction.input;
      return {
        swapDvp,
        mint,
        escrow: dvpSourceAta,
        destination: signerDestAta,
      };
    }
    case "cancel":
    case "reject": {
      const { swapDvp, mintB, dvpAtaB, userBAtaB } = instruction.input;
      return { swapDvp, mint: mintB, escrow: dvpAtaB, destination: userBAtaB };
    }
    case "recover": {
      const { swapDvp, mint, dvpEscrowAta, signerDestAta } = instruction.input;
      return {
        swapDvp,
        mint,
        escrow: dvpEscrowAta,
        destination: signerDestAta,
      };
    }
  }
}
