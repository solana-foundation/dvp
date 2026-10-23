/**
 * Checked, verify-before-fund helpers for the DvP swap client.
 *
 * The generated `fetchSwapDvp` / `decodeSwapDvp` decode any bytes at an
 * address with no owner check. Escrows are funded by raw transfers to an ATA
 * derived from the `swap_dvp` pubkey, so an attacker-owned account that
 * decodes as a `SwapDvp` lets a funder deposit into an ATA the attacker drains.
 *
 * These helpers require, before treating an account as a canonical `SwapDvp`:
 * owner is the DvP program, size matches the public or confidential layout, and the
 * address is the canonical PDA for the decoded terms. `findSwapDvpPda` and
 * `findSwapDvpEscrowAta` derive those addresses from agreed terms.
 */
import {
  assertAccountExists,
  decodeAccount,
  fetchEncodedAccount,
  parseBase64RpcAccount,
  getAddressEncoder,
  getProgramDerivedAddress,
  type Account,
  type Address,
  type EncodedAccount,
  type FetchAccountConfig,
  type MaybeEncodedAccount,
  type ReadonlyUint8Array,
  type GetProgramAccountsApi,
  type Rpc,
} from "@solana/kit";
import { getSwapDvpDecoder, type SwapDvp } from "./generated/accounts/swapDvp";
import { DVP_SWAP_PROGRAM_PROGRAM_ADDRESS } from "./generated/programs/dvpSwapProgram";
import { getSafeU64Encoder } from "./safeNumberCodecs";
import { ELGAMAL_CIPHERTEXT_SIZE, U64_MAX } from "./confidential/constants";
import type { AmountCiphertexts } from "./confidential/types";

/** Fixed on-chain size of a `SwapDvp` account (`SwapDvp::LEN`). */
export const SWAP_DVP_ACCOUNT_SIZE = 458;
export const CONFIDENTIAL_SWAP_DVP_ACCOUNT_SIZE =
  SWAP_DVP_ACCOUNT_SIZE + 2 * ELGAMAL_CIPHERTEXT_SIZE;

export type ConfidentialSwapDvp = Readonly<{
  base: SwapDvp;
  amountB: AmountCiphertexts;
}>;
// Common fields stay directly accessible to existing public-swap integrations.
export type SwapDvpAccount = SwapDvp &
  (
    | { mode: "public" }
    | { mode: "confidential"; confidential: ConfidentialSwapDvp }
  );

/** Seed prefix for the `SwapDvp` PDA (`SWAP_DVP_SEED`). */
export const SWAP_DVP_SEED = "dvp";

/** Canonical SPL Associated Token Account program. */
export const ASSOCIATED_TOKEN_PROGRAM_ADDRESS =
  "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL" as Address;

/** Token programs the DvP program accepts for either leg. */
const SUPPORTED_TOKEN_PROGRAMS: readonly Address[] = [
  "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" as Address,
  "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb" as Address,
];

export class SwapDvpVerificationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SwapDvpVerificationError";
  }
}

const textEncoder = new TextEncoder();

/**
 * Decodes a fetched account as a `SwapDvp`, rejecting it unless it is owned
 * by the DvP program and exactly the on-chain size. Use instead of the
 * generated `decodeSwapDvp`. Throws if the account does not exist.
 *
 * This does NOT verify the account sits at its canonical PDA (PDA derivation
 * is async in kit). When checking a counterparty-supplied address before
 * funding, use `verifySwapDvp`, which adds the PDA check, or derive the
 * address with `findSwapDvpPda` and compare it yourself. The Rust
 * `verify::decode_swap_dvp_account` bundles all three checks because it takes
 * the expected address and derives the PDA synchronously.
 */
