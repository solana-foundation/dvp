import assert from "node:assert/strict";
import { test } from "node:test";
import { readEscrowBalance, refundSession } from "../../confidential";
import { TestContext, execute } from "./context";
import { createAndFund, fixture } from "./utils";

test("reject with a partial refund leaves remaining escrow funds", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    await createAndFund(context, f);
    const instruction = {
      kind: "reject" as const,
      input: { ...f.common, signer: f.userA },
    };
    // A partial refund closes the swap while leaving escrow funds recoverable.
    await execute(
      context,
      f.config,
      await refundSession(f.config, instruction, {
        source: f.source(),
        recipient: context.account(f.common.userBAtaB)!,
        mint: context.account(f.common.mintB)!,
        amount: { kind: "partial", amount: f.amount / 2n },
      }),
    );
    assert.equal(context.account(f.common.swapDvp), undefined);
    assert.equal(
      readEscrowBalance(f.state(f.common.dvpAtaB), 0n, f.keys).available,
      f.amount / 2n,
    );
    assert.equal(
      readEscrowBalance(f.state(f.common.userBAtaB), 0n, f.buyerKeys).pending,
      f.amount / 2n,
    );
  } finally {
    f.close();
  }
});
