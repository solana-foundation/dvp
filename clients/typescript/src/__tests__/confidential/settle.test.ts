import { runSwap } from "../../../examples/confidential/swap";
import { prepareSettlement } from "../../../examples/confidential/settle";
import { getTokenDecoder } from "@solana-program/token-2022";
import { address, generateKeyPairSigner } from "@solana/kit";
import { ElGamalKeypair } from "@solana/zk-sdk";
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  readEscrowBalance,
  settleSession,
  verifyConfidentialTerms,
} from "../../confidential";
import { AMOUNT_LO_BITS } from "../../confidential/constants";
import { TestContext, execute } from "./context";
import { createAndFund, fixture, fundAsset, fundEscrow } from "./utils";

for (const format of [1, 0] as const) {
  test(`integrator example: Create, fund, Apply and exact Settle (v${format})`, async () => {
    // Exercise the example against the alternate deployment used for devnet.
    const programAddress = address(
      "bVntZ9Us9Wv8v2hMytmzLs4omXcZf3UwJ5xXbB3TnA6",
    );
    const context = new TestContext(programAddress);
    const f = await fixture(context, format, { programAddress });
    try {
      await runSwap(
        context.rpc,
        f.config,
        { create: f.create, apply: f.apply, settle: f.settle },
        f.keys,
        f.terms,
        {
          send: async (session) => {
            await execute(context, f.config, session);
          },
          fund: async (request) => {
            // The example verifies the newly created escrow before asking wallets to fund.
            assert.equal(request.escrowA, f.common.dvpAtaA);
            assert.equal(request.escrowB, f.common.dvpAtaB);
            assert.equal(request.amountA, f.create.amountA);
            assert.equal(request.amountB, f.amount);
            assert.deepEqual(request.recipient, f.state(request.escrowB));
            await fundAsset(context, f);
            await fundEscrow(context, f, request.amountB);
          },
        },
      );

      // Both legs arrived and both escrows closed; no B surplus went back to the buyer.
      assert.equal(context.account(f.common.swapDvp), undefined);
      assert.equal(context.account(f.common.dvpAtaA), undefined);
      assert.equal(context.account(f.common.dvpAtaB), undefined);
      assert.equal(
        getTokenDecoder().decode(
          context.account(address(f.assetRecipient))!.data,
        ).amount,
        f.create.amountA,
      );
      assert.equal(
        readEscrowBalance(f.state(f.recipient), 0n, f.recipientKeys).pending,
        f.amount,
      );
      assert.equal(
        readEscrowBalance(f.state(f.common.userBAtaB), 0n, f.buyerKeys).pending,
        0n,
      );
    } finally {
      f.close();
    }
  });

  test(`Settle with surplus and an auditor (v${format})`, async () => {
    const context = new TestContext(),
      f = await fixture(context, format, {
        // Nonzero low and high limbs exercise both amount-binding proofs.
        amountB: (1n << AMOUNT_LO_BITS) + 1n,
        auditor: true,
      });
    // Exercise fee-aware proof packing, including SPL Record writes in v0.
    // Record rent lookups to check that each account size is fetched once.
    const rentSizes: number[] = [];
    f.config = {
      ...f.config,
      computeUnitPrice: 3n,
      minimumBalanceForRentExemption: (space) => {
        rentSizes.push(space);
        return context.minimumBalanceForRentExemption(BigInt(space));
      },
    };
    const surplus = (1n << AMOUNT_LO_BITS) + 1n;
    try {
      await createAndFund(context, f, f.amount + surplus);

      // Count only the Settle session; funding builds its own sessions.
      rentSizes.length = 0;
      await execute(
        context,
        f.config,
        await prepareSettlement(
          context.rpc,
          f.config,
          f.settle,
          f.keys,
          f.terms,
        ),
      );
      assert(rentSizes.length > 0);
      assert.equal(new Set(rentSizes).size, rentSizes.length);
      assert.equal(context.account(f.common.swapDvp), undefined);
      assert.equal(
        readEscrowBalance(f.state(address(f.recipient)), 0n, f.recipientKeys)
          .pending,
        f.amount,
      );
      assert.equal(
        readEscrowBalance(f.state(f.common.userBAtaB), 0n, f.buyerKeys).pending,
        surplus,
      );
      assert.equal(
        getTokenDecoder().decode(
          context.account(address(f.assetRecipient))!.data,
        ).amount,
        f.create.amountA,
      );
    } finally {
      f.close();
    }
  });
}

test("Settle binds the recipient snapshot and expected key", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    await createAndFund(context, f);
    const pubkey = f.recipientKeys.elgamal.pubkey(),
      other = new ElGamalKeypair(),
      otherPubkey = other.pubkey();
    try {
      const request = {
        source: f.source(),
        swap: f.swap(),
        expectedAmountB: f.amount,
        recipient: context.account(address(f.recipient))!,
        mint: context.account(f.common.mintB)!,
        surplusRecipient: context.account(f.common.userBAtaB)!,
      };
      await settleSession(f.config, f.settle, {
        ...request,
        expectedRecipient: pubkey.toBytes(),
      });
      for (const [override, message] of [
        // A key other than the agreed one would receive the amount.
        [{ expectedRecipient: otherPubkey.toBytes() }, /ElGamal key mismatch/],
        // A snapshot of another account than the Settle destination.
        [{ recipient: request.surplusRecipient }, /address or owner/],
      ] as const)
        await assert.rejects(
          settleSession(f.config, f.settle, { ...request, ...override }),
          { name: "ConfidentialError", code: "Account", message },
        );
    } finally {
      pubkey.free();
      otherPubkey.free();
      other.free();
    }
  } finally {
    f.close();
  }
});

