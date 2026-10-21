/**
 * The generated u64/i64 instruction args must not silently round unsafe
 * JavaScript numbers. `@solana/kit`'s `getU64Encoder` does `BigInt(value)`
 * on an already-materialized `number`, so any integer above
 * `Number.MAX_SAFE_INTEGER` has lost precision before it is encoded, and
 * distinct on-chain amounts collapse to the same wire bytes. The codegen
 * patch swaps those encoders for guards that reject `number` outright, so
 * callers must pass `bigint`. CreateDvp is the consent point and the
 * program stores/settles the amounts verbatim, so this is enforced there.
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { getCreateDvpInstructionDataEncoder } from "../generated/instructions/createDvp";
import { getSafeI64Encoder, getSafeU64Encoder } from "../safeNumberCodecs";

/** Baseline args with every u64/i64 field a `bigint` (the safe path). */
function args(overrides: Record<string, unknown> = {}) {
  return {
    amountA: 1_000n,
    amountB: 2_000n,
    expiryTimestamp: 1_780_000_000n,
    nonce: 42n,
    refString: null,
    userASettlementDestination: null,
    userBSettlementDestination: null,
    earliestSettlementTimestamp: null,
    ...overrides,
  };
}

describe("CreateDvp u64/i64 args reject unsafe numbers", () => {
  it("encodes distinct large bigints to distinct bytes", () => {
    const encoder = getCreateDvpInstructionDataEncoder();
    const lo = encoder.encode(args({ amountA: 2n ** 53n + 1n }));
    const hi = encoder.encode(args({ amountA: 2n ** 53n + 2n }));
    assert.equal(Buffer.from(lo).equals(Buffer.from(hi)), false);
  });

  it("rejects a plain number for amountA instead of rounding it", () => {
    const encoder = getCreateDvpInstructionDataEncoder();
    // 2**53 + 1 is not representable as a number; it silently becomes
    // 2**53. The guard must throw rather than encode the rounded value.
    assert.throws(() => encoder.encode(args({ amountA: 2 ** 53 + 1 })));
  });

  it("rejects a plain number even when it is small and safe", () => {
    const encoder = getCreateDvpInstructionDataEncoder();
    assert.throws(() => encoder.encode(args({ amountB: 5 })));
  });

  it("rejects a plain number for the i64 expiry field", () => {
    const encoder = getCreateDvpInstructionDataEncoder();
    assert.throws(() =>
      encoder.encode(args({ expiryTimestamp: 1_780_000_000 })),
    );
  });
});

describe("safe 64-bit encoders", () => {
  it("encode a bigint above 2^53 to its exact little-endian bytes", () => {
    // A value a JS number cannot represent; only a bigint round-trips.
    const big = 2n ** 63n - 1n;
    const expectedU64 = new Uint8Array(8);
    new DataView(expectedU64.buffer).setBigUint64(0, big, true);
    assert.deepEqual(
      Buffer.from(getSafeU64Encoder().encode(big)),
      Buffer.from(expectedU64),
    );

    const expectedI64 = new Uint8Array(8);
    new DataView(expectedI64.buffer).setBigInt64(0, -1n, true);
    assert.deepEqual(
      Buffer.from(getSafeI64Encoder().encode(-1n)),
      Buffer.from(expectedI64),
    );
  });

  it("throw on any number, even a safe one", () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    assert.throws(() => getSafeU64Encoder().encode(1 as any), TypeError);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    assert.throws(() => getSafeI64Encoder().encode(1 as any), TypeError);
  });
});
