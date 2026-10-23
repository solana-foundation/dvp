import { hkdf } from "@noble/hashes/hkdf.js";
import { sha512 } from "@noble/hashes/sha2.js";
import { getAddressEncoder, type Address } from "@solana/kit";
import { ConfidentialKeys, PedersenOpening } from "@solana/zk-sdk";
import { ristretto255 } from "@noble/curves/ed25519.js";

import type { AmountCiphertexts } from "./types";
import { AMOUNT_LO_BITS, MAX_TRANSFER_AMOUNT } from "./constants";
import { ConfidentialError } from "./errors";
const SEED_DOMAIN = new TextEncoder().encode(
  "dvp/confidential-amount-b/seed/v1",
);
const OPENING_SALT = new TextEncoder().encode("dvp/confidential-amount-b/v1");

/** The callback can use a KMS: the master key never enters the client. */
export async function deriveSharedSeed(
  swapDvp: Address,
  mac: (message: Uint8Array) => Uint8Array | Promise<Uint8Array>,
): Promise<Uint8Array> {
  const seed = await mac(
    new Uint8Array([...SEED_DOMAIN, ...getAddressEncoder().encode(swapDvp)]),
  );
  if (seed.length !== 32)
    throw new Error("Expected a 32-byte HMAC-SHA256 result");
  return seed;
}

export function readLittleEndian(bytes: Uint8Array): bigint {
  return bytes.reduceRight((value, byte) => (value << 8n) | BigInt(byte), 0n);
}

function opening(seed: Uint8Array, label: string): PedersenOpening {
  const wide = hkdf(
    sha512,
    seed,
    OPENING_SALT,
    new TextEncoder().encode(label),
    64,
  );
  let scalar = readLittleEndian(wide) % ristretto255.Point.Fn.ORDER;
  const bytes = new Uint8Array(32);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = Number(scalar & 255n);
    scalar >>= 8n;
  }
  return PedersenOpening.fromBytes(bytes);
}

export class EscrowKeys {
  readonly elgamal;
  readonly ae;
  readonly openingLo;
  readonly openingHi;

  private constructor(seed: Uint8Array) {
    const keys = ConfidentialKeys.fromIkm(seed);
    this.elgamal = keys.elgamal();
    this.ae = keys.ae();
    keys.free();
    this.openingLo = opening(seed, "opening-lo");
    this.openingHi = opening(seed, "opening-hi");
  }

  static fromSeed(seed: Uint8Array): EscrowKeys {
    if (seed.length !== 32) throw new Error("Expected a 32-byte shared seed");
    return new EscrowKeys(seed);
  }

  encryptAmount(amount: bigint): AmountCiphertexts {
    if (amount <= 0n || amount > MAX_TRANSFER_AMOUNT)
      throw new ConfidentialError(
        "InvalidAmount",
        "Amount must be in 1..2^48-1",
      );
    const pubkey = this.elgamal.pubkey();
    const lo = pubkey.encryptWith(
      amount & ((1n << AMOUNT_LO_BITS) - 1n),
      this.openingLo,
    );
    const hi = pubkey.encryptWith(amount >> AMOUNT_LO_BITS, this.openingHi);
    try {
      return { lo: lo.toBytes(), hi: hi.toBytes() };
    } finally {
      lo.free();
      hi.free();
      pubkey.free();
    }
  }

  /** Release the WASM allocations when the integration ends its session. */
  free(): void {
    this.elgamal.free();
    this.ae.free();
    this.openingLo.free();
    this.openingHi.free();
  }
}

/** Existing wallet keys suffice for balances and transfers; no DvP amount openings are needed. */
export type ConfidentialAccountKeys = Pick<EscrowKeys, "elgamal" | "ae">;
