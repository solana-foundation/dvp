import { getApplyConfidentialDvpInstruction } from "../..";
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  applySession,
  BalanceHistory,
  ConfidentialError,
  PlannedTransaction,
  readAvailableBalance,
  recoverEscrowBalance,
  verifyConfidentialFunding,
} from "../../confidential";
import { execute, send, TestContext } from "./context";
import { createAndFund, fixture } from "./utils";

test("recover a false AE value from funding history and repair it with Apply", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    const history = await createAndFund(context, f);
    // A stale caller can submit an authenticated AE value that disagrees with ElGamal.
    const falseAe = f.keys.ae.encrypt(1n);
    try {
      history.push(
        await send(
          context,
          f.config,
          new PlannedTransaction([
            getApplyConfidentialDvpInstruction({
              ...f.apply,
              expectedPendingBalanceCreditCounter: 0n,
              newDecryptableAvailableBalance: Array.from(falseAe.toBytes()),
            }),
          ]),
        ),
      );
    } finally {
      falseAe.free();
    }
    await assert.rejects(
      verifyConfidentialFunding(
        context.account(f.common.swapDvp)!,
        context.account(f.common.dvpAtaB)!,
        f.keys,
        f.amount,
      ),
      { name: "ConfidentialError", code: "BalanceMismatch" },
    );

    const replay = new BalanceHistory(f.common.dvpAtaB);
    for (const tx of history) replay.push(tx);
    const state = f.state(f.common.dvpAtaB);
    assert.equal(
      recoverEscrowBalance(state, 0n, f.keys, replay.events()).available,
      f.amount,
    );
    assert.throws(() => recoverEscrowBalance(state, 0n, f.keys, []), {
      name: "ConfidentialError",
      code: "IncompleteHistory",
    });
    // A limb above 32 bits fails SDK decryption; replay reports it as invalid history.
    const pubkey = f.keys.elgamal.pubkey(),
      wide = pubkey.encryptU64(1n << 33n),
      zero = pubkey.encryptU64(0n);
    try {
      assert.throws(
        () =>
          recoverEscrowBalance(state, 0n, f.keys, [
            ...replay.events(),
            { kind: "credit", lo: wide.toBytes(), hi: zero.toBytes() },
          ]),
        {
          name: "ConfidentialError",
          code: "IncompleteHistory",
          message: "Invalid history transfer amount",
        },
      );
    } finally {
      wide.free();
      zero.free();
      pubkey.free();
    }
    // The reader reports the failed history fallback and keeps the false AE as its cause.
    assert.throws(
      () => readAvailableBalance(state, f.keys),
      (error) =>
        error instanceof ConfidentialError &&
        error.code === "IncompleteHistory" &&
        error.cause instanceof ConfidentialError &&
        error.cause.code === "BalanceMismatch",
    );
    await execute(
      context,
      f.config,
      applySession(f.config, f.apply, {
        state,
        keys: f.keys,
        history: replay.events(),
      }),
    );
    await verifyConfidentialFunding(
      context.account(f.common.swapDvp)!,
      context.account(f.common.dvpAtaB)!,
      f.keys,
      f.amount,
    );
  } finally {
    f.close();
  }
});

test("Apply with history rejects a snapshot that history contradicts", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    const history = await createAndFund(context, f);
    const replay = new BalanceHistory(f.common.dvpAtaB);
    for (const tx of history) replay.push(tx);
    const real = f.state(f.common.dvpAtaB);
    const pubkey = f.keys.elgamal.pubkey(),
      zero = pubkey.encryptU64(0n),
      seven = pubkey.encryptU64(7n),
      zeroAe = f.keys.ae.encrypt(0n);
    try {
      // Each snapshot is self-consistent and passes the fast path, but
      // supplied history wins: forged pending limbs (DVP-9) and a replayed
      // zero available balance (DVP-19) must both be rejected.
      const forgedPending = {
        ...real,
        pendingBalanceLow: seven.toBytes(),
        pendingBalanceCreditCounter: 1n,
      };
      const replayedZero = {
        ...real,
        availableBalance: zero.toBytes(),
        decryptableAvailableBalance: zeroAe.toBytes(),
      };
      for (const state of [forgedPending, replayedZero]) {
        applySession(f.config, f.apply, { state, keys: f.keys });
        assert.throws(
          () =>
            applySession(f.config, f.apply, {
              state,
              keys: f.keys,
              history: replay.events(),
            }),
          { name: "ConfidentialError", code: "IncompleteHistory" },
        );
      }
      applySession(f.config, f.apply, {
        state: real,
        keys: f.keys,
        history: replay.events(),
      });
    } finally {
      zero.free();
      seven.free();
      zeroAe.free();
      pubkey.free();
    }
  } finally {
    f.close();
  }
});
