/**
 * Checked decode helpers must reject accounts that are not owned
 * by the DvP program or don't match the exact on-chain layout, and the
 * derivation helpers must let funders compute canonical addresses instead
 * of trusting attacker-supplied ones.
 */
import { describe, expect, it } from "@jest/globals";
import {
  getAddressDecoder,
  type Address,
  type MaybeEncodedAccount,
} from "@solana/kit";
import { DVP_SWAP_PROGRAM_PROGRAM_ADDRESS } from "../generated/programs/dvpSwapProgram";
import { getSwapDvpEncoder } from "../generated/accounts/swapDvp";
import {
  decodeSwapDvpChecked,
  findSwapDvpEscrowAta,
  findSwapDvpPda,
  SWAP_DVP_ACCOUNT_SIZE,
} from "../verify";

const SYSTEM_PROGRAM = "11111111111111111111111111111111" as Address;
const TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" as Address;

/** Dummy pubkey made of a single byte repeated 32 times. */
const addressOf = (fill: number) =>
  getAddressDecoder().decode(new Uint8Array(32).fill(fill));

const validData = () =>
  new Uint8Array(
    getSwapDvpEncoder().encode({
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
      refString: Array.from(new Uint8Array(64)),
      userASettlementDestination: addressOf(1),
      userBSettlementDestination: addressOf(2),
      mintAAuthority: addressOf(3),
      mintBAuthority: addressOf(4),
      earliestSettlementTimestamp: null,
    }),
  );

function encodedAccount(overrides: {
  programAddress?: Address;
  data?: Uint8Array;
  exists?: boolean;
}): MaybeEncodedAccount {
  return {
    address: addressOf(11),
    exists: overrides.exists ?? true,
    programAddress:
      overrides.programAddress ?? DVP_SWAP_PROGRAM_PROGRAM_ADDRESS,
    data: overrides.data ?? validData(),
    executable: false,
    lamports: 1_000_000n,
    space: BigInt((overrides.data ?? validData()).length),
  } as MaybeEncodedAccount;
}

describe("decodeSwapDvpChecked", () => {
  it("accepts a program-owned, exact-size account", () => {
    const decoded = decodeSwapDvpChecked(encodedAccount({}));
    expect(decoded.data.amountA).toBe(1_000n);
    expect(decoded.data.earliestSettlementTimestamp).toEqual({
      __option: "None",
    });
  });

  it("rejects a System-owned account even with perfect data", () => {
    expect(() =>
      decodeSwapDvpChecked(encodedAccount({ programAddress: SYSTEM_PROGRAM })),
    ).toThrow(/owned/i);
  });

  it("rejects a wrong-size account", () => {
    expect(() =>
      decodeSwapDvpChecked(encodedAccount({ data: validData().slice(0, 450) })),
    ).toThrow(/458|size|length/i);
  });

  it("rejects a missing account", () => {
    expect(() =>
      decodeSwapDvpChecked(encodedAccount({ exists: false })),
    ).toThrow();
  });

  it("exposes the on-chain account size", () => {
    expect(SWAP_DVP_ACCOUNT_SIZE).toBe(458);
  });
});

describe("canonical derivation helpers (verify-before-fund)", () => {
  // Expected address computed independently of this library with the
  // Solana CLI, mirroring the on-chain seed order
  // [b"dvp", settlement_authority, user_a, user_b, mint_a, mint_b, nonce_le]:
  //
  //   solana find-program-derived-address dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq \
  //     string:dvp \
  //     hex:0505050505050505050505050505050505050505050505050505050505050505 \  (settlement_authority = addressOf(5))
  //     hex:0101010101010101010101010101010101010101010101010101010101010101 \  (user_a = addressOf(1))
  //     hex:0202020202020202020202020202020202020202020202020202020202020202 \  (user_b = addressOf(2))
  //     hex:0303030303030303030303030303030303030303030303030303030303030303 \  (mint_a = addressOf(3))
  //     hex:0404040404040404040404040404040404040404040404040404040404040404 \  (mint_b = addressOf(4))
  //     u64le:42                                                                 (nonce)
  //
  //   => 6uMoF2mAhQD9QTz3CmyvhwKzEumEgodECwTf44GoL9Ki
  it("derives the canonical SwapDvp PDA from agreed terms", async () => {
    const [address] = await findSwapDvpPda({
      settlementAuthority: addressOf(5),
      userA: addressOf(1),
      userB: addressOf(2),
      mintA: addressOf(3),
      mintB: addressOf(4),
      nonce: 42n,
    });
    expect(address).toBe("6uMoF2mAhQD9QTz3CmyvhwKzEumEgodECwTf44GoL9Ki");
  });

  // Escrow ATAs are canonical Associated Token Accounts of the SwapDvp PDA,
  // i.e. seeds [swap_dvp, token_program, mint] under the ATA program.
  // Expected address computed with the Solana CLI:
  //
  //   solana find-program-derived-address ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL \
  //     pubkey:6uMoF2mAhQD9QTz3CmyvhwKzEumEgodECwTf44GoL9Ki \
  //     pubkey:TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA \
  //     hex:0303030303030303030303030303030303030303030303030303030303030303   (mint_a = addressOf(3))
  //
  //   => GBrJyDbxFv8EQ14ww1546RimNv256wEJwVv3LpBDDbEZ
  it("derives the canonical escrow ATA for a leg", async () => {
    const [ata] = await findSwapDvpEscrowAta({
      swapDvp: "6uMoF2mAhQD9QTz3CmyvhwKzEumEgodECwTf44GoL9Ki" as Address,
      mint: addressOf(3),
      tokenProgram: TOKEN_PROGRAM,
    });
    expect(ata).toBe("GBrJyDbxFv8EQ14ww1546RimNv256wEJwVv3LpBDDbEZ");
  });
});

// The nonce is a PDA seed the on-chain program treats as a full-width
// u64. A JavaScript number above 2^53 has already rounded before it can
// be encoded, so distinct nonces would derive the same PDA. The helper
// must require a bigint and reject number outright.
describe("findSwapDvpPda nonce is a lossless u64", () => {
  const baseArgs = {
    settlementAuthority: addressOf(5),
    userA: addressOf(1),
    userB: addressOf(2),
    mintA: addressOf(3),
    mintB: addressOf(4),
  };

  it("rejects an unsafe number nonce instead of rounding it", () => {
    // 2**53 + 1 is not representable as a number; it silently becomes
    // 2**53. The guard must throw rather than derive a rounded PDA.
    expect(() =>
      findSwapDvpPda({
        ...baseArgs,
        nonce: (2 ** 53 + 1) as unknown as bigint,
      }),
    ).toThrow(/bigint/i);
  });

  it("rejects a plain number even when small and safe", () => {
    expect(() =>
      findSwapDvpPda({ ...baseArgs, nonce: 42 as unknown as bigint }),
    ).toThrow(/bigint/i);
  });

  it("derives distinct PDAs for distinct large bigint nonces", async () => {
    const [a] = await findSwapDvpPda({ ...baseArgs, nonce: 2n ** 53n + 1n });
    const [b] = await findSwapDvpPda({ ...baseArgs, nonce: 2n ** 53n + 2n });
    expect(a).not.toBe(b);
  });
});
