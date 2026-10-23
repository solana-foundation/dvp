import {
  assertAccountsExist,
  fetchEncodedAccounts,
  type Address,
  type GetMultipleAccountsApi,
  type Rpc,
} from "@solana/kit";
import {
  applySession,
  createSession,
  readEscrowAccount,
  verifyConfidentialTerms,
  type ApplySessionInput,
  type ConfidentialTransferAccount,
  type CreateSessionInput,
  type EscrowKeys,
  type ExpectedConfidentialTerms,
  type SessionConfig,
  type SettleSessionInput,
  type TransactionSession,
} from "../../src/confidential";
import { prepareSettlement } from "./settle";

/**
 * Exact-payment happy path, without hooks or an auditor. Mints, funded wallets
 * and receiving token accounts already exist; v0 also needs an active LUT.
 * Supply generated inputs and the expected terms of the same agreed deal, and
 * keys derived from its seed.
 * The caller owns the keys and frees them after the workflow ends.
 */
export async function runSwap(
  rpc: Rpc<GetMultipleAccountsApi>,
  config: SessionConfig,
  accounts: {
    create: CreateSessionInput;
    apply: ApplySessionInput;
    settle: SettleSessionInput;
  },
  keys: EscrowKeys,
  terms: ExpectedConfidentialTerms,
  wallet: {
    // Confirm each session before returning. Retain it for cleanup on failure.
    // This can call sendSession from send-session.ts with your RPC sender.
    send: (session: TransactionSession) => Promise<void>;
    // Use the parties' SPL wallets; B is a confidential transfer to the checked key.
    // Confirm both deposits before returning. Funding is not a DvP instruction.
    fund: (request: {
      escrowA: Address;
      escrowB: Address;
      amountA: bigint;
      amountB: bigint;
      recipient: ConfidentialTransferAccount;
    }) => Promise<void>;
  },
) {
  // Create the swap and configure its confidential escrow.
  await wallet.send(
    createSession(config, accounts.create, keys, terms.amountB),
  );

  // Verify the on-chain swap, escrow key, private amount and every public term
  // against the agreed trade before either party funds.
  const snapshots = await fetchEncodedAccounts(
    rpc,
    [accounts.create.swapDvp, accounts.create.dvpAtaB] as const,
    { commitment: "confirmed" },
  );
  assertAccountsExist(snapshots);
  const [swap, escrow] = snapshots;
  if (!swap || !escrow)
    throw new Error("RPC returned an incomplete funding snapshot");
  const checked = await verifyConfidentialTerms(
    swap,
    escrow,
    keys,
    terms,
    config.programAddress,
  );
  // Fund the canonical escrows derived from the agreed terms.
  await wallet.fund({
    escrowA: checked.escrowA,
    escrowB: checked.escrowB,
    amountA: terms.amountA,
    amountB: terms.amountB,
    recipient: checked.state,
  });

  // Funding changes pending balances. Refresh the escrow before building Apply.
  const funded = await fetchEncodedAccounts(
    rpc,
    [accounts.apply.dvpAtaB] as const,
    {
      commitment: "confirmed",
    },
  );
  assertAccountsExist(funded);
  if (!funded[0]) throw new Error("RPC returned no funded escrow");
  const source = await readEscrowAccount(
    funded[0],
    accounts.create.swapDvp,
    accounts.create.mintB,
  );
  await wallet.send(
    applySession(config, accounts.apply, { state: source.state, keys }),
  );

  // Refresh again after Apply, then prepare proofs and settle both legs.
  const settlement = await prepareSettlement(
    rpc,
    config,
    accounts.settle,
    keys,
    terms,
  );
  await wallet.send(settlement);
}
