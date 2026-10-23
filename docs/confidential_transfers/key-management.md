# DvP Confidential Transfer: Key Management

The [specification](spec.md#4-shared-seed-and-keys) defines the exact derivation.
This guide covers its rationale and how participants keep the secrets.

## 1. Secrets and owners

The seed operator (the settlement authority's operator or an appointed party)
holds a dedicated `seed_master_key`. It derives a per-swap `shared_seed` and
shares it with `user_a`, `user_b` and the settlement authority's operator.
Each derives escrow B's ElGamal and AE keys and amount openings from that seed.
Signing keys and the parties' own token-account CT keys are separate from the
DvP seed.

## 2. Recommended derivation

```text
shared_seed = HMAC-SHA256(seed_master_key, "dvp/confidential-amount-b/seed/v1" || swap_dvp)
(escrow_elgamal, escrow_ae) = derive_confidential_keys_from_ikm(shared_seed)
```

Use a dedicated, non-exportable HMAC-SHA256 key of at least 32 bytes in a
KMS/HSM. `swap_dvp` is the PDA's raw 32 bytes, known before Create; the nonce
tombstone prevents reuse within the deployment.

Rust `derive_shared_seed(swap, mac)` and TypeScript
`deriveSharedSeed(swapDvp, mac)` pass the complete message to an HMAC callback.
Implement it with the KMS's `GenerateMac` or equivalent operation so the master
key stays in the KMS. Then use `EscrowKeys::from_seed` / `EscrowKeys.fromSeed`
to derive ElGamal, AE and deterministic amount openings.

See the [Rust](rust-client.md#keys-and-funding-checks) and
[TypeScript](typescript-client.md#keys-and-verification) guides and
`clients/rust/examples/confidential/create_apply.rs`. Implementations must match
`clients/test-vectors/confidential-amount-b.json`, including the openings defined
in [spec 4.3](spec.md#43-amount-ciphertexts).

## 3. Why this derivation

HMAC gives a reproducible seed per swap without exporting the master key. The
domain tag separates and versions this use; a leaked swap seed does not reveal
the master key or other swaps' seeds. A random seed per swap is also valid, but
requires retaining recoverable copies of each seed. HMAC instead requires the
master keys and the swap-to-key/version mapping to be backed up.

A wallet signature can derive CT keys for a single-owner account, but is not
recommended for the shared escrow:

- Recovery would depend on one wallet's signing setup, while the seed still
  needs to be distributed to the other participants. Standard Ed25519 is
  [deterministic](https://www.rfc-editor.org/rfc/rfc8032.html#section-8.2), but
  the actual custody/MPC signer must support signing the derivation message
  and reproducing the same bytes. Losing access to the original signing key
  or changing those bytes breaks re-derivation; saved seeds remain usable.
- Anyone obtaining a signature over the derivation message would obtain the
  escrow secrets. A separate HMAC key keeps seed recovery independent of
  transaction signing and wallet rotation.

## 4. Storage and handling

**Seed operator:**

- Restrict and audit access to the KMS HMAC operation. Protect master keys using
  the provider's backup or replication mechanism, and back up the mapping from
  each `swap_dvp` to its immutable key ID/version. On rotation, use the new key
  for new swaps and retain old keys until their escrows B are closed.
- Deliver seeds over an authenticated, encrypted channel to the three
  participants. Keep seed storage and proof services separate from signing keys.
  The settlement authority can delegate proof preparation without handing its
  signing key to that service.

**Each participant, including the settlement authority:**

- Before funding, run `verify_confidential_terms` / `verifyConfidentialTerms`
  with the received seed's keys and the agreed terms, which also compares the
  public terms with the agreed trade ([spec 4.4](spec.md#44-verification-before-funding)).
- Store the seed encrypted at rest, indexed by `swap_dvp`, separately from
  signing keys. Exclude it from logs, analytics and error reports. Derive escrow
  keys as needed; in TypeScript call `keys.free()` to release WASM allocations.
- Never serialize, log or include escrow keys or their fields in error reports.
  In Rust the `EscrowKeys` fields are public and the SDK `ElGamalKeypair`
  serializes its secret.
- Retain the seed until **escrow B is closed**, including Apply and Recover
  after settlement or swap closure. It can then be deleted if historical
  decryption is no longer needed. Deletion cannot revoke copies held by others.

## 5. Compromise and loss

- A leaked seed reveals that swap's amount and escrow B balances; a leaked
  master key exposes every swap derived from it. Neither alone authorizes
  transfers. Rotate a compromised master key for new swaps; existing swaps
  remain exposed.
- A leaked signing key threatens the funds it controls but does not directly
  reveal the independent DvP seed or escrow keys. If the wallet's own CT keys
  derive from its signatures, those keys and the amounts they protect may also
  be exposed.
- If a seed copy is lost, recover it from another participant or re-derive it
  with the original master key. Loss of the master key does not invalidate
  saved seeds. Confidential funds become unrecoverable only when no usable
  seed or escrow-key copy and no way to reconstruct the required keys remain.
