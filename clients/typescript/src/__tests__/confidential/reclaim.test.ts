import { prepareRefund } from "../../../examples/confidential/refunds";
import { getTokenDecoder } from "@solana-program/token-2022";
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  createSession,
  readEscrowBalance,
  refundSession,
} from "../../confidential";
import { TestContext, execute } from "./context";
import { createAndFund, fixture, fundAsset } from "./utils";

test("Partial Reclaim followed by Full Reclaim (v0)", async () => {
  const context = new TestContext(),
    f = await fixture(context, 0);
  try {
    await createAndFund(context, f);
    // Fixture expires after one hour; one extra second crosses the boundary.
    context.advanceClock(3601n);
    const partial = f.amount / 2n;
    await execute(
      context,
      f.config,
      await refundSession(
        f.config,
        { kind: "reclaim", input: f.reclaim },
        {
          source: f.source(),
          recipient: context.account(f.common.userBAtaB)!,
          mint: context.account(f.common.mintB)!,
          amount: { kind: "partial", amount: partial },
        },
      ),
    );
    assert.equal(
      readEscrowBalance(f.state(f.common.dvpAtaB), 0n, f.keys).available,
      f.amount - partial,
    );
    await execute(
      context,
      f.config,
      await prepareRefund(
        context.rpc,
        f.config,
        { kind: "reclaim", input: f.reclaim },
        f.keys,
        { kind: "full" },
      ),
    );
    assert.equal(
      readEscrowBalance(f.state(f.common.userBAtaB), 0n, f.buyerKeys).pending,
      f.amount,
    );
  } finally {
    f.close();
  }
});

test("reclaim public leg A without proofs", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    await execute(
      context,
      f.config,
      createSession(f.config, f.create, f.keys, f.amount),
    );
    await fundAsset(context, f);
    const reclaim = await refundSession(f.config, {
      kind: "reclaim",
      input: {
        ...f.reclaim,
        signer: f.userA,
        mint: f.common.mintA,
        dvpSourceAta: f.common.dvpAtaA,
        signerDestAta: f.assetRefund,
        tokenProgram: f.common.tokenProgramA,
      },
    });
    assert.equal(reclaim.preparation.length, 0);
    await execute(context, f.config, reclaim);
    assert.equal(
      getTokenDecoder().decode(context.account(f.assetRefund)!.data).amount,
      f.create.amountA,
    );
  } finally {
    f.close();
  }
});

test("None refund does not need a readable balance", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    await createAndFund(context, f);
    const funded = f.state(f.common.dvpAtaB);
    // The all-zero available ciphertext is all None checks. A false AE with
    // pending credits and no history must not block it.
    const state = {
      ...funded,
      availableBalance: new Uint8Array(funded.availableBalance.length),
      pendingBalanceLo: funded.availableBalance,
      pendingBalanceCreditCounter: 1n,
    };
    await refundSession(
      f.config,
      { kind: "reclaim", input: f.reclaim },
      {
        source: { state, keys: f.keys },
        recipient: context.account(f.common.userBAtaB)!,
        mint: context.account(f.common.mintB)!,
        amount: { kind: "none" },
      },
    );
  } finally {
    f.close();
  }
});
