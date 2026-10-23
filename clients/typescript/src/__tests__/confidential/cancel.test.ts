import assert from "node:assert/strict";
import { test } from "node:test";
import { createSession, refundSession } from "../../confidential";
import { TestContext, execute } from "./context";
import { fixture } from "./utils";

test("cancel an unfunded leg B without proofs", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    await execute(
      context,
      f.config,
      createSession(f.config, f.create, f.keys, f.amount),
    );
    // None is sufficient because no confidential funds ever entered escrow B.
    const cancel = await refundSession(f.config, {
      kind: "cancel",
      input: { ...f.common, settlementAuthority: f.authority },
    });
    assert.equal(cancel.preparation.length, 0);
    await execute(context, f.config, cancel);
    assert.equal(context.account(f.common.swapDvp), undefined);
    assert.equal(context.account(f.common.dvpAtaB), undefined);
  } finally {
    f.close();
  }
});
