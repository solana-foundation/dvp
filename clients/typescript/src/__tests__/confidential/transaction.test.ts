import assert from "node:assert/strict";
import { test } from "node:test";
import {
  address,
  appendTransactionMessageInstruction,
  blockhash,
  createTransactionMessage,
  generateKeyPairSigner,
  getBase64EncodedWireTransaction,
  getTransactionMessageComputeUnitPrice,
  getTransactionMessagePriorityFeeLamports,
  getTransactionMessageSizeLimit,
  pipe,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
  type Address,
  type Base58EncodedBytes,
} from "@solana/kit";
import {
  executedTransactionFromRpc,
  PlannedTransaction,
  type RpcTransaction,
  type SessionConfig,
} from "../../confidential";
import { U64_MAX } from "../../confidential/constants";
import { EscrowKeys } from "../../confidential/keys";
import { ProofKind, SessionBuilder } from "../../confidential/transaction";
import { PubkeyValidityProofData } from "@solana/zk-sdk";

const lifetime = {
  blockhash: blockhash("11111111111111111111111111111111"),
  lastValidBlockHeight: 0n,
};

for (const format of [1, 0] as const) {
  test(`priority fees use the correct units and count toward the size limit (v${format})`, async () => {
    const config: SessionConfig = {
      payer: await generateKeyPairSigner(),
      format,
      minimumBalanceForRentExemption: () => 0n,
      // A non-round product checks that v1 rounds up, rather than truncating.
      computeUnitLimit: 400_001,
    };
    const plan = new PlannedTransaction([]);
    for (const [price, fee] of [
      [undefined, undefined],
      [0n, 0n],
      [3n, 2n],
    ] as const) {
      const message = plan.message(
        { ...config, computeUnitPrice: price },
        lifetime,
      );
      if (message.version === 0)
        assert.equal(getTransactionMessageComputeUnitPrice(message), price);
      else assert.equal(getTransactionMessagePriorityFeeLamports(message), fee);
    }

    // A message fitting exactly without the fee must be rejected once the fee is added.
    const memo = (size: number) =>
      new PlannedTransaction([
        {
          programAddress: address(
            "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",
          ),
          data: new Uint8Array(size),
        },
      ]);
    const limit = getTransactionMessageSizeLimit(
      plan.message(config, lifetime),
    );
    // Keep the same instruction-length encoding width as the final payload.
    const sampleSize = 256;
    const overhead = memo(sampleSize).wireSize(config) - sampleSize;
    const full = memo(limit - overhead);
    assert.equal(full.wireSize(config), limit);
    full.checkSize(config);
    const withFee = { ...config, computeUnitPrice: 3n };
    assert.throws(() => full.checkSize(withFee), {
      name: "ConfidentialError",
      code: "TransactionTooLarge",
      message: `Transaction too large: ${full.wireSize(withFee)} > ${limit}`,
    });
    for (const price of [-1n, U64_MAX + 1n]) {
      assert.throws(
        () => plan.message({ ...config, computeUnitPrice: price }, lifetime),
        { message: "Compute unit price must be in 0..2^64-1" },
      );
    }
  });
}

test("RPC history rejects loaded addresses on legacy and v1 messages", async () => {
  const payer = await generateKeyPairSigner();
  const memo = {
    programAddress: address("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr"),
    data: new Uint8Array(1),
  };
  const legacy = await signTransactionMessageWithSigners(
    pipe(
      createTransactionMessage({ version: "legacy" }),
      (m) => setTransactionMessageFeePayerSigner(payer, m),
      (m) => setTransactionMessageLifetimeUsingBlockhash(lifetime, m),
      (m) => appendTransactionMessageInstruction(memo, m),
    ),
  );
  const v1 = await new PlannedTransaction([memo]).sign(
    { payer, format: 1, minimumBalanceForRentExemption: () => 0n },
    lifetime,
  );
  const injected = (await generateKeyPairSigner()).address;
  for (const transaction of [legacy, v1]) {
    const rpc = (
      writable: Address[],
      innerInstructions: NonNullable<
        RpcTransaction["meta"]
      >["innerInstructions"],
    ): RpcTransaction => ({
      transaction: [getBase64EncodedWireTransaction(transaction), "base64"],
      meta: {
        err: null,
        loadedAddresses: { writable, readonly: [] },
        innerInstructions,
      },
    });
    assert.equal(executedTransactionFromRpc(rpc([], [])).succeeded, true);
    // Index 2 is past the payer and memo program, so only the injected
    // address could resolve it.
    assert.throws(
      () =>
        executedTransactionFromRpc(
          rpc(
            [injected],
            [
              {
                index: 0,
                instructions: [
                  {
                    programIdIndex: 1,
                    accounts: [2],
                    data: "" as Base58EncodedBytes,
                  },
                ],
              },
            ],
          ),
        ),
      { code: "IncompleteHistory" },
    );
  }
});

test("proof accounts reject rent above the default", async () => {
  const keys = EscrowKeys.fromSeed(new Uint8Array(32).fill(5));
  const proof = new PubkeyValidityProofData(keys.elgamal);
  const payer = await generateKeyPairSigner();
  // Mainnet rent: 6960 lamports per byte, including 128 bytes of overhead.
  const mainnet = (space: number) => (128n + BigInt(space)) * 6960n;
  // An untrusted callback may overpay the payer's lamports to the authority.
  for (const [rent, accepted] of [
    [mainnet, true],
    [(space: number) => mainnet(space) + 1n, false],
    [() => -1n, false],
  ] as const) {
    const session = new SessionBuilder(
      { payer, format: 1, minimumBalanceForRentExemption: rent },
      payer,
    ).proof(ProofKind.Pubkey, proof);
    if (accepted) await session;
    else await assert.rejects(session, { code: "Transaction" });
  }
});
