/**
 * Regression tests: the SwapDvp account codec must mirror the
 * fixed-width on-chain layout (SwapDvp::LEN == 458), where
 * `earliestSettlementTimestamp` always occupies 1 tag byte + 8 payload
 * bytes (the payload after a `0` tag is an ignored sentinel).
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { getAddressDecoder, type ReadonlyUint8Array } from "@solana/kit";
import {
  getSwapDvpDecoder,
  getSwapDvpEncoder,
} from "../generated/accounts/swapDvp";

export const SWAP_DVP_ACCOUNT_SIZE = 458;

const addressOf = (fill: number) =>
  getAddressDecoder().decode(new Uint8Array(32).fill(fill));

const u64le = (value: bigint) => {
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setBigUint64(0, BigInt.asUintN(64, value), true);
  return bytes;
};

/**
 * Builds SwapDvp account bytes exactly as the on-chain `to_bytes` does.
 * `earliest` semantics: `undefined` = None (tag 0 + i64::MAX sentinel,
 * the real on-chain encoding), a bigint = Some(tag 1 + value).
 */
function onChainBytes(earliest?: bigint): Uint8Array {
  const bytes: number[] = [];
  bytes.push(254); // bump
  for (const fill of [1, 2, 3, 4, 5, 6, 7]) {
    bytes.push(...new Uint8Array(32).fill(fill)); // pubkeys
  }
  bytes.push(...u64le(1_000n)); // amount_a
  bytes.push(...u64le(2_500n)); // amount_b
  bytes.push(...u64le(1_780_000_000n)); // expiry_timestamp
  bytes.push(...u64le(42n)); // nonce
  bytes.push(...new Uint8Array(64).fill(8)); // ref_string
  bytes.push(...new Uint8Array(32).fill(9)); // user_a_settlement_destination
  bytes.push(...new Uint8Array(32).fill(10)); // user_b_settlement_destination
  bytes.push(...new Uint8Array(32).fill(11)); // mint_a_authority
  bytes.push(...new Uint8Array(32).fill(12)); // mint_b_authority
  if (earliest === undefined) {
    bytes.push(0);
    bytes.push(...u64le(0x7fffffffffffffffn)); // i64::MAX sentinel
  } else {
    bytes.push(1);
    bytes.push(...u64le(earliest));
  }
  return new Uint8Array(bytes);
}

/** The 450-byte forgery: earliest None as a lone `0` tag, 8 bytes short. */
function shortForgedBytes(): Uint8Array {
  const full = onChainBytes();
  return full.slice(0, full.length - 8);
}

const decode = (bytes: ReadonlyUint8Array) => getSwapDvpDecoder().decode(bytes);

const baseArgs = {
  bump: 254,
  userA: addressOf(1),
  userB: addressOf(2),
  mintA: addressOf(3),
  mintB: addressOf(4),
  settlementAuthority: addressOf(5),
  tokenProgramA: addressOf(6),
  tokenProgramB: addressOf(7),
  amountA: 1_000n,
  amountB: 2_500n,
  expiryTimestamp: 1_780_000_000n,
  nonce: 42n,
  refString: Array.from(new Uint8Array(64).fill(8)),
  userASettlementDestination: addressOf(9),
  userBSettlementDestination: addressOf(10),
  mintAAuthority: addressOf(11),
  mintBAuthority: addressOf(12),
};

describe("SwapDvp account codec", () => {
  it("rejects the 450-byte short-None forgery", () => {
    const forged = shortForgedBytes();
    assert.equal(forged.length, 450);
    assert.throws(() => decode(forged));
  });

  it("decodes the genuine on-chain None encoding (tag 0 + sentinel)", () => {
    const bytes = onChainBytes();
    assert.equal(bytes.length, SWAP_DVP_ACCOUNT_SIZE);
    const decoded = decode(bytes);
    assert.deepEqual(decoded.earliestSettlementTimestamp, { __option: "None" });
    assert.equal(decoded.userA, addressOf(1));
    assert.equal(decoded.settlementAuthority, addressOf(5));
    assert.equal(decoded.amountA, 1_000n);
    assert.equal(decoded.amountB, 2_500n);
    assert.equal(decoded.nonce, 42n);
    assert.equal(decoded.userBSettlementDestination, addressOf(10));
  });

  it("decodes the genuine on-chain Some encoding", () => {
    const decoded = decode(onChainBytes(1_770_000_000n));
    assert.deepEqual(decoded.earliestSettlementTimestamp, {
      __option: "Some",
      value: 1_770_000_000n,
    });
  });

  it("always encodes to the fixed on-chain size, None or Some", () => {
    const encoder = getSwapDvpEncoder();
    const noneBytes = encoder.encode({
      ...baseArgs,
      earliestSettlementTimestamp: null,
    });
    const someBytes = encoder.encode({
      ...baseArgs,
      earliestSettlementTimestamp: 1_770_000_000n,
    });
    assert.equal(noneBytes.length, SWAP_DVP_ACCOUNT_SIZE);
    assert.equal(someBytes.length, SWAP_DVP_ACCOUNT_SIZE);
  });

  it("round-trips Some through encode/decode", () => {
    const encoded = getSwapDvpEncoder().encode({
      ...baseArgs,
      earliestSettlementTimestamp: 1_770_000_000n,
    });
    const decoded = decode(encoded);
    assert.deepEqual(decoded, {
      ...baseArgs,
      earliestSettlementTimestamp: { __option: "Some", value: 1_770_000_000n },
    });
  });

  // 64-bit fields are exact identity: a JS number above 2^53 rounds
  // before encoding. The encoder must reject number, not round it.
  it("rejects a plain number for a 64-bit field instead of rounding it", () => {
    const encoder = getSwapDvpEncoder();
    assert.throws(
      () =>
        encoder.encode({
          ...baseArgs,
          amountA: (2 ** 53 + 1) as unknown as bigint,
          earliestSettlementTimestamp: null,
        }),
      /bigint/i,
    );
  });
});