export function decodeSwapDvpChecked<TAddress extends string = string>(
  encodedAccount: EncodedAccount<TAddress> | MaybeEncodedAccount<TAddress>,
  programAddress: Address = DVP_SWAP_PROGRAM_PROGRAM_ADDRESS,
): Account<SwapDvpAccount, TAddress> {
  if ("exists" in encodedAccount) assertAccountExists(encodedAccount);
  const account = encodedAccount as EncodedAccount<TAddress>;

  if (account.programAddress !== programAddress) {
    throw new SwapDvpVerificationError(
      `Account ${account.address} is not owned by the DvP program ` +
        `(${programAddress}); it is owned by ` +
        `${account.programAddress}. Refusing to treat it as a SwapDvp.`,
    );
  }

  if (account.space !== BigInt(account.data.length)) {
    throw new SwapDvpVerificationError(
      `Account ${account.address} has ${account.data.length} bytes of data ` +
        `but ${account.space} bytes of space; refusing a partial snapshot.`,
    );
  }

  if (
    account.data.length !== SWAP_DVP_ACCOUNT_SIZE &&
    account.data.length !== CONFIDENTIAL_SWAP_DVP_ACCOUNT_SIZE
  ) {
    throw new SwapDvpVerificationError(
      `Account ${account.address} has ${account.data.length} bytes of data; ` +
        `a canonical SwapDvp is ${SWAP_DVP_ACCOUNT_SIZE} or ${CONFIDENTIAL_SWAP_DVP_ACCOUNT_SIZE} bytes.`,
    );
  }

  const decoded = decodeAccount(
    { ...account, data: account.data.slice(0, SWAP_DVP_ACCOUNT_SIZE) },
    getSwapDvpDecoder(),
  );
  for (const [leg, program] of [
    ["A", decoded.data.tokenProgramA],
    ["B", decoded.data.tokenProgramB],
  ] as const) {
    if (!SUPPORTED_TOKEN_PROGRAMS.includes(program))
      throw new SwapDvpVerificationError(
        `Unsupported token program ${leg}: ${program}`,
      );
  }
  if (account.data.length === SWAP_DVP_ACCOUNT_SIZE) {
    return { ...decoded, data: { ...decoded.data, mode: "public" } };
  }
  const hiOffset = SWAP_DVP_ACCOUNT_SIZE + ELGAMAL_CIPHERTEXT_SIZE;
  if (decoded.data.amountB !== U64_MAX)
    throw new SwapDvpVerificationError("Invalid confidential amount sentinel");
  return {
    ...decoded,
    data: {
      ...decoded.data,
      mode: "confidential",
      confidential: {
        base: decoded.data,
        amountB: {
          lo: new Uint8Array(
            account.data.slice(SWAP_DVP_ACCOUNT_SIZE, hiOffset),
          ),
          hi: new Uint8Array(
            account.data.slice(hiOffset, CONFIDENTIAL_SWAP_DVP_ACCOUNT_SIZE),
          ),
        },
      },
    },
  };
}

/**
 * Fetches and checks a `SwapDvp` in one call. Throws if the account is
 * missing, wrong-owner, or wrong-size.
 */
export async function fetchSwapDvpChecked<TAddress extends string = string>(
  rpc: Parameters<typeof fetchEncodedAccount>[0],
  address: Address<TAddress>,
  config?: FetchAccountConfig & { programAddress?: Address },
): Promise<Account<SwapDvpAccount, TAddress>> {
  const { programAddress, ...fetchConfig } = config ?? {};
  const encoded = await fetchEncodedAccount(rpc, address, fetchConfig);
  return decodeSwapDvpChecked(encoded, programAddress);
}

// The nonce is a PDA seed; a JavaScript number above 2^53 would round
// before encoding and derive the wrong address, so require a bigint.
const u64Bytes = (value: bigint): ReadonlyUint8Array =>
  getSafeU64Encoder().encode(value);

/**
 * Derives the canonical `SwapDvp` PDA from agreed terms (on-chain seeds
 * `[b"dvp", settlement_authority, user_a, user_b, mint_a, mint_b, nonce_le]`).
 * Compare this against any address a counterparty supplies.
 */
