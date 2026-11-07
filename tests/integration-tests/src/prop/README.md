# Property-based tests

These tests drive the program on LiteSVM with generated sequences of instructions, signers, clock moves and mint configurations, and check every step against a small reference model of the DvP lifecycle.

## Properties

- `prop_lifecycle_matches_model`: random Fund / Reclaim / Settle / Cancel / Reject / clock moves / foreign signers on every leg combination (SPL Token, Token-2022, Token-2022 with the transfer-hook fixture). After each step the result and every balance match the model, token totals are conserved, and the PDA and escrows are closed after completion. Includes the L3 case from the audit, where dust in escrow A and a closed ATA block settlement and refunds until the ATA is re-created.
- `prop_settle_time_window`: `Create` rejects past or too distant expiry and `earliest > expiry`. `Settle` succeeds if and only if `earliest <= t <= expiry`, probed at both boundaries ±1 second.
- `prop_permissions`: only the authority can Settle and Cancel, only the parties can Reject, an outsider can do nothing.
- `prop_mint_extensions`: 10 mint configurations per leg. `Create` fails with `BlockedMintExtension` exactly for the blocked extensions, allowed ones settle with exact amounts, and swapping an unfunded leg's mint to a blocked one after `Create` makes `Settle` fail with `BlockedMintExtension` while `Cancel` still refunds in full.
- `prop_recover_and_nonce`: `Recover` fails with `DvpStillOpen` while the DvP is open. After Settle or Cancel, late deposits are recoverable by the right party only, a reused nonce always fails, and a second DvP between the same parties with a new nonce stays independent.

Each property counts how often it reached the interesting cases and fails if a count falls below its minimum. The minimums scale with `PROPTEST_CASES`. Runs are seeded (`PROPTEST_RNG_SEED`, default `20261014`) and repeat exactly.

## Running

After `just build`, from `tests/integration-tests/`:

```sh
PROPTEST_CASES=64 cargo test prop:: -- --test-threads=4
PROPTEST_CASES=512 cargo test prop:: -- --test-threads=4   # long run
```

## Not covered yet

A transfer hook whose behaviour depends on the recipient or the amount, WSOL legs, memo-required accounts, smart-wallet parties.
