import { ristretto255 } from "@noble/curves/ed25519.js";
import type { ReadonlyUint8Array } from "@solana/kit";
import { ElGamalCiphertext } from "@solana/zk-sdk";
import { type ConfidentialAccountKeys, readLittleEndian } from "./keys";

import { AMOUNT_LO_BITS, U64_MAX } from "./constants";
import { ConfidentialError } from "./errors";

const Point = ristretto255.Point;

export function assertU64(value: bigint): void {
  if (value < 0n || value > U64_MAX)
    throw new ConfidentialError("Arithmetic", "Balance arithmetic overflow");
}

export { bytesEqual } from "@solana/kit";

export function parseCiphertext(bytes: ReadonlyUint8Array): ElGamalCiphertext {
  const ciphertext = ElGamalCiphertext.fromBytes(new Uint8Array(bytes));
  if (!ciphertext)
    throw new ConfidentialError("Account", "Invalid ElGamal ciphertext");
  return ciphertext;
}

function points(bytes: ReadonlyUint8Array) {
  if (bytes.length !== 64) throw new Error("Expected 64 ciphertext bytes");
  return [
    Point.fromBytes(new Uint8Array(bytes.slice(0, 32))),
    Point.fromBytes(new Uint8Array(bytes.slice(32))),
  ] as const;
}

export function ciphertextMatches(
  bytes: ReadonlyUint8Array,
  keys: ConfidentialAccountKeys,
  amount: bigint,
): boolean {
  const [commitment, handle] = points(bytes);
  const secret = keys.elgamal.secret();
  try {
    const scalar = readLittleEndian(secret.toBytes());
    const expected = amount === 0n ? Point.ZERO : Point.BASE.multiply(amount);
    return commitment.subtract(handle.multiply(scalar)).equals(expected);
  } finally {
    secret.free();
  }
}

export function subtractTransfer(
  available: ReadonlyUint8Array,
  lo: ReadonlyUint8Array,
  hi: ReadonlyUint8Array,
): Uint8Array {
  const a = points(available),
    l = points(lo),
    h = points(hi);
  return new Uint8Array(
    a.flatMap((point, index) => [
      ...point
        .subtract(l[index]!)
        .subtract(h[index]!.multiply(1n << AMOUNT_LO_BITS))
        .toBytes(),
    ]),
  );
}

export function extractCiphertext(
  group: ReadonlyUint8Array,
  handle: number,
): Uint8Array {
  if (group.length !== 128 || handle < 0 || handle > 2)
    throw new ConfidentialError("Account", "Invalid grouped ciphertext");
  return new Uint8Array([
    ...group.slice(0, 32),
    ...group.slice(32 + handle * 32, 64 + handle * 32),
  ]);
}

export function decryptLimb(
  bytes: ReadonlyUint8Array,
  keys: ConfidentialAccountKeys,
): bigint {
  const ciphertext = parseCiphertext(bytes),
    secret = keys.elgamal.secret();
  try {
    return secret.decrypt(ciphertext);
  } finally {
    ciphertext.free();
    secret.free();
  }
}
