# Invariants (draft)

Draft for review. Each entry is something the program has to hold, the property test that checks it, and the place in `program/src` that enforces it. Line numbers are against `2ccb9c8`. The list comes from the property tests in this directory, so it only covers what they check. Unless an entry says otherwise, the test checks that a transaction succeeds or fails and that every balance matches, not the error code.

## Invariants

### DVP-INV-01 Settle needs both legs funded

Settle fails unless each escrow holds at least its agreed amount (`LegNotFunded`), and a failed Settle moves no tokens.

Tests: `prop::lifecycle::prop_lifecycle_matches_model`

Code: `program/src/processor/settle_dvp.rs:224`, `program/src/processor/settle_dvp.rs:229`

### DVP-INV-02 Counterparty gets exactly the agreed amount, surplus goes back to the depositor

On Settle each destination receives exactly `amount_a` or `amount_b`. Anything above that, including dust sent by a third party, goes back to `user_a` or `user_b` and never to the counterparty.

Tests: `prop::lifecycle::prop_lifecycle_matches_model`

Code: `program/src/processor/settle_dvp.rs:247`, `program/src/processor/settle_dvp.rs:260`, `program/src/processor/settle_dvp.rs:278`, `program/src/processor/settle_dvp.rs:294`

### DVP-INV-03 Token totals are conserved per mint

For each mint the sum over every account that can hold it stays the same after every step, accepted or rejected. This relies on the deny-list of amount-mutating extensions described in the comment above `validate_mint_extensions`. Both tests sum the balances directly, independent of the model, after every step (the recover test after every step following the close). Deposits are generated, but the DvP amounts and the starting balances are fixed.

Tests: `prop::lifecycle::prop_lifecycle_matches_model`, `prop::recover::prop_recover_and_nonce`

Code: `program/src/processor/shared/token_utils.rs:339`

### DVP-INV-04 A terminal instruction closes everything, once

After Settle, Cancel or Reject the SwapDvp PDA and both escrows are closed, and any later Reclaim, Settle, Cancel or Reject on that DvP fails.

Tests: `prop::lifecycle::prop_lifecycle_matches_model`

Code: `program/src/processor/settle_dvp.rs:311`, `program/src/processor/settle_dvp.rs:337`, `program/src/processor/shared/refund.rs:129`, `program/src/processor/shared/refund.rs:152`, `program/src/processor/settle_dvp.rs:91`, `program/src/processor/cancel_dvp.rs:64`, `program/src/processor/reject_dvp.rs:67`, `program/src/processor/reclaim_dvp.rs:72`

### DVP-INV-05 A refund into a missing ATA blocks until the ATA is recreated

While `user_a`'s mint A ATA is closed, Settle with surplus on leg A and Cancel, Reject or Reclaim with a non-empty escrow A fail, because the transfer into that ATA fails. Once anyone recreates the ATA they succeed again (Cantina L3). The recreate step is generated but has no coverage minimum of its own.

Tests: `prop::lifecycle::prop_lifecycle_matches_model`

Code: `program/src/processor/settle_dvp.rs:280`, `program/src/processor/shared/refund.rs:100`, `program/src/processor/reclaim_dvp.rs:134`

### DVP-INV-06 Settlement window is inclusive at both ends

Settle succeeds at `t == earliest` and at `t == expiry` and fails one second outside either bound.

Tests: `prop::permissions_time::prop_settle_time_window`

Code: `program/src/processor/settle_dvp.rs:143`, `program/src/processor/settle_dvp.rs:145`

### DVP-INV-07 Create bounds the expiry and the earliest settlement time

Create fails unless `now < expiry <= now + 365 days` and `earliest`, when set, is not after `expiry`.

Tests: `prop::permissions_time::prop_settle_time_window`

Code: `program/src/processor/create_dvp.rs:444`, `program/src/processor/create_dvp.rs:448`, `program/src/processor/create_dvp.rs:453`

### DVP-INV-08 Only the settlement authority settles or cancels

Settle and Cancel signed by a party or an outsider fail.

Tests: `prop::permissions_time::prop_permissions`

Code: `program/src/processor/settle_dvp.rs:99`, `program/src/processor/cancel_dvp.rs:72`

### DVP-INV-09 Only a party rejects

Reject signed by the settlement authority or an outsider fails.

Tests: `prop::permissions_time::prop_permissions`

Code: `program/src/processor/reject_dvp.rs:75`

### DVP-INV-10 Create rejects blocked mint extensions

Create fails with `BlockedMintExtension` if either mint has `TransferFee`, `InterestBearing`, `ScaledUiAmount` or `NonTransferable`, and accepts the other configurations the test generates. The test checks the error code.

Tests: `prop::extensions::prop_mint_extensions`

Code: `program/src/processor/create_dvp.rs:107`, `program/src/processor/shared/token_utils.rs:339`

### DVP-INV-11 Settle rechecks the mints, Cancel still refunds

If an unfunded leg's mint gets a blocked extension after Create, Settle fails with `BlockedMintExtension` and Cancel still refunds both parties in full. The test stands in for closing and recreating the mint by rewriting the mint account, and checks the error code.

Tests: `prop::extensions::prop_mint_extensions`

Code: `program/src/processor/settle_dvp.rs:128`, `program/src/processor/settle_dvp.rs:129`

### DVP-INV-12 Recover only works on a closed DvP

Recover on an open DvP fails with `DvpStillOpen`. The test checks the error code.

Tests: `prop::recover::prop_recover_and_nonce`

Code: `program/src/processor/recover_dvp.rs:138`

### DVP-INV-13 Recover returns a late deposit to the right party, repeatedly

After close, a deposit into a recreated escrow is drained to the signer's own ATA and the escrow is closed again, as many times as it is recreated. The other party, the authority and an outsider cannot take it.

Tests: `prop::recover::prop_recover_and_nonce`

Code: `program/src/processor/recover_dvp.rs:108`, `program/src/processor/recover_dvp.rs:112`, `program/src/processor/recover_dvp.rs:219`

### DVP-INV-14 A nonce is never reused

Create with the seeds and nonce of a closed DvP always fails, the tombstone stays, and the closed PDA never comes back. A second DvP between the same parties with a new nonce stays independent.

Tests: `prop::recover::prop_recover_and_nonce`

Code: `program/src/processor/create_dvp.rs:167`

## Not covered yet

These come in the next PR, from the review of #21.

- every instruction rejects wrong accounts (another DvP's escrows and PDA, a mint other than at Create, the wrong token program, someone else's ATA, duplicate accounts, the wrong owner) and no balance moves;
- per-mint conservation on generated amounts, including 0, 1 and near `u64::MAX`;
- per-mint conservation after rejected transactions with wrong accounts;
- each party ends with either its own deposit back or the other leg, checked from chain state without the model.
