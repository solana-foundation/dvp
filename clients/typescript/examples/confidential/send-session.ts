import {
  fetchEncodedAccount,
  type GetAccountInfoApi,
  type GetLatestBlockhashApi,
  type Rpc,
} from "@solana/kit";
import {
  PlannedTransaction,
  type SessionConfig,
  type TransactionSession,
} from "../../src/confidential";

type SendAndConfirm = (
  transaction: Awaited<ReturnType<PlannedTransaction["sign"]>>,
) => Promise<void>;

/** sendAndConfirm must submit as base64, check execution errors and await confirmed commitment. */
export async function sendSession(
  rpc: Rpc<GetLatestBlockhashApi>,
  config: SessionConfig,
  session: TransactionSession,
  sendAndConfirm: SendAndConfirm,
) {
  // Each preparation must land before its proof context can be used by Settle.
  for (const plan of [...session.preparation, session.finalTransaction])
    await sendPlan(rpc, config, plan, sendAndConfirm);
}

/**
 * Call only after reconciling transaction status and deciding to abandon the
 * session. A send timeout does not mean the transaction failed. Close only
 * surviving accounts, one transaction at a time; this can resume after partial
 * cleanup. Record accounts need the payer, proof contexts the operation authority.
 */
export async function cleanupSession(
  rpc: Rpc<GetAccountInfoApi & GetLatestBlockhashApi>,
  config: SessionConfig,
  session: TransactionSession,
  sendAndConfirm: SendAndConfirm,
) {
  for (const ix of session.cleanup) {
    const target = ix.accounts?.[0]?.address;
    if (!target) throw new Error("Cleanup instruction has no target account");
    const account = await fetchEncodedAccount(rpc, target, {
      commitment: "confirmed",
    });
    if (account.exists)
      await sendPlan(rpc, config, new PlannedTransaction([ix]), sendAndConfirm);
  }
}

async function sendPlan(
  rpc: Rpc<GetLatestBlockhashApi>,
  config: SessionConfig,
  plan: PlannedTransaction,
  sendAndConfirm: SendAndConfirm,
) {
  // Proofs may take time to prepare; obtain the blockhash immediately before signing.
  const { value: lifetime } = await rpc
    .getLatestBlockhash({ commitment: "confirmed" })
    .send();
  await sendAndConfirm(await plan.sign(config, lifetime));
}