test("Settle binds the mint snapshot and expected auditor", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1, { auditor: true });
  try {
    await createAndFund(context, f);
    const other = new ElGamalKeypair(),
      otherPubkey = other.pubkey();
    try {
      const request = {
        source: f.source(),
        swap: f.swap(),
        expectedAmountB: f.amount,
        recipient: context.account(address(f.recipient))!,
        surplusRecipient: context.account(f.common.userBAtaB)!,
        mint: context.account(f.common.mintB)!,
      };
      await settleSession(f.config, f.settle, {
        ...request,
        expectedAuditor: f.auditor,
      });
      for (const [override, message] of [
        // An RPC must neither remove nor substitute the configured auditor.
        [{ expectedAuditor: null }, /Mint auditor mismatch/],
        [{ expectedAuditor: otherPubkey.toBytes() }, /Mint auditor mismatch/],
        // A snapshot of another account than the swap's mint B.
        [{ mint: context.account(f.common.mintA)! }, /address or owner/],
      ] as const)
        await assert.rejects(
          settleSession(f.config, f.settle, { ...request, ...override }),
          { name: "ConfidentialError", code: "Account", message },
        );
    } finally {
      otherPubkey.free();
      other.free();
    }
  } finally {
    f.close();
  }
});

test("verifyConfidentialTerms compares every public term", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    await createAndFund(context, f);
    const swap = context.account(f.common.swapDvp)!,
      escrow = context.account(f.common.dvpAtaB)!;
    const checked = await verifyConfidentialTerms(
      swap,
      escrow,
      f.keys,
      f.terms,
    );
    assert.equal(checked.swapAddress, f.common.swapDvp);
    assert.equal(checked.escrowA, f.common.dvpAtaA);
    assert.equal(checked.escrowB, f.common.dvpAtaB);
    const other = f.common.dvpAtaA;
    for (const [change, field] of [
      [{ tokenProgramA: other }, "token program A"],
      [{ amountA: f.terms.amountA + 1n }, "amount A"],
      [{ expiryTimestamp: f.terms.expiryTimestamp + 1n }, "expiry"],
      [{ earliestSettlementTimestamp: 1n }, "earliest settlement"],
      [{ userASettlementDestination: other }, "user A settlement destination"],
      [{ userBSettlementDestination: other }, "user B settlement destination"],
    ] as const)
      await assert.rejects(
        verifyConfidentialTerms(swap, escrow, f.keys, {
          ...f.terms,
          ...change,
        }),
        {
          name: "ConfidentialError",
          code: "Account",
          message: `Wrong ${field}`,
        },
      );
    // Seed terms derive another PDA; amount B is bound by the ciphertexts.
    await assert.rejects(
      verifyConfidentialTerms(swap, escrow, f.keys, {
        ...f.terms,
        nonce: f.terms.nonce + 1n,
      }),
      { name: "SwapDvpVerificationError" },
    );
    await assert.rejects(
      verifyConfidentialTerms(swap, escrow, f.keys, {
        ...f.terms,
        amountB: f.terms.amountB + 1n,
      }),
      { name: "ConfidentialError", code: "AmountMismatch" },
    );
  } finally {
    f.close();
  }
});

test("Settle checks the account set against the swap", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    await createAndFund(context, f);
    const signer = await generateKeyPairSigner(),
      other = signer.address;
    const request = {
      source: f.source(),
      swap: f.swap(),
      expectedAmountB: f.amount,
      recipient: context.account(address(f.recipient))!,
      surplusRecipient: context.account(f.common.userBAtaB)!,
      mint: context.account(f.common.mintB)!,
    };
    for (const [change, field] of [
      [{ settlementAuthority: signer }, "settlement authority"],
      [{ swapDvp: other }, "swap"],
      [{ mintA: other }, "mint A"],
      [{ mintB: other }, "mint B"],
      [{ tokenProgramA: other }, "token program A"],
      [{ tokenProgramB: other }, "token program B"],
      [{ dvpAtaA: other }, "escrow A"],
      [{ dvpAtaB: other }, "escrow B"],
      [{ userADestinationAtaB: other }, "user A destination"],
      [{ userBDestinationAtaA: other }, "user B destination"],
      [{ userAAtaA: other }, "user A surplus account"],
      [{ userBAtaB: other }, "user B surplus account"],
    ] as const)
      await assert.rejects(
        settleSession(f.config, { ...f.settle, ...change }, request),
        {
          name: "ConfidentialError",
          code: "Account",
          message: `Wrong Settle ${field}`,
        },
      );
  } finally {
    f.close();
  }
});
