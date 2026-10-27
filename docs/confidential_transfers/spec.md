# DvP Confidential Transfer: Program Specification

**Status.** Implementation specification, based on the design reviewed on 26 September 2026. This document describes the implemented behavior and the remaining release checks in sections 11 and 13. The current implementation provides test infrastructure, state layouts, instruction ABI checks, shared CT/ZK helpers, all confidential on-chain lifecycle instructions and the Rust and TypeScript confidential clients.

**Source baseline.** Program sources at `solana-foundation/dvp` main `df9919e`, with the deployed mainnet program id (`dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq`).

**Release baseline.** The existing DvP program is deployed on mainnet at
`dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq`. The confidential changes are
not yet deployed and target an in-place upgrade at that same address.
Compatibility is currently checked against source baseline `df9919e`; the exact
commit/tag and binary of the deployed release still need to be pinned for
upgrade and rollback validation.

## 1. Purpose

Add an optional mode to DvP in which leg B (`mint_b`, `amount_b`, the payment) moves through the Token-2022 Confidential Transfer (CT) extension. The amount and the escrow balance exist on-chain only as ciphertexts, readable by `user_a`, `user_b` and `settlement_authority`. Leg A stays public. This assumes authentic recipient keys (section 10).

Preserved properties: atomic Settle; the Settle payment equals the agreed amount, enforced by the program, not by trusting the authority; each party authorizes recovery of its own funds without the other parties, subject to the seed and mint restrictions in section 10; funding without calling the program.

## 2. Requirements and limits

- Support transfer hooks on `mint_b` (section 8).
- Implement in the clients, and describe in the docs, the derivation of the shared seed and of the ElGamal and AE keys from it (section 4).
- Upgrade the existing mainnet program in place without account migration; preserve the public mode and ABI and validate against the deployed release baseline (section 11).

**Amount cap.** A CT transfer carries at most 48 bits, so `amount_b < 2^48` base units (section 4.3):

| `mint_b` decimals | Max `amount_b` (2^48 − 1 base units) |
| ----------------- | ------------------------------------ |
| 6                 | 281,474,976.710655 tokens            |
| 9                 | 281,474.976710655 tokens             |

The cap applies to one transfer, not to escrow B's balance: a larger balance, for example after several credits, is refunded in several `Partial` steps (7.2).

**Manual-approval mints.** On a `mint_b` with `auto_approve_new_accounts = false`, escrow B must be approved by the mint's CT authority before it can be funded (7.3).

## 3. Principle

The leg B escrow ATA (owned by the `SwapDvp` PDA) is configured for CT with an ElGamal key derived from a per-swap shared seed that the three parties hold off-chain. Any party can decrypt the escrow and build proofs; only the program can sign transfers out of it, and only inside its own instructions.

At Create the program stores `Enc(lo)` and `Enc(hi)` of `amount_b` under the escrow key. At Settle the caller brings the CT transfer proofs plus two `CiphertextCiphertextEquality` proofs; the program checks byte-for-byte that they bind the transfer's `lo`/`hi` ciphertexts to the stored ones.

All proofs except `PubkeyValidity` at Create are pre-verified into context-state accounts in preparatory transactions. The program validates them, passes them to Token-2022 CPIs and closes them in the same instruction.

## 4. Shared seed and keys

All three parties must derive identical values, so this section is part of the protocol. Test vectors (4.5) pin it.

### 4.1 Shared seed

`shared_seed` is 32 bytes, one per swap, never stored on-chain.

