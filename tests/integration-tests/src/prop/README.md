# Property-based tests

These tests drive the program on LiteSVM with generated sequences of instructions, signers, clock moves and mint configurations, and check every step against a small reference model of the DvP lifecycle.

## Properties

- `prop_lifecycle_matches_model`: random Fund / Reclaim / Settle / Cancel / Reject / clock moves / foreign signers on every leg combination (SPL Token, Token-2022, Token-2022 with the transfer-hook fixture). After each step the result and every balance match the model, token totals are conserved, and the PDA and escrows are closed after completion. Includes finding 3.2.3 from the Cantina audit ("Dust surplus can block settlement when refund ATAs are absent"), where dust in escrow A and a closed ATA block settlement and refunds until the ATA is re-created.
- `prop_settle_time_window`: `Create` rejects past or too distant expiry and `earliest > expiry`. `Settle` succeeds if and only if `earliest <= t <= expiry`, probed at both boundaries ±1 second.
- `prop_permissions`: only the authority can Settle and Cancel, only the parties can Reject, an outsider cannot Settle, Cancel or Reject.
- `prop_mint_extensions`: 10 mint configurations per leg. `Create` fails with `BlockedMintExtension` exactly for the blocked extensions, allowed ones settle with exact amounts, and swapping an unfunded leg's mint to a blocked one after `Create` makes `Settle` fail with `BlockedMintExtension` while `Cancel` still returns the funded leg in full.
- `prop_recover_and_nonce`: `Recover` fails with `DvpStillOpen` while the DvP is open. After Settle or Cancel, late deposits are recoverable by the right party only, a reused nonce always fails, and a second DvP between the same parties with a new nonce stays independent.

Each property counts how often it reached the interesting cases and fails if a count falls below its minimum. The minimums are sized for 64 cases and scale with `PROPTEST_CASES`. Below 64 cases the counts are still printed but not checked. Runs are seeded (`PROPTEST_RNG_SEED`, default `20261014`, printed at the start of each property). The same seed generates the same sequences, but keypairs and the starting clock are new on every run. Failures are not saved to `proptest-regressions` files, the seed is what reproduces a run. The nightly workflow uses a new seed on every scheduled run. If a nightly run fails only on coverage, the seed from the log reproduces the same counts locally, and a manual run with the default seed shows whether the minimums still hold.

A draft list of the invariants these properties check, with the code that enforces each one, is in [INVARIANTS.md](INVARIANTS.md).

## Running

After `just build`, from `tests/integration-tests/`:

```sh
PROPTEST_CASES=64 cargo test prop:: -- --test-threads=4
PROPTEST_CASES=512 cargo test prop:: -- --test-threads=4   # long run
```

## Not covered yet

See the end of [INVARIANTS.md](INVARIANTS.md).
