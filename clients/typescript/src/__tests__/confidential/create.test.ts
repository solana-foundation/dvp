import assert from "node:assert/strict";
import { test } from "node:test";
import { createSession, verifyConfidentialFunding } from "../../confidential";
import { TestContext, execute } from "./context";
import { fixture } from "./utils";

test("create and verify an unfunded confidential swap", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    await execute(
      context,
      f.config,
      createSession(f.config, f.create, f.keys, f.amount),
    );
    const checked = await verifyConfidentialFunding(
      context.account(f.common.swapDvp)!,
      context.account(f.common.dvpAtaB)!,
      f.keys,
      f.amount,
    );
    assert.deepEqual(checked.swap, f.swap());
    assert.deepEqual(checked.state, f.state(f.common.dvpAtaB));
    assert.equal(checked.public, 0n);
  } finally {
    f.close();
  }
});