An operator derives it (the settlement authority's operator or another party the integrator appoints) from a dedicated master key:

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

let mut mac = Hmac::<Sha256>::new_from_slice(&seed_master_key)?;
mac.update(b"dvp/confidential-amount-b/seed/v1");
mac.update(swap_dvp.as_ref());
let shared_seed: [u8; 32] = mac.finalize().into_bytes().into();
```

```ts
import { createHmac } from "node:crypto";
import { getAddressEncoder } from "@solana/kit";

const sharedSeed = createHmac("sha256", seedMasterKey)
  .update("dvp/confidential-amount-b/seed/v1")
  .update(getAddressEncoder().encode(swapDvp))
  .digest();
```

`seed_master_key` is at least 32 bytes and lives in the operator's KMS/HSM; there the same value is one `GenerateMac` (HMAC-SHA256) call over `tag || swap_dvp`, and the key never leaves the KMS. The snippets take the raw key for clarity; the Rust helper `derive_shared_seed(swap, mac)` takes an HMAC callback instead (section 12).

`swap_dvp` (the swap's PDA) is known before Create and unique per swap (the nonce tombstone forbids reuse). `seed_master_key` is a dedicated symmetric secret, separate from the `settlement_authority` signing key. The operator hands `shared_seed` to `user_a` and `user_b`, and to the settlement authority's operator if that is a different party, over a secure off-chain channel of the integrator's choice (out of scope).

Why this derivation is recommended (and not, for example, one from a wallet signature), how participants store the seed, and what a leak or loss means are in [key-management.md](key-management.md).

Every party checks the seed it received against the chain before depositing (section 4.4).

### 4.2 Escrow keys

Rust: [`solana-zk-sdk`](https://crates.io/crates/solana-zk-sdk) >= 7.0.1 ([GitHub](https://github.com/solana-program/zk-elgamal-proof/tree/main/zk-sdk)).

```rust
use solana_zk_sdk::encryption::derivation;

let (escrow_elgamal, escrow_ae) =
    derivation::derive_confidential_keys_from_ikm(&shared_seed)?;
```

TypeScript: [`@solana/zk-sdk`](https://www.npmjs.com/package/@solana/zk-sdk) >= 0.5.4 ([GitHub](https://github.com/solana-program/zk-elgamal-proof/tree/main/zk-sdk-wasm-js)).

```ts
const escrowElgamal = ElGamalKeypair.fromSeed(sharedSeed);
const escrowAe = AeKey.fromSeed(sharedSeed);
```

`ConfidentialKeys.fromIkm(sharedSeed)` returns both keys in one call, like the Rust function.

Both run the same HKDF-SHA512 chain, so the keys are identical in Rust and TypeScript (`@solana/zk-sdk` 0.5.4 wraps `solana-zk-sdk` 8.1.0); the test vectors (4.5) check it.

### 4.3 Amount ciphertexts

```rust
use curve25519_dalek::Scalar; // same version as solana-zk-sdk uses
use hkdf::Hkdf;
use sha2::Sha512;
use solana_zk_sdk::encryption::pedersen::PedersenOpening;

let salt = b"dvp/confidential-amount-b/v1";
let hk = Hkdf::<Sha512>::new(Some(salt), &shared_seed);
let opening = |info: &[u8]| {
    let mut wide = [0u8; 64];
    hk.expand(info, &mut wide).unwrap();
    PedersenOpening::new(Scalar::from_bytes_mod_order_wide(&wide))
};

assert!(amount_b > 0 && amount_b < 1 << 48);
let (lo, hi) = (amount_b & 0xffff, amount_b >> 16);
let pk = escrow_elgamal.pubkey();
let enc_lo = pk.encrypt_with(lo, &opening(b"opening-lo"));
let enc_hi = pk.encrypt_with(hi, &opening(b"opening-hi"));
```

`amount_b` is in base units of `mint_b`. Example: 1,234,567.89 of a 6-decimal token is `amount_b = 1_234_567_890_000`, so `lo = amount_b mod 65536 = 1104` and `hi = amount_b >> 16 = 18_838_011` (`amount_b = hi * 65536 + lo`). The transfer range proof caps `hi` below 2^32, hence `amount_b < 2^48`: 281,474,976.71 tokens at 6 decimals.

Each ciphertext is stored as a 64-byte `PodElGamalCiphertext`. They depend only on `(shared_seed, amount_b)`, so any party recomputes them to verify a swap (4.4) and rebuilds the Settle equality proofs from the seed alone.

TypeScript: these ciphertexts and the Settle equality proofs need `PedersenOpening.fromBytes`, available in `@solana/zk-sdk` 0.5.4 and later.

### 4.4 Verification before funding

Before funding, the Rust client uses `verify::verify_confidential_funding` with
the fetched swap/escrow accounts, keys derived from the shared seed and expected amount B. The lower-level
`verify_confidential_swap` accepts checked account state and derived keys.
`verify::decode_swap_dvp_account` only checks the swap owner, layout and PDA and
returns `SwapDvpAccount`; it does not perform the funding checks below.
TypeScript `verifyConfidentialFunding` enforces the same checks; `verifySwapDvp`
checks only the swap owner, layout and PDA:

1. The escrow ATA's `ConfidentialTransferAccount.elgamal_pubkey` equals `escrow_elgamal.pubkey`.
2. The escrow ATA's `ConfidentialTransferAccount.approved` and `allow_confidential_credits` are both `true`.
3. Recomputed `Enc(lo)`, `Enc(hi)` equal the stored bytes.
4. `0 < expected_amount_b < 2^48`.
5. The escrow's `decryptable_available_balance` decrypts under `escrow_ae`, and that value matches the available ElGamal ciphertext. An authenticated but false AE value fails verification; repair it through a verified Apply before funding.

Check 4 is the only lower bound on amount B: confidential Create cannot reject
a zero amount (7.3), so a funder who skips it can fund leg A for no payment.

These checks do not compare the public terms with the agreed trade. Callers
must also compare parties, mints, settlement authority, amount A,
destinations, nonce, expiry and earliest settlement time before funding or
settlement. A valid PDA and amount B do not prove agreement on those terms.
Rust `verify::verify_confidential_terms` and TypeScript
`verifyConfidentialTerms` do both: they take the agreed terms, require the swap
at their PDA (covering parties, mints, settlement authority and nonce), run
these checks and compare the remaining public terms.

### 4.5 Test vectors

The clients ship a vectors file (`clients/test-vectors/confidential-amount-b.json`) with at least: `seed_master_key + swap_dvp → shared_seed`; `shared_seed → elgamal pubkey, elgamal secret, ae key, opening_lo, opening_hi`; `(shared_seed, amount_b) → Enc(lo), Enc(hi)` for `amount_b ∈ {1, 2^16 − 1, 2^16, 2^48 − 1}`. Rust and TypeScript tests assert them.

### 4.6 Seed loss

Leg B's confidential funds cannot be recovered if no usable seed or escrow-key copy remains and the required keys cannot be reconstructed. Losing one participant's seed copy or the operator's master key alone does not invalidate surviving copies. HMAC avoids separate backups of each secret seed, but operators must preserve the master keys and each swap's key ID/version mapping ([key-management.md](key-management.md#4-storage-and-handling)).

## 5. State and account layout

### 5.1 `SwapDvp`

The account has no discriminator; the program tells account kinds apart by owner and data length:

| Data length   | Meaning                                                                         |
| ------------- | ------------------------------------------------------------------------------- |
| 0             | Nonce tombstone (unchanged)                                                     |
| 458           | `SwapDvp`, public mode. Byte-identical to the mainnet release                   |
| 586           | `SwapDvp`, confidential amount B: the 458-byte base followed by a 128-byte tail |
| anything else | `InvalidAccountData`                                                            |

Base (offsets 0..458) is exactly today's layout, with `earliest_settlement_timestamp` at 449..458 as a fixed 9-byte slot. Tail, starting at 458:

```
458..522  amount_b_ciphertext_lo: [u8; 64]   // Enc(lo) under the escrow key
522..586  amount_b_ciphertext_hi: [u8; 64]   // Enc(hi)
```

In a confidential swap the base field `amount_b` holds `u64::MAX` and is never read by the new program. Any code that does read it (the mainnet binary after a rollback, an old client or indexer that skips the size check) sees an amount that funding cannot reach; only the mint authority could mint that much into escrow B (section 11).

The escrow ElGamal key is not stored in `SwapDvp`; it is read from the escrow ATA's `ConfidentialTransferAccount`.

Program-side loading: the baseline `SwapDvp::try_from_bytes` (`len >= 458`) is replaced by two loaders:

- `SwapDvp::load`: requires `len == 458`; a 586-byte account fails with `SwapModeMismatch`. Used by ReclaimDvp, SettleDvp, CancelDvp and RejectDvp.
- `ConfidentialSwapDvp::load`: requires `len == 586`; a 458-byte account fails with `SwapModeMismatch`. Returns `base: SwapDvp` plus the two ciphertext fields. Used by the confidential instructions.

Any other length fails with `InvalidAccountData` (table above).

`SwapDvp::LEN` stays 458; add `ConfidentialSwapDvp::LEN = 586`.

### 5.2 Other accounts

| Account                                                         | Owner                    | Public mode    | Confidential mode                                                                                                                      | Existing mainnet accounts                                                        |
| --------------------------------------------------------------- | ------------------------ | -------------- | -------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| `SwapDvp` PDA                                                   | DvP                      | 458, unchanged | 586 (+128 bytes, ≈ +0.0009 SOL rent, returned at close)                                                                                | Keep working, no migration                                                       |
| Nonce tombstone PDA                                             | DvP                      | 0, unchanged   | 0, unchanged                                                                                                                           | Unchanged. Recover and Apply after close rely on it                              |
| Escrow ATA leg A                                                | Token / Token-2022       | Unchanged      | Unchanged                                                                                                                              | Unchanged                                                                        |
| Escrow ATA leg B                                                | Token-2022               | Unchanged      | Base + `ConfidentialTransferAccount` (295 bytes + 4 TLV header), added by `Reallocate` at Create, payer funds; rent back to the closer | Unchanged. The extension is never added to the escrow of an existing public swap |
| Proof context-state accounts                                    | ZK ElGamal Proof program | n/a            | Transient: created by the caller, closed inside the consuming instruction                                                              | n/a                                                                              |
| SPL Record staging accounts (legacy / v0 path only, section 12) | SPL Record               | n/a            | Transient, created and closed by the client in preparatory transactions                                                                | n/a                                                                              |

Mode of a closed swap (Recover, Apply): a leg B escrow that is a Token-2022 account carrying `ConfidentialTransferAccount` is confidential. Only the PDA can add that extension (Reallocate and ConfigureAccount need the owner's signature), and the ATA program never adds it, so an escrow recreated by a third party after close is always public and goes through `RecoverDvp`, as today.

## 6. Proof contexts

Context-state account: owner = ZK ElGamal Proof program (`ZkE1Gama1Proof11111111111111111111111111111`), data = header (authority 32 ‖ proof_type 1) ‖ context.

| Proof                                    | proof_type | Context bytes | Layout                                                                      |
| ---------------------------------------- | ---------- | ------------- | --------------------------------------------------------------------------- |
| ZeroCiphertext                           | 1          | 96            | pubkey 32 ‖ ciphertext 64                                                   |
| CiphertextCiphertextEquality             | 2          | 192           | first_pubkey 32 ‖ second_pubkey 32 ‖ first_ct 64 ‖ second_ct 64             |
| CiphertextCommitmentEquality             | 3          | 128           | body not read by DvP                                                        |
| BatchedRangeProofU128                    | 7          | 264           | body not read by DvP                                                        |
| BatchedGroupedCiphertext3HandlesValidity | 12         | 352           | pk_source 32 ‖ pk_dest 32 ‖ pk_auditor 32 ‖ grouped_lo 128 ‖ grouped_hi 128 |

Grouped ciphertext = commitment 32 ‖ handle_source 32 ‖ handle_dest 32 ‖ handle_auditor 32; its first 64 bytes are the ciphertext under the source key (what Token-2022 extracts as `try_extract_ciphertext(0)`).

**Checks on every context account** (program, before any CPI). Token-2022 checks owner, exact length and proof type, but not the authority; DvP checks all four itself.

1. Owner is the ZK ElGamal Proof program.
2. `data_len == 33 + context_bytes` exactly.
3. `proof_type` equals the expected one.
4. `context_state_authority` equals the instruction's signer (who receives the rent).

**Amount binding (Settle only).** With `validity` = the payment transfer's validity context and `pk_escrow` = the ElGamal key read from the escrow ATA:

1. `validity.pk_source == pk_escrow`.
2. For `eq_lo`: `first_pubkey == second_pubkey == pk_escrow`; `first_ct == validity.grouped_lo[0..64]`; `second_ct == ConfidentialSwapDvp.amount_b_ciphertext_lo`.
3. Same for `eq_hi` with `grouped_hi` and `amount_b_ciphertext_hi`.
4. The same `validity` account (by position) is the one passed into the CT `Transfer` CPI.

Order is normative: first = transfer ciphertext (prover knows the escrow secret), second = stored ciphertext (prover knows the seed-derived opening). `transfer_split_proof_data` (in `spl-token-confidential-transfer-proof-generation`) does not expose the transfer openings, so the reverse order cannot be built.

Why it holds: the range proof inside the transfer forces `lo < 2^16`, `hi < 2^32`, so the decomposition is unique; equal `lo` and `hi` under the same key mean equal amounts.

**Zero check (Settle and `Full` refunds).** After the CT transfers out of escrow B, with `zero` = the ZeroCiphertext context: `zero.pubkey == pk_escrow` and `zero.ciphertext` equals the escrow's `available_balance` as it stands after the transfers, else `EscrowBalanceNotZero`. The program checks this itself instead of relying on Token-2022 `EmptyAccount`, which also requires both pending balances to be zero: anyone can credit the escrow's pending balance, and that must not block Settle or a refund.

**Resetting escrow B.** If `pending_balance_lo` and `pending_balance_hi` are all-zero bytes, the program then calls `EmptyAccount` with the same `zero` context, which resets available to all-zero bytes and is required before `CloseAccount`. Otherwise it skips `EmptyAccount` and escrow B stays open for Apply + RecoverConfidentialDvp.

**Closing.** Every context account passed to an instruction is closed inside it with a `CloseContextState` CPI, the outer signer passed through as authority, rent to the signer.

## 7. Instructions

### 7.1 Instruction set

| Disc. | Instruction            | Mode                        |
| ----- | ---------------------- | --------------------------- |
| 0     | CreateDvp              | public, source baseline     |
| 1     | ReclaimDvp             | public, source baseline     |
| 2     | SettleDvp              | public, source baseline     |
| 3     | CancelDvp              | public, source baseline     |
| 4     | RejectDvp              | public, source baseline     |
| 5     | RecoverDvp             | public, source baseline     |
| 6     | CreateConfidentialDvp  | confidential                |
| 7     | ReclaimConfidentialDvp | confidential                |
| 8     | SettleConfidentialDvp  | confidential                |
| 9     | CancelConfidentialDvp  | confidential                |
| 10    | RejectConfidentialDvp  | confidential                |
| 11    | RecoverConfidentialDvp | confidential                |
| 12    | ApplyConfidentialDvp   | confidential, new operation |

Confidential discriminators mirror the public ones at +6; Apply has no public counterpart.

The public instructions keep their wire format and logic; their only edits are listed in 7.9. Logic shared by both families (context checks, CT CPI builders, leg A transfers, readiness checks) lives in helpers.

### 7.2 Conventions for the confidential instructions

- **`CtTransferData`** (`CtTransferData::LEN = 164` bytes), used wherever the program issues a CT `Transfer`: `new_source_decryptable_available_balance [36]`, `auditor_ciphertext_lo [64]`, `auditor_ciphertext_hi [64]`. Forwarded verbatim into Token-2022 `Transfer` data with all proof offsets = 0.
- **CT transfer contexts**, in this order: equality (type 3), validity (type 12), range (type 7).
- **Optional accounts** use the program-id placeholder: an absent optional account is passed as the DvP program id (Metaplex/Codama `optionalAccountStrategy: programId`). Each group of optional accounts goes with an `Option<...>` or a `LegBRefund` mode in the data, which says whether real accounts or placeholders are passed; a mismatch → `InvalidInstructionData`. The account list and its length are therefore fixed per instruction.
- **Transfer-hook extras** follow the fixed list as remaining accounts, exactly as in the public instructions (`leg_a_extras_count` where there are two legs).
- **Leg B "funded"** means the escrow's `available_balance` is not all-zero bytes. After a refund that could not reset escrow B, available holds an encryption of 0; the next refund passes `Full` with a transfer of 0. Pending is never applied by terminal instructions.
- Byte sizes: AE ciphertext 36, ElGamal ciphertext 64.

The confidential instructions cover both legs of a confidential swap: leg A goes through the same `TransferChecked` helpers as the public instructions.

**Recipient readiness** (a leg B recipient ATA, before a CT transfer to it):

1. Has `ConfidentialTransferAccount`, else `RecipientNotConfidential`.
2. `approved`, else `RecipientNotApproved`.
3. `allow_confidential_credits`, else `RecipientConfidentialCreditsDisabled`.
4. `pending_balance_credit_counter < maximum`, else `RecipientPendingCounterFull`.

**Leg B refund** (`leg_b_refund: LegBRefund`, used by Reclaim, Cancel, Reject and Recover):

```rust
enum LegBRefund {
    None,                    // tag 0
    Full(CtTransferData),    // tag 1
    Partial(CtTransferData), // tag 2
}
```

Tags 0 and 1 encode like `Option<CtTransferData>`. A CT transfer carries at most 2^48 − 1 (section 2), while escrow B can hold more after several credits; `Partial` refunds it in steps.

| Mode      | Transfer contexts | Zero context | Effect                                                                    |
| --------- | ----------------- | ------------ | ------------------------------------------------------------------------- |
| `None`    | placeholders      | placeholder  | no CT transfer; public balance withdrawal only in Reclaim/Recover (below) |
| `Full`    | real              | real         | the full available balance; zero check and reset of escrow B              |
| `Partial` | real              | placeholder  | part of the available balance; escrow B stays open                        |

`Full` and `Partial`:

1. Context checks (section 6), context authority = the signer, before the instruction's first CPI.
2. Recipient readiness of `user_b`'s canonical ATA for `mint_b`.
3. CPI CT `Transfer` to that ATA, PDA signs, leg B hook extras appended: the full available balance (`Full`) or the amount the proofs were built for (`Partial`).
4. `Full` only: zero check and reset of escrow B (section 6).
5. `CloseContextState` for every context passed, rent to the signer.

`None` in the leg B branch: if available is not all-zero bytes, the instruction fails with `LegBRefundRequired`. Reclaim by `user_a` has no leg B branch.

**Public balance of escrow B.** `DisableNonConfidentialCredits` blocks public transfers into escrow B, but not `MintTo`: the mint authority can still mint public tokens into it. No instruction fails on such a balance; escrow B is closed only when its public amount and all CT balances are zero. `user_b` withdraws the public balance with Reclaim (swap open) or Recover (swap closed) and `leg_b_refund = None`: `TransferChecked` of the full public amount to `user_b`'s canonical ATA, PDA signs. The recipient must accept non-confidential credits; a hook on `mint_b` sees the real amount.

**Memo.** If a recipient ATA requires memos (`RequiredMemoTransfers`), the CT transfer CPI is preceded by a memo CPI, as `transfer_checked_cpi` does in the public instructions.

### 7.3 CreateConfidentialDvp

Discriminator 6. Creates a confidential swap and configures escrow B for CT.

Data:

| Field                                                                                                           | Type       | Notes                                  |
| --------------------------------------------------------------------------------------------------------------- | ---------- | -------------------------------------- |
| `amount_a`                                                                                                      | `u64`      | as CreateDvp                           |
| `expiry_timestamp`                                                                                              | `i64`      | as CreateDvp                           |
| `nonce`                                                                                                         | `u64`      | as CreateDvp                           |
| `amount_b_ciphertext_lo`                                                                                        | `[u8; 64]` | Enc(lo), section 4.3                   |
| `amount_b_ciphertext_hi`                                                                                        | `[u8; 64]` | Enc(hi)                                |
| `decryptable_zero_balance`                                                                                      | `[u8; 36]` | `escrow_ae` encryption of 0            |
| `pubkey_validity_proof_offset`                                                                                  | `i8`       | non-zero, relative to this instruction |
| `ref_string`, `user_a_settlement_destination`, `user_b_settlement_destination`, `earliest_settlement_timestamp` | options    | as CreateDvp                           |

There is no `amount_b` field.

Accounts:

| #    | Account               | Flags        |
| ---- | --------------------- | ------------ |
| 0-13 | CreateDvp accounts    | as CreateDvp |
| 14   | `instructions_sysvar` |              |

Steps, after all CreateDvp checks (`ZeroAmount` applies to `amount_a` only):

1. `mint_b` is owned by Token-2022 and has `ConfidentialTransferMint`, else `MintNotConfidential`. `TransferHook` is allowed (section 8); confidential transfer fees are rejected, as TransferFee is today.
2. Create the escrow B ATA and run `verify_escrow_not_preloaded` before `Reallocate`, since it compares lamports with the rent of the current size.
3. Escrow B's public `amount == 0`, else `EscrowPublicBalanceNotEmpty`. Its address is predictable, so it could hold someone else's public tokens before Create. Those are recovered through a public swap on the same seeds (`CreateDvp`, then Reclaim); the confidential swap uses another nonce.
4. PDA signs: `Reallocate` (adds `ConfidentialTransferAccount`, payer funds) → `ConfigureAccount` (`decryptable_zero_balance`, `maximum_pending_balance_credit_counter = 65536`, PubkeyValidity read through the instructions sysvar) → `DisableNonConfidentialCredits`.
5. Allocate 586 bytes; store the base with `amount_b = u64::MAX`, and the ciphertexts as the tail. They are stored unchecked: the bounds on `amount_b` are a client check (4.4). Confidential Create therefore cannot reject an encryption of zero; only the funder's check 4 in 4.4 catches it.

`ConfigureAccount` sets `approved` from the mint's `auto_approve_new_accounts`. On a manual-approval mint escrow B stays unapproved: it can be neither funded nor used as a transfer source until the mint's CT authority calls `ApproveAccount` on it. Clients check `approved` before funding.

The client puts `VerifyPubkeyValidity(escrow pubkey)` (ZK program, inline) immediately before this instruction. Verified on a prototype: offset `-1` resolves correctly under CPI.

### 7.4 ReclaimConfidentialDvp

Discriminator 7. One party takes its own leg back; the swap stays open.

Data:

| Field          | Type         | Notes                             |
| -------------- | ------------ | --------------------------------- |
| `leg_b_refund` | `LegBRefund` | `None`, `Full` or `Partial` (7.2) |

Accounts:

| #   | Account                                                       | Flags                                                          |
| --- | ------------------------------------------------------------- | -------------------------------------------------------------- |
| 0-6 | ReclaimDvp accounts                                           | as ReclaimDvp; 0 (signer) also writable, receives context rent |
| 7   | `zk_elgamal_proof_program`                                    |                                                                |
| 8   | equality context (`CiphertextCommitmentEquality`)             | writable, optional                                             |
| 9   | validity context (`BatchedGroupedCiphertext3HandlesValidity`) | writable, optional                                             |
| 10  | range context (`BatchedRangeProofU128`)                       | writable, optional                                             |
| 11  | zero context (`ZeroCiphertext`)                               | writable, optional                                             |

Hook extras follow.

Steps:

- Signer `user_a`: as ReclaimDvp for leg A; `leg_b_refund = None`.
- Signer `user_b`: leg B refund (`Full` or `Partial`), or the public balance withdrawal with `None` (7.2). Escrow B stays open, so it can be funded again.

### 7.5 SettleConfidentialDvp

Discriminator 8. Settles both legs atomically; the settlement authority signs.

Data:

| Field                | Type                     | Notes                                               |
| -------------------- | ------------------------ | --------------------------------------------------- |
| `leg_a_extras_count` | `u8`                     | hook extras for leg A, as in the public instruction |
| `payment`            | `CtTransferData`         | payment transfer to `user_a_destination_ata_b`      |
| `surplus_b`          | `Option<CtTransferData>` | surplus refund to `user_b_ata_b`                    |

Accounts:

| #    | Account                                                               | Flags                                |
| ---- | --------------------------------------------------------------------- | ------------------------------------ |
| 0-12 | SettleDvp accounts                                                    | as SettleDvp                         |
| 13   | `zk_elgamal_proof_program`                                            |                                      |
| 14   | payment equality context (`CiphertextCommitmentEquality`)             | writable                             |
| 15   | payment validity context (`BatchedGroupedCiphertext3HandlesValidity`) | writable                             |
| 16   | payment range context (`BatchedRangeProofU128`)                       | writable                             |
| 17   | `eq_lo` context (`CiphertextCiphertextEquality`)                      | writable                             |
| 18   | `eq_hi` context (`CiphertextCiphertextEquality`)                      | writable                             |
| 19   | zero context (`ZeroCiphertext`)                                       | writable                             |
| 20   | surplus equality context (`CiphertextCommitmentEquality`)             | writable, optional, with `surplus_b` |
| 21   | surplus validity context (`BatchedGroupedCiphertext3HandlesValidity`) | writable, optional, with `surplus_b` |
| 22   | surplus range context (`BatchedRangeProofU128`)                       | writable, optional, with `surplus_b` |

Hook extras follow, split by `leg_a_extras_count`.

Steps:

1. All SettleDvp checks. `LegNotFunded` applies to leg A only; for leg B the range proof of the transfer enforces sufficiency.
2. Recipient readiness of `user_a_destination_ata_b`, and of `user_b_ata_b` if `surplus_b`.
3. Context checks (section 6) on all 6 or 9 contexts; context authority = `settlement_authority`.
4. Amount binding (section 6), else `ConfidentialAmountBMismatch`.
5. CPI CT `Transfer` escrow B → `user_a_destination_ata_b` with the payment contexts and data, PDA signs, leg B hook extras appended.
6. `TransferChecked` of leg A and of its surplus, as in SettleDvp.
7. If `surplus_b`: CPI CT `Transfer` escrow B → `user_b_ata_b` with the surplus contexts and data.
8. Zero check and reset of escrow B (section 6). This also checks `surplus_b`: a surplus with `None` leaves the escrow non-empty and fails; `Some` without a surplus is a harmless zero transfer.
9. `CloseContextState` for every context, rent to `settlement_authority`.
10. Close escrow A and `SwapDvp` as in SettleDvp. Close escrow B if step 8 reset it and its public amount is zero; otherwise leave it open for Apply + Recover.

If escrow B's available exceeds `amount_b + 2^48 − 1`, the surplus does not fit one transfer. `user_b` first returns part of it with `ReclaimConfidentialDvp` (`Partial`), then the settlement authority builds fresh Settle proofs. This step needs `user_b`'s signature.

Only the final transaction needs the `settlement_authority` signature. The preparatory transactions (proofs verified into context accounts) need the seed but not the signing key: the ZK program records the context authority as a pubkey without its signature. A proof service can prepare them and hand the context addresses to the signer.

### 7.6 CancelConfidentialDvp, RejectConfidentialDvp

Discriminators 9 (Cancel, settlement authority) and 10 (Reject, `user_a` or `user_b`). Unwind the swap and refund both legs.

Data:

| Field                | Type         | Notes                                               |
| -------------------- | ------------ | --------------------------------------------------- |
| `leg_a_extras_count` | `u8`         | hook extras for leg A, as in the public instruction |
| `leg_b_refund`       | `LegBRefund` | `None`, `Full` or `Partial` (7.2)                   |

Accounts:

| #    | Account                                                       | Flags                    |
| ---- | ------------------------------------------------------------- | ------------------------ |
| 0-10 | CancelDvp / RejectDvp accounts                                | as CancelDvp / RejectDvp |
| 11   | `zk_elgamal_proof_program`                                    |                          |
| 12   | equality context (`CiphertextCommitmentEquality`)             | writable, optional       |
| 13   | validity context (`BatchedGroupedCiphertext3HandlesValidity`) | writable, optional       |
| 14   | range context (`BatchedRangeProofU128`)                       | writable, optional       |
| 15   | zero context (`ZeroCiphertext`)                               | writable, optional       |

Hook extras follow, split by `leg_a_extras_count`.

Steps:

1. Context checks of the leg B refund contexts (section 6), before any transfer.
2. Leg A refunded as in CancelDvp / RejectDvp.
3. Leg B refund if funded (7.2); context authority and rent = the signer. After `Partial`, `user_b` recovers the rest with RecoverConfidentialDvp.
4. Close `SwapDvp` and escrow A as today. Close escrow B if its public amount and all CT balances are zero; otherwise leave it open.

### 7.7 RecoverConfidentialDvp

Discriminator 11. After close, `user_b` drains an escrow B that stayed open: a credit arrived in pending (7.5 step 10), a `Partial` refund left a remainder, or a public balance arrived (7.2). Leg A of a closed confidential swap is recovered with the public `RecoverDvp`.

Data:

| Field                                                          | Type           | Notes                               |
| -------------------------------------------------------------- | -------------- | ----------------------------------- |
| `settlement_authority`, `user_a`, `user_b`, `mint_a`, `mint_b` | `Address` each | seed inputs, re-derive the swap PDA |
| `nonce`                                                        | `u64`          | seed input                          |
| `leg_b_refund`                                                 | `LegBRefund`   | `None`, `Full` or `Partial` (7.2)   |

Accounts:

| #   | Account                                                       | Flags              |
| --- | ------------------------------------------------------------- | ------------------ |
| 0-7 | RecoverDvp accounts                                           | as RecoverDvp      |
| 8   | `zk_elgamal_proof_program`                                    |                    |
| 9   | equality context (`CiphertextCommitmentEquality`)             | writable, optional |
| 10  | validity context (`BatchedGroupedCiphertext3HandlesValidity`) | writable, optional |
| 11  | range context (`BatchedRangeProofU128`)                       | writable, optional |
| 12  | zero context (`ZeroCiphertext`)                               | writable, optional |

Hook extras follow.

Steps:

1. RecoverDvp checks: PDA re-derived, swap closed, tombstone exists; signer is `user_b` of the seed inputs.
2. Escrow B has `ConfidentialTransferAccount`, else `EscrowNotConfidential`.
3. Leg B refund (`Full` or `Partial`) if available is not all-zero bytes; otherwise `None` withdraws the public balance (7.2).
4. Close escrow B if its public amount and all CT balances are zero; otherwise leave it open for another Apply + Recover.

### 7.8 ApplyConfidentialDvp

Discriminator 12. Moves escrow B's pending balance into available, while the swap is open or after close. No public counterpart.

Data:

| Field                                                          | Type           | Notes                                   |
| -------------------------------------------------------------- | -------------- | --------------------------------------- |
| `expected_pending_balance_credit_counter`                      | `u64`          | as Token-2022 `ApplyPendingBalance`     |
| `new_decryptable_available_balance`                            | `[u8; 36]`     | new available balance under `escrow_ae` |
| `settlement_authority`, `user_a`, `user_b`, `mint_a`, `mint_b` | `Address` each | seed inputs, re-derive the swap PDA     |
| `nonce`                                                        | `u64`          | seed input                              |

Accounts:

| #   | Account                      | Flags    |
| --- | ---------------------------- | -------- |
| 0   | `signer`                     | signer   |
| 1   | `swap_dvp`                   |          |
| 2   | `nonce_tombstone`            |          |
| 3   | `dvp_ata_b` (escrow B)       | writable |
| 4   | `token_program` (Token-2022) |          |

Steps:

1. Re-derive the `SwapDvp` PDA from the seed inputs; `swap_dvp` must be at that address.
2. Swap open (program-owned): `ConfidentialSwapDvp::load`; signer is `user_a`, `user_b` or `settlement_authority` of the stored state. Swap closed: the tombstone must exist, as in RecoverDvp; signer is one of the seed-input parties.
3. `dvp_ata_b` is the canonical ATA of the PDA for the seed input `mint_b` and has `ConfidentialTransferAccount`, else `EscrowNotConfidential`.
4. CPI `ApplyPendingBalance` with the given data, PDA signs. The program does not read or check the decryptable balance.

### 7.9 Changes to the mainnet instructions

Only these:

- Entrypoint: discriminators 6-12 dispatch to the new handlers.
- CreateDvp: none. It always creates a 458-byte public swap.
- ReclaimDvp, SettleDvp, CancelDvp, RejectDvp: `SwapDvp::try_from_bytes` → `SwapDvp::load`. A confidential swap fails with `SwapModeMismatch`.
- RecoverDvp: if the escrow carries `ConfidentialTransferAccount`, fail with `SwapModeMismatch` (use RecoverConfidentialDvp). The check makes an otherwise late failure (close on a non-empty CT balance) explicit and early.

## 8. Transfer hooks on `mint_b`

What the program does:

- A CT `Transfer` on a hooked mint makes Token-2022 invoke the hook with `amount = u64::MAX` (the real amount is hidden), after setting the `transferring` flag on source and destination. Accounts after the CT transfer's own list are forwarded to the hook.
- The confidential instructions append leg B hook extras to every CT `Transfer` CPI they issue (Settle payment, Settle surplus, refunds), exactly as the public instructions do for `TransferChecked`. `MAX_HOOK_REMAINING_ACCOUNTS = 32` per leg, unchanged.
- Apply, ConfigureAccount, EmptyAccount and context closing do not invoke the hook.

What the client does:

- Resolves the hook's `ExtraAccountMetaList` with `amount = u64::MAX` and the CT transfer's source/destination/authority (authority = the wallet for funding, the `SwapDvp` PDA for escrow withdrawals).
- Builds Settle with hook extras on either leg only in the v1 transaction format. This is the supported scope: the size of a hooked Settle in v0 depends on the hook's extra accounts and has not been measured (section 12).
- A hook validation account (`ExtraAccountMetaList`) is optional. Both client resolvers return the hook program as readonly and non-signer when it is absent. If present, the list is resolved normally; malformed data and fetch errors remain errors.
- The Settle surplus transfer reuses leg B's extras with a different destination, the same limitation the public mode documents today ("TransferHook + over-deposit").

Hook programs that read or limit the amount see `u64::MAX` and may reject. The buyer's own funding transfer into escrow also triggers the hook. Successful funding does not guarantee that subsequent transfers remain possible: the hook authority can change the hook after funding, or the hook can change its behavior. If it rejects the sentinel, Settle and all CT refund paths fail atomically. Returning leg B then depends on the hook accepting the transfer again; DvP cannot bypass it. Leg A remains independently reclaimable if its own mint and destination permit the transfer.

## 9. Errors

Confidential error codes are 24-36, in the table order below. Codes 0-23 retain the source baseline ABI at `df9919e`; compare against the deployed mainnet release before upgrading.

| Error                                  | When                                                                                                          |
| -------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| `SwapModeMismatch`                     | Public instruction on a confidential swap or CT escrow, or a confidential instruction on an open public swap  |
| `MintNotConfidential`                  | CreateConfidentialDvp / SettleConfidentialDvp: `mint_b` lacks `ConfidentialTransferMint`                      |
| `EscrowPublicBalanceNotEmpty`          | CreateConfidentialDvp: leg B escrow already holds a public balance                                            |
| `EscrowNotConfidential`                | Apply, RecoverConfidentialDvp: escrow lacks the CT extension                                                  |
| `InvalidProofContext`                  | Context owner, length or type wrong                                                                           |
| `ProofContextAuthorityMismatch`        | Context authority is not the signer                                                                           |
| `ConfidentialAmountBMismatch`          | SettleConfidentialDvp: amount binding fails                                                                   |
| `RecipientNotConfidential`             | Recipient ATA lacks the CT extension                                                                          |
| `RecipientNotApproved`                 | Recipient ATA not approved                                                                                    |
| `RecipientConfidentialCreditsDisabled` | Recipient does not accept confidential credits                                                                |
| `RecipientPendingCounterFull`          | Recipient pending counter at maximum                                                                          |
| `EscrowBalanceNotZero`                 | Settle and `Full` refunds: the zero context does not match the escrow's available balance after the transfers |
| `LegBRefundRequired`                   | Leg B branch of Reclaim, Cancel, Reject, Recover: `leg_b_refund = None` while available is not all-zero bytes |

## 10. Security invariants

1. Within DvP, only the `SwapDvp` PDA can move funds out of either escrow, and only inside program instructions. Mint-level powers (permanent delegate, freeze) stay with the mint's authorities, as in the public mode.
2. A confidential Settle transfers to the seller exactly the amount encrypted at Create: binding per section 6 plus Token-2022's own proof checks.
3. After a confidential Settle or a `Full` refund the escrow's available balance is proven zero by a ZeroCiphertext context the program checks itself; no surplus is left behind. A `Partial` refund moves funds only to `user_b`'s canonical ATA.
4. Every context account is owned by the ZK program, of exact length and type, its authority is the signer, and closed in the same instruction.
5. No terminal instruction applies pending; a credit to the escrow's pending balance, from anyone, cannot invalidate prepared proofs or block Settle and refunds. Only parties can Apply.
6. Each party can authorize refund of its leg without the other parties, as in the public mode, given the seed. Execution still depends on the mint's restrictions, including its hook and freeze state, and a ready destination account.
7. Public and confidential instructions never operate on the other mode's swap (5.1); the mode comes from on-chain state.
8. Public instructions preserve the source baseline ABI. Execution against historical deployed accounts remains a release validation requirement (section 11).
9. Rollback must not move CT-held leg B funds. The expected baseline behavior in section 11 still needs a checked-in binary rollback suite.

Known limitations, not prevented by the program:

- `Partial` proves a valid transfer, not a positive amount or exhaustion of available funds. In particular, valid `Partial(0)` proofs can close the swap through Cancel or Reject while leaving all available B in escrow. Only `user_b` can recover the remainder, bearing the cost of fresh proofs and Recover transactions (7.7). This cannot redirect B to another recipient.
- A hook authority or changed hook behavior can block CT transfers after successful funding, including Reclaim and Recover of leg B (section 8). Atomic rollback protects balances but does not guarantee eventual recovery. Preparatory proof contexts from earlier transactions remain open after a failed final transaction and require separate cleanup by their authority.
- A party can invalidate prepared Settle proofs by calling Apply after a pending credit; the proofs are rebuilt. The same party could abort the swap with Reject anyway.
- Metadata stays public: which instruction was used, account sizes and counts, whether there was a surplus, timing.
- The mint auditor, if configured, sees all amounts. An auditor key substituted or added through a forged mint snapshot would also receive a decryption handle for every proof built from it, for the same reason as the recipient keys below. Both clients bind the mint snapshot to `mint_b` and accept the expected auditor as optional input, refusing to build any proof on a mismatch; without it the mint snapshot is trusted.
- Confidentiality of the Settle and refund amounts assumes authentic recipient ElGamal keys. This follows from Token-2022 confidential transfers, not from DvP: every sender reads the recipient's key from account state served by an RPC, and neither Token-2022 nor the ZK ElGamal proof program binds a proof to the intended recipient before the proof is published. A failed final transaction still leaves its preparation proofs on chain, so DvP cannot close this on chain. Recipient keys belong to the recipients' own token accounts and cannot be derived from the shared seed. Both clients bind each recipient snapshot to the instruction's destination, Token-2022 and `mint_b`, and accept the expected recipient keys as optional input, refusing to build any proof on a mismatch. The party building Settle or refund proofs must either supply expected keys, for example from the trade agreement, or read account state from an RPC it trusts.
- `user_b` can block Cancel and Reject indefinitely by making its own `mint_b` ATA not ready (for example, disabling confidential credits). `user_a` still reclaims leg A, but the swap cannot close.
- Any party can write a wrong `new_decryptable_available_balance` through Apply. The others can rebuild the escrow balance from complete transaction history and the proof data used by those transactions; direct ElGamal decoding only works up to 32 bits. Both clients replay inline proofs, SPL Record writes and ConfidentialMint/Burn. Writes to a pre-existing Record with no initialization in the supplied history are ignored unless a relevant proof needs its bytes. Such Records and proofs stored by other programs require historical account bytes through Rust `BalanceHistory::push_with_proof_accounts` or the resolver argument to TypeScript `BalanceHistory.push`; ordinary RPC transaction metadata alone does not contain them. Missing data can prevent recovery of an unknown large balance. All reconstructed available and pending limbs and the pending counter must match the account snapshot.
- RPC history affects recovery availability, not balance integrity. Neither client verifies transaction signatures, and the parts of a `getTransaction` response that replay depends on (error status, inner instructions, loaded addresses) are not covered by any signature. RPC metadata lists inner-instruction groups only for instructions that made CPIs, so a client cannot tell an omitted group from an instruction without CPIs and treats a missing group as no CPIs. Replay also follows the reported order of inner instructions; an honest node reports CPIs in execution order, and DvP closes its proof contexts only after the transfer that uses them. A dishonest or faulty RPC can forge, withhold or omit history, including whole inner-instruction groups, or reorder it, and so block recovery, but it cannot make a client accept a false balance, because every reconstructed limb and the pending counter must match the snapshot. Take history for the fallback from a trusted or cross-checked source.
- The mint authority can mint public tokens into escrow B (`MintTo` ignores `DisableNonConfidentialCredits`). They go to `user_b` (7.2), and escrow B stays open until then.
- A Settle surplus above 2^48 − 1 needs a `Partial` Reclaim by `user_b` first (7.5).

## 11. Release compatibility

- **Program id** `dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq` is the existing mainnet deployment address. The confidential extension upgrades this program in place and retains its id. "Unchanged" currently means unchanged against source baseline `df9919e`; pin the deployed binary and released client versions before claiming deployment compatibility. The earlier Cantina audit linked in the README does not establish coverage of this extension.
- **Instructions 0-5**: wire format frozen (data, account count and order, signer/writable flags). The checked-in `test_public_abi.rs` compares the current IDL structurally with `tests/fixtures/public-abi.json` from `df9919e`. Executing pinned old-client bytes on the new binary remains a release check (section 13).
- **Accounts**: no migration (5.2).
- **Clients**: the source-baseline `verify` helpers pin `SwapDvp` to 458 bytes and fail closed on a confidential swap; the raw generated decoders read the base and ignore the tail. Upgraded clients accept 458 or 586: the base with the generated decoder on `data[..458]`, the tail by hand; the IDL describes the base only. Indexers that filter `getProgramAccounts` by `dataSize: 458` miss confidential swaps; the client adds a helper that queries both sizes.
- **Expected rollback behavior**, based on the source baseline and still requiring binary validation: no CT-held leg B funds move. Its Settle reads `amount_b = u64::MAX` and fails `LegNotFunded`, unless the mint authority has minted that much publicly into escrow B. Its Cancel, Reject and RecoverDvp fail at `CloseAccount` while escrow B holds a CT balance (`user_a` can still Reclaim leg A), and close the swap when escrow B is empty. Its Reclaim by `user_b` moves only a public balance of escrow B, as in the public mode.

Release checklist (not evidence of completed deployment tests):

1. Snapshot program accounts; every non-empty one is 458 bytes and decodes.
2. On a mainnet fork, deploy the new binary and run instructions 1-5 on real open swaps.
3. Extend ProgramData if the new `.so` is larger; verifiable build, hash in the release notes.
4. After the upgrade: one public and one confidential swap end to end, then publish IDL and clients and announce the mode.

## 12. Transactions and client

Settle and refunds of a funded leg B take several transactions: preparatory ones verify the proofs into context accounts, and the final one runs the DvP instruction. The clients support two transaction formats:

- **v1**, up to 4096 bytes in the configured Agave 4.2 test environment: proofs go inline. Confirm v1 support on the target cluster before submitting.
- **v0 + lookup table**, up to 1232 bytes: the range proof (1000 bytes of proof data) does not fit and is staged through SPL Record (`recr1L3PCGKLbckBqMNcJhuuyU1zgo8nBhfLVsJNwr5`), which adds transactions.

Measured Rust sessions in `test_client_confidential`, with a separate fee payer and the fixture's active lookup tables. Sizes include signatures; final sizes and packing depend on the supplied LUTs:

| Flow                                                | v1                                       | v0 + LUT                       |
| --------------------------------------------------- | ---------------------------------------- | ------------------------------ |
| Create                                              | 1 tx, 972 bytes                          | 1 tx, 609 bytes                |
| Apply                                               | 1 tx, 624 bytes                          | 1 tx, 572 bytes                |
| Settle, no surplus                                  | 1 prep (4096 bytes) + final (1076 bytes) | 5 prep + final (779 bytes)     |
| Settle, with surplus                                | 2 prep + final (1336 bytes)              | 9 prep + final (1039 bytes)    |
| Settle, hooked `mint_b`, surplus and required memos | 2 prep + final (1435 bytes)              | not supported                  |
| Reclaim, Partial                                    | 1 prep + final (776 bytes)               | 4 prep + final (665 bytes)     |
| Reclaim, Full                                       | 1 prep + final (808 bytes)               | 4 prep + final (697 bytes)     |
| Cancel/Reject, funded leg B                         | 1 prep + final (909-941 bytes)           | 4 prep + final (674-706 bytes) |
| Recover, Full                                       | 1 prep + final (1009 bytes)              | 4 prep + final (867 bytes)     |
| Recover, None public withdrawal                     | 1 tx, 717 bytes                          | 1 tx, 575 bytes                |
| Buyer's funding (a plain CT transfer)               | 1 prep + transfer (702 bytes)            | 4 prep + transfer (712 bytes)  |

- Final Settle used roughly 70-85k CU without surplus, 105-120k with surplus, and 152k with the test hook and required memos. Preparation stayed below 250k CU per transaction.
- All six Settle contexts fit in one v1 preparation at exactly 4096 bytes. V0 Record writes reach 1232 bytes; the client checks every transaction against its format's limit.

Requirements:

- v1: set `compute_unit_limit` and `loaded_accounts_data_size_limit` explicitly. Agave treats missing values as 0, and Token-2022 alone loads 1.3 MB.
- v1: send transactions in base64 (base58 cannot carry a full-size v1 transaction) and read them with `maxSupportedTransactionVersion >= 1` in `getTransaction` / `getBlock`.
- v0: set compute and loaded-account limits explicitly through compute-budget instructions. Rust sessions default to 400k CU and 4 MB, with preparation packing bounded by estimated proof cost and wire size.

Rust and TypeScript client helpers:

| Helper                                                                                                                                            | What it does                                                                                                                                                                                                                                                                                                            |
| ------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rust `derive_shared_seed(swap, mac)` / TS `deriveSharedSeed(swapDvp, mac)`                                                                        | shared seed (4.1); takes an HMAC callback so the key stays in a KMS                                                                                                                                                                                                                                                     |
| Rust `EscrowKeys::from_seed`, `encrypt_amount` / TS `EscrowKeys.fromSeed`, `encryptAmount`                                                        | escrow keys and amount ciphertexts from the seed (4.2, 4.3)                                                                                                                                                                                                                                                             |
| Rust `verify::decode_swap_dvp_account` / TS `decodeSwapDvpChecked`, `verifySwapDvp`                                                               | check and decode 458 or 586 bytes into `SwapDvpAccount`; `base()` exposes common fields                                                                                                                                                                                                                                 |
| Rust `verify::verify_confidential_funding` / TS `verifyConfidentialFunding`                                                                       | pre-funding checks using shared seed and expected amount B (4.4)                                                                                                                                                                                                                                                        |
| Rust `read_escrow_balance`, `BalanceHistory` / TS `readEscrowBalance`, `BalanceHistory`                                                           | available and pending balance of escrow B; rebuilt from transaction history when the decryptable balance is wrong                                                                                                                                                                                                       |
| Rust `create_session`, `apply_session`, `settle_session`, `refund_session` / TS `createSession`, `applySession`, `settleSession`, `refundSession` | ordered preparatory and final transactions for Settle and every refund path (`Full`, `Partial`, public balance withdrawal), in v1 or v0 + LUT, with CT hook extras resolved at `u64::MAX` and public-withdrawal extras at the actual public amount; Apply is a single transaction. Sending and retries are the caller's |

In TypeScript, `encryptAmount` and the Settle builder need `PedersenOpening.fromBytes` from `@solana/zk-sdk` 0.5.4 or later (4.3).

Dependencies:

- Rust: `solana-zk-sdk` 7.0.1 (8.x once `spl-token-confidential-transfer-proof-generation` supports it), `spl-token-confidential-transfer-proof-generation` 0.6.1, `spl-record`.
- TypeScript: `@solana/zk-sdk` >= 0.5.4, `@solana-program/token-2022`, `@solana-program/zk-elgamal-proof`, `@solana-program/record` and `@solana/kit` 8.x for v1 messages.

If the initial balance read and history fallback both fail, Rust returns
`BalanceRecoveryFailed { initial, history }` with the initial error as its source.
TypeScript retains the recovery code/message and sets the initial error as
`cause`. Both clients reject a foreign escrow key before attempting recovery.

## 13. Tests

Environment:

- LiteSVM 0.16 (Agave 4.2, mainnet feature set) with the mainnet Token-2022 and SPL Record binaries loaded via `add_program`; real proofs.
- Tests use the Solana 4.x crates and Rust 1.93.1.
- LiteSVM does not enforce the 1232 / 4096-byte limits, so test helpers assert transaction size and CU of every Settle variant in both formats.

Confidential flows:

- Full cycle with and without surplus.
- Every unwind path for leg A and leg B.
- Apply and Recover after close.
- A manual-approval `mint_b`: escrow B cannot be funded until the mint's CT authority approves it, then the full cycle works.
- Two credits of 2^47, Apply, then `Partial(2^48 − 1)` and `Full(1)` in every refund path; Cancel and Reject with `Partial`, then Recover.
- Settle with a surplus of 2^48: `Partial(1)` by `user_b` first, then Settle.
- `MintTo` into escrow B after Create: Settle and refunds succeed and leave escrow B open; Recover with `None` returns the public tokens to `user_b` and closes escrow B.
- A third-party confidential credit to escrow B right before Settle and before each refund: the instruction succeeds and escrow B stays open, then Apply → Recover, with another credit right before Recover.

Negative cases:

| Case                                                                               | Expected                           |
| ---------------------------------------------------------------------------------- | ---------------------------------- |
| Amount substitution                                                                | `ConfidentialAmountBMismatch`      |
| Foreign key in an equality context                                                 | `ConfidentialAmountBMismatch`      |
| Equality bound to a different validity context                                     | `ConfidentialAmountBMismatch`      |
| Foreign context authority                                                          | `ProofContextAuthorityMismatch`    |
| Wrong context length or type                                                       | `InvalidProofContext`              |
| `Option` or `LegBRefund` mode and placeholders disagree                            | `InvalidInstructionData`           |
| `Full` that leaves available non-zero                                              | `EscrowBalanceNotZero`             |
| Zero context does not match the escrow balance                                     | `EscrowBalanceNotZero`             |
| `leg_b_refund = None` while leg B is funded                                        | `LegBRefundRequired`               |
| `mint_b` without CT                                                                | `MintNotConfidential`              |
| Recipient not ready, each check (including a manual-approval mint without approve) | the matching `Recipient*` error    |
| Public balance in escrow B before Create                                           | `EscrowPublicBalanceNotEmpty`      |
| `amount_b >= 2^48`                                                                 | rejected by the client check (4.4) |

Mode separation: instructions 1-4 on a confidential swap, RecoverDvp on a CT escrow, and 7-10 or 12 on an open public swap fail with `SwapModeMismatch`; RecoverConfidentialDvp and Apply on a public escrow of a closed swap fail with `EscrowNotConfidential`. Nothing changes in either case. RecoverDvp still recovers leg A of a closed confidential swap.

Transfer hooks:

- A hooked `mint_b` through Settle (with and without surplus) and every unwind path, in v1. Both client suites execute Settle without an optional validation account and retain malformed-data/fetch-error checks.
- After successful funding, update the mint's hook through Token-2022 to one that rejects `u64::MAX`. Settle, Cancel, Reject, Reclaim B and Recover B must reach that hook's rejection and leave swap, escrows, recipients and prepared proof contexts unchanged. This is an atomic failure, not a guarantee of later recovery. Reclaim A must still succeed when leg A itself is transferable.

Keys:

- Test vectors (4.5) in Rust and TypeScript.
- A TypeScript-derived seed verifies against a Rust-created swap: `clients/test-vectors/confidential-funding.json` holds a 586-byte swap and its escrow B dump from the Rust suite, and both clients run funding verification on it. The reverse follows from the 4.5 vectors: the same seed yields the same escrow key and ciphertext bytes, and verification compares them byte for byte.

Checked-in compatibility coverage:

- `tests/integration-tests/src/test_public_abi.rs` compares the public IDL structure with the pinned `df9919e` fixture: program id, accounts, PDAs, instructions 0-5 and errors 0-23.
- Public lifecycle tests execute against the current binary, alongside confidential mode-separation tests.
- Swap loaders and checked clients distinguish exact 458 / 586-byte layouts and reject malformed sizes. Zero-length tombstones are not swaps.

Remaining release compatibility checks:

- Build instructions 0-5 with a pinned release client, compare golden wire bytes and execute them against the new binary.
- Produce 458-byte accounts with the pinned old binary (`earliest` Some/None, with/without hooks), then exercise every public instruction on the new binary.
- Run binary rollback cases for funded and empty confidential escrows and verify that no CT-held leg B funds move (section 11).
- Run the pinned old verification helpers against confidential accounts and confirm rejection. The current repository's dual-mode readers do not substitute for this check.

These are release requirements, not claims that the corresponding binary tests
already exist in this repository.

## 14. Out of scope

Confidential leg A; the demo application; Mosaic/SDP/Explorer integrations; confidential transfer fees; buyer-side wallet support; the off-chain channel that carries the shared seed.

## 15. Open questions

1. **Seed derivation.** HMAC under an operator-held master key (4.1) is accepted for this design; the rationale is in [key-management.md](key-management.md#3-why-this-derivation).
2. **Deployment baseline.** Exact deployed release commit/tag, binary and last error code (sections 11, 9) remain to be confirmed.
3. **Re-audit.** Solana Foundation will arrange the re-audit; timing remains to be confirmed. Scope: instructions 6-12 and the migration and rollback properties of section 11.