export function findSwapDvpPda(args: {
  settlementAuthority: Address;
  userA: Address;
  userB: Address;
  mintA: Address;
  mintB: Address;
  nonce: bigint;
  programAddress?: Address;
}): Promise<readonly [Address, number]> {
  const addressEncoder = getAddressEncoder();
  return getProgramDerivedAddress({
    programAddress: args.programAddress ?? DVP_SWAP_PROGRAM_PROGRAM_ADDRESS,
    seeds: [
      textEncoder.encode(SWAP_DVP_SEED),
      addressEncoder.encode(args.settlementAuthority),
      addressEncoder.encode(args.userA),
      addressEncoder.encode(args.userB),
      addressEncoder.encode(args.mintA),
      addressEncoder.encode(args.mintB),
      u64Bytes(args.nonce),
    ],
  });
}

/**
 * Derives a leg's escrow ATA (the `SwapDvp` PDA's ATA for a mint/token
 * program). Funders send raw transfers here, so derive it from a verified
 * `swapDvp` PDA, not from an unverified supplied address.
 */
export function findSwapDvpEscrowAta(args: {
  swapDvp: Address;
  mint: Address;
  tokenProgram: Address;
}): Promise<readonly [Address, number]> {
  const addressEncoder = getAddressEncoder();
  return getProgramDerivedAddress({
    programAddress: ASSOCIATED_TOKEN_PROGRAM_ADDRESS,
    seeds: [
      addressEncoder.encode(args.swapDvp),
      addressEncoder.encode(args.tokenProgram),
      addressEncoder.encode(args.mint),
    ],
  });
}

export function findNonceTombstonePda(
  swapDvp: Address,
  programAddress: Address = DVP_SWAP_PROGRAM_PROGRAM_ADDRESS,
): Promise<readonly [Address, number]> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: [textEncoder.encode("nonce"), getAddressEncoder().encode(swapDvp)],
  });
}

/** Discover both layouts, skipping accounts that fail decoding or canonical PDA verification. */
export async function fetchSwapDvpAccounts(
  rpc: Rpc<GetProgramAccountsApi>,
  programAddress: Address = DVP_SWAP_PROGRAM_PROGRAM_ADDRESS,
) {
  const accounts = (
    await Promise.all(
      [SWAP_DVP_ACCOUNT_SIZE, CONFIDENTIAL_SWAP_DVP_ACCOUNT_SIZE].map((size) =>
        rpc
          .getProgramAccounts(programAddress, {
            encoding: "base64",
            withContext: false,
            filters: [{ dataSize: BigInt(size) }],
          })
          .send(),
      ),
    )
  ).flat();
  const verified = await Promise.all(
    accounts.map(async (raw) => {
      try {
        const checked = decodeSwapDvpChecked(
          parseBase64RpcAccount(raw.pubkey, raw.account),
          programAddress,
        );
        const [expected] = await findSwapDvpPda({
          ...checked.data,
          programAddress,
        });
        return expected === checked.address ? checked : undefined;
      } catch {
        // Program ownership and matching size do not prove DvP initialized the account.
        return undefined;
      }
    }),
  );
  return verified.filter((account) => account !== undefined);
}

/**
 * Checks a fetched `SwapDvp` is
 * program-owned, exact-size, and at the canonical PDA for its decoded terms.
 * Throws `SwapDvpVerificationError` otherwise. Confidential funding additionally
 * requires `verifyConfidentialFunding` to check the escrow, keys and agreed amount.
 */
export async function verifySwapDvp<TAddress extends string = string>(
  rpc: Parameters<typeof fetchEncodedAccount>[0],
  address: Address<TAddress>,
  config?: FetchAccountConfig & { programAddress?: Address },
): Promise<Account<SwapDvpAccount, TAddress>> {
  const account = await fetchSwapDvpChecked(rpc, address, config);
  const { data } = account;
  const [expected] = await findSwapDvpPda({
    settlementAuthority: data.settlementAuthority,
    userA: data.userA,
    userB: data.userB,
    mintA: data.mintA,
    mintB: data.mintB,
    nonce: data.nonce,
    programAddress: config?.programAddress,
  });
  if (expected !== (address as Address)) {
    throw new SwapDvpVerificationError(
      `Account ${address} is program-owned but not at its canonical PDA ` +
        `for the stored terms (expected ${expected}). Refusing to trust it.`,
    );
  }
  return account;
}
