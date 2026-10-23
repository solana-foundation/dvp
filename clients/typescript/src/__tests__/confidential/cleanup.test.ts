import { cleanupSession } from "../../../examples/confidential/send-session";
import { address } from "@solana/kit";
import { FailedTransactionMetadata } from "litesvm";
import assert from "node:assert/strict";
import { test } from "node:test";
import { settleSession } from "../../confidential";
import { TestContext, execute, send } from "./context";
import { createAndFund, fixture } from "./utils";

test("clean up interrupted v0 preparation and rebuild the session", async () => {
  const context = new TestContext(),
    f = await fixture(context, 0);
  try {
    await createAndFund(context, f);
    const request = {
      source: f.source(),
      swap: f.swap(),
      expectedAmountB: f.amount,
      recipient: context.account(address(f.recipient))!,
      mint: context.account(f.common.mintB)!,
      surplusRecipient: context.account(f.common.userBAtaB)!,
    };
    const session = await settleSession(f.config, f.settle, request);
    await send(context, f.config, session.preparation[0]!);
    // Only the first preparation landed; the example skips accounts never created.
    await cleanupSession(
      context.rpc,
      f.config,
      session,
      async (transaction) => {
        const result = context.sendTransaction(transaction);
        assert(
          !(result instanceof FailedTransactionMetadata),
          result.toString(),
        );
      },
    );
    for (const ix of session.cleanup)
      assert.equal(context.account(ix.accounts![0]!.address), undefined);
    await execute(
      context,
      f.config,
      await settleSession(f.config, f.settle, request),
    );
  } finally {
    f.close();
  }
});
