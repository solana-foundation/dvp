<div align="center">
  <br />
  <h1>Solana DvP</h1>
  <h3>Atomic Delivery-versus-Payment for Solana</h3>
  <br />

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

</div>

A canonical, minimal on-chain program for atomic delivery-versus-payment (DvP) swaps between two parties on Solana. One transaction settles both legs simultaneously — no counterparty risk, no custodian, no intermediary.

## What it does

Two parties — **`user_a`** (seller) and **`user_b`** (buyer) — agree to exchange `amount_a` of `mint_a` (the _asset_ leg) for `amount_b` of `mint_b` (the _cash_ leg). A third party, the **`settlement_authority`**, is the only address allowed to atomically settle the trade. Either party (or, via Cancel, the authority) can abort before settlement and recover their funded leg.

The trade lives as a single `SwapDvp` PDA with two associated escrow ATAs (one per leg). Each side funds its own leg by sending tokens to the corresponding escrow ATA via a plain SPL Transfer — there's no custom funding instruction, so custodian integrations need no special program call. Settlement transfers both legs in a single transaction, refunds any over-deposit to the depositor, and closes the PDA + escrows.

## State

```rust
SwapDvp {
    bump: u8,
    user_a: Pubkey,                              // seller
    user_b: Pubkey,                              // buyer
    mint_a: Pubkey,                              // asset
    mint_b: Pubkey,                              // cash
    settlement_authority: Pubkey,
    amount_a: u64,
    amount_b: u64,
    expiry_timestamp: i64,                       // settlement rejected after this
    nonce: u64,                                  // disambiguates DvPs sharing other seeds
    ref_string: [u8; 64],                        // opaque client reference, UTF-8 zero-padded
    user_a_settlement_destination: Pubkey,       // receives the cash leg at Settle (defaults to user_a)
    user_b_settlement_destination: Pubkey,       // receives the asset leg at Settle (defaults to user_b)
    earliest_settlement_timestamp: Option<i64>,  // optional lower bound on settlement
}
```

PDA seeds: `[b"dvp", settlement_authority, user_a, user_b, mint_a, mint_b, nonce.to_le_bytes(), bump]`.

## Lifecycle

```
                      ┌─────────────────────┐
                      │       Create        │  permissionless
                      └──────────┬──────────┘
                                 │
                ┌────────────────┴────────────────┐
                ▼                                 ▼
         ┌─────────────┐                  ┌─────────────┐
         │ SPL Transfer│                  │ SPL Transfer│  user_a / user_b
         │ → escrow A  │                  │ → escrow B  │  (raw SPL — no
         └──────┬──────┘                  └──────┬──────┘   program call)
                │                                │
                └────────────────┬───────────────┘
                                 │
        ┌────────────────────────┼────────────────────────┐
        ▼                        ▼                        ▼
 ┌─────────────┐          ┌─────────────┐          ┌─────────────┐
 │  Reclaim X  │          │   Settle    │          │ Cancel /    │
 │  (re-fund   │          │ (cash→A,    │          │ Reject      │
 │   allowed)  │          │  asset→B,   │          │ (refund all,│
 │             │          │  close all) │          │  close all) │
 └─────────────┘          └─────────────┘          └─────────────┘
```

## Instructions

| #   | Name       | Signer                 | Effect                                                                                                                                                                     |
| --- | ---------- | ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0   | CreateDvp  | any                    | Allocates the SwapDvp PDA, its nonce tombstone, and both escrow ATAs. No funding.                                                                                          |
| 1   | ReclaimDvp | `user_a` or `user_b`   | Drains signer's leg back to them. DvP stays open.                                                                                                                          |
| 2   | SettleDvp  | `settlement_authority` | Transfers `amount_x` of each leg to its settlement destination (cross; defaults to the counterparty), refunds any over-deposit to the depositor, closes SwapDvp + escrows. |
| 3   | CancelDvp  | `settlement_authority` | Refunds any funded legs to depositors, closes SwapDvp + escrows.                                                                                                           |
| 4   | RejectDvp  | `user_a` or `user_b`   | Refunds any funded legs to depositors, closes SwapDvp + escrows.                                                                                                           |
| 5   | RecoverDvp | `user_a` or `user_b`   | Post-close only: drains a recreated escrow ATA back to the signer's leg and closes it. Recovers deposits that land after Settle/Cancel/Reject.                             |

### Notes

- **Funding via raw token transfer.** There is no on-program funding instruction. Each side deposits its leg by transferring tokens to the leg's escrow ATA (escrow ATAs are derivable from public state), so custodian integrations need no custom program call. Use `TransferChecked`, not an unchecked `Transfer`: Token-2022 mints with extension-aware transfer behavior reject unchecked transfers because they need the mint account. A `Pausable` mint needs the mint account supplied so Token-2022 can enforce the current pause state, and a `TransferHook` mint needs the same hook account resolution as terminal transfers (hook program, validation PDA, and the resolved `ExtraAccountMetaList` accounts). A plain unchecked transfer can fail for these mints even though `CreateDvp` accepts them.
- **Native SOL (WSOL) legs.** Wrapped-SOL legs are supported. Before reading an escrow balance, the program `SyncNative`s the account if it is native, so lamports sent to the escrow via a raw system transfer (which don't update the token `amount` on their own) are counted and refunded to the depositor rather than swept to the close recipient.
- **Non-native escrows must start with no SOL beyond rent.** `CreateDvp` accepts pre-created escrow ATAs (idempotent create) but rejects a non-WSOL escrow whose lamport balance exceeds its rent-exempt minimum (`EscrowPreloadedWithLamports`). Raw SOL is not token balance on a non-native leg, so the close paths would sweep it to the closing party instead of returning it to the sender. The check runs after the ATA create CPI, so it also catches lamports parked at the escrow address before the ATA existed. Raw SOL sent to a non-native escrow _after_ creation cannot be prevented and still goes to the closer — never send SOL to a non-WSOL escrow. Dust below the rent minimum is absorbed into rent by the ATA program's top-up and cannot grief creation; a preload above it blocks that `(seeds, nonce)` create, and the client retries with a fresh nonce.
- **Settle clamps to `amount_x`.** Because anyone holding the leg mint can transfer tokens into an escrow ATA, the escrow may hold more than `amount_x`. Settle transfers exactly `amount_x` to the counterparty and refunds the surplus to the depositor's own-mint ATA, so over-deposits don't leak.
- **Settlement destinations.** Each leg's proceeds go to the counterparty by default — `user_a` receives the cash leg, `user_b` the asset leg. `CreateDvp` can redirect either side with the optional `user_a_settlement_destination` / `user_b_settlement_destination` args (e.g. a custodian deposit wallet); proceeds then land at the destination's canonical ATA. Destinations are resolved at Create (the consent point) and stored — an omitted arg is recorded as the user's own address, so `Settle` never branches on them. They affect `Settle` only: surplus refunds and Reclaim/Cancel/Reject always return funds to the original depositor, never a destination. Because they redirect proceeds, destinations are part of the terms funders must verify before depositing (see "`CreateDvp` is permissionless" below).
- **Recipient ATAs are authenticated by contents, not just address.** A legacy SPL Token account's owner can be reassigned with `SetAuthority` without changing its ATA pubkey, so address canonicality alone would let a reassigned account keep receiving another wallet's proceeds. Before moving value, every path that transfers to a wallet ATA — `Settle` (both delivery legs), the `Cancel`/`Reject` refund path, and the `Reclaim`/`Recover` drains — re-reads each recipient ATA and requires its `owner`/`mint` fields still match the expected wallet and mint (`RecipientAtaMismatch` otherwise). The surplus and refund legs tolerate an uninitialized ATA (no transfer targets it) but reject a reassigned one, so funds stay in escrow to recover once the owner is restored rather than paying an attacker. Token-2022 ATAs are immune (the ATA program stamps `ImmutableOwner`); this closes the legacy-SPL case.
- **Opaque client reference (`ref_string`).** `CreateDvp` takes an optional reference (e.g. an off-chain order ID), at most 64 bytes (`RefStringTooLong` otherwise), stored zero-padded on the SwapDvp; omitting it stores all zeros. The program never reads or validates it — it exists for client/indexer correlation, and clients recover the value by trimming trailing zero bytes.
- **Reclaim/Cancel/Reject drain the escrow.** These instructions transfer the escrow's _actual_ balance back to the depositor, leaving a 0-balance escrow that `CloseAccount` accepts.
- **Late deposits are recoverable via `RecoverDvp`.** A raw funding transfer never reads the SwapDvp, so it can land after Settle/Cancel/Reject closed the trade — including when a counterparty deliberately front-runs an in-flight funding transaction with Reject. Anyone can recreate the escrow ATA for the dead PDA and the transfer settles into it, out of reach of Reclaim (the SwapDvp is gone) and of a same-nonce Create (tombstone). `RecoverDvp` closes that gap: the depositor supplies the original seed inputs (`settlement_authority`, `user_a`, `user_b`, `mint_a`, `mint_b`, `nonce`), the program re-derives the PDA and requires its nonce tombstone as proof the DvP existed, then drains the escrow to the signer's canonical ATA and closes it (rent to the signer). The signer selects the leg as in Reclaim. The token program is bound to the escrow account rather than the mint's current owner, so recovery still works if a closed Token-2022 mint is recreated under legacy SPL after the deposit landed. Recover skips the extension check like the other unwind paths, so the program never blocks it, but it still issues a real `TransferChecked`: if a mint's authority recreates it as `NonTransferable` or fee-bearing after close, the token program itself makes the late deposit unmovable or fee-taxed, which no recovery path can override (the same trusted-authority exception the live paths carry). Barring that, clients that keep the create parameters can recover a late deposit, and a malicious reject can only abort a trade, never burn the counterparty's deposit.
- **Closed-account rent goes to the closer, not the create payer.** `CreateDvp`'s payer funds the SwapDvp PDA and both escrow ATAs but isn't recorded as a rent recipient. On close those lamports go to the closing party: the settlement authority on `Settle`/`Cancel`, the rejecting `user_a`/`user_b` on `Reject`. If payer and closer differ, the payer subsidizes the closer.
- **Expiry.** Only `Settle` rejects after `expiry_timestamp`. `Reclaim`, `Cancel`, and `Reject` always work — otherwise an expired-but-funded DvP would strand funds. Reclaim is per-leg recovery: each party can pull their own leg back at any time, so a problem with the counterparty's leg can't lock a healthy leg behind the all-or-nothing Cancel/Reject path. Funding via raw SPL Transfer is unauthenticated by the program; clients must avoid funding past expiry, but any tokens that do land are still recoverable via Reclaim/Cancel/Reject.
- **Earliest settlement.** If `earliest_settlement_timestamp` is set, `Settle` additionally rejects when `now < earliest`.
- **Expiry is capped at one year.** `CreateDvp` rejects an `expiry_timestamp` more than one year past creation, bounding how long escrow rent can be locked. `earliest_settlement_timestamp` inherits the cap since it must be `<= expiry`.
- **Time is cluster time, not wall-clock.** All time gates use `Clock::unix_timestamp` (validator-vote median), which lags real time ~10 to 30s and can jump forward. Budget ~60s of drift: set `expiry` well past the real deadline and don't fund/settle/reclaim right at a boundary.
- **Nonces are single-use forever.** `CreateDvp` also creates a small nonce-tombstone PDA (seeds `[b"nonce", swap_dvp]`) that is never closed. A `(seeds, nonce)` combination can therefore only ever map to one trade: after a DvP closes, its PDA address can't be re-instantiated with new terms, so a deposit queued against the old escrow can't be captured by a recreated instance (it stays recoverable via `RecoverDvp`). The tombstone doubles as the proof-of-existence `RecoverDvp` checks. Use a fresh nonce for each new DvP between the same parties + mints.
- **Parties must be signer-capable identities.** `user_a` and `user_b` are passed as accounts and must each be system-owned and non-executable, i.e. a wallet-style identity that can authorize the unwind paths (Reject/Reclaim/Recover) — either an ordinary keypair wallet or a smart-wallet PDA that signs via CPI (e.g. a Squads vault). A party owned by another program, such as an SPL Token multisig, or an executable account is rejected at Create (`PartyNotSignerCapable`): it could control a funding account but could never sign to recover a late deposit, so its funds would be strandable. The `settlement_authority` carries no such requirement — if it can't sign, the parties still recover their funds via Reject/Reclaim. For custodian integrations that hold assets in a token multisig, route the DvP party through a smart-wallet/PDA the custodian controls rather than naming the multisig directly.

- **`CreateDvp` is permissionless — a record is not proof of agreement.** Only the payer signs; `user_a`, `user_b`, and `settlement_authority` do not. So anyone can create a `SwapDvp` for any parties with arbitrary terms (any non-zero amounts, any future expiry). The economic terms are stored in the account but are **not** part of the PDA seeds, so the address doesn't bind them. Two consequences clients must handle:
  - **Use a cryptographically random 64-bit `nonce` per trade.** A predictable nonce lets a third party squat the intended slot (and, after the real parties reject it, the tombstone burns that nonce — so they'd need a fresh one anyway). Random nonces make squatting/DoS impractical. In TypeScript, always pass the nonce (and 64-bit amounts) as a `bigint`: it is a PDA seed, and a plain `number` above 2^53 rounds and would derive the wrong address. The generated instruction builders and `findSwapDvpPda` reject a `number` at runtime.
  - **Verify stored terms before acting.** Funders must read the on-chain `SwapDvp` and check `amount_a`/`amount_b`/`expiry`/`earliest`/`user_a`/`user_b`/`settlement_authority`/`user_a_settlement_destination`/`user_b_settlement_destination` against the agreed deal — a forged create with attacker destinations would redirect settlement proceeds — _before_ depositing (escrow addresses derive only from PDA+mint, never from terms, so a raw transfer can land against terms you never agreed to). The `settlement_authority` must re-validate before `Settle`. Verification must also confirm the account is owned by the DvP program and is exactly `SwapDvp::LEN` (458) bytes; otherwise an attacker can serialize look-alike terms into a wallet-owned account and drain the escrow ATA you fund. The generated `fetch`/`decode` readers check neither, so use the checked helpers: `verifySwapDvp` / `decodeSwapDvpChecked` / `findSwapDvpPda` / `findSwapDvpEscrowAta` (TypeScript) or `verify::decode_swap_dvp_account` / `verify::find_swap_dvp_address` / `verify::find_swap_dvp_escrow_ata` (Rust).
- **Token-2022.** Each leg carries its own `token_program` account, so a single DvP can mix legacy SPL and Token-2022 mints. `CreateDvp` rejects mints carrying `TransferFee` (transfers deduct a fee) and `NonTransferable` (a balance reaching the escrow could never be drained, stranding both legs). `SettleDvp` re-runs the same check and also requires each mint to still be owned by the token program and to carry the same mint authority captured at Create, so a leg recreated after Create (see the close-authority note below) with a deny-listed extension, under the other token program, or with a fresh mint authority can't reach a counterfeit or short "successful" settlement. The recovery paths (`Cancel`/`Reject`/`Reclaim`/`Recover`) skip both checks so the program never blocks recovery when a mint changes later, but the token program's own transfer rules still apply: a mint recreated as `NonTransferable` (unmovable) or with a transfer fee (net payout) after Create is the trusted-authority exception, same as the other close/mint-authority risks below. `InterestBearing` and `ScaledUiAmount` are allowed: they affect only UI amounts, while DvP terms, deposits, and transfers use base units. Interest accrues over time and the issuer can change the ScaledUiAmount multiplier, so the same base amount can display a different UI amount at Create and Settle. Integrators should present amounts in base units, or show both base units and UI amounts. `PermanentDelegate`, `Pausable`, `MintCloseAuthority`, `DefaultAccountState`, `ConfidentialTransfer`, `ConfidentialTransferFeeConfig`, and a mint freeze authority are accepted as-is. `ConfidentialTransfer` doesn't force confidential transfers, and the escrow can't be configured to receive one (that needs the SwapDvp PDA to sign `ConfigureAccount`), so legs settle on exact public amounts — and the `ConfidentialTransferFeeConfig` fee applies only to confidential transfers, which therefore can't occur. These are all trusted-authority risks the program cannot defend against: a permanent delegate can move escrowed tokens, a freeze authority can freeze the escrow ATA (or a frozen `DefaultAccountState` can make new ATAs start frozen), a pause authority can halt transfers, and a close authority can close a zero-supply mint and recreate it at the same address with a different extension set, under the other token program, or with a fresh mint authority, changing transfer behavior mid-trade (Settle re-validates mint ownership, extensions, and mint authority so this can't force a counterfeit or short settlement, but a hostile recreation blocks Settle until the trade is unwound via Reject/Reclaim). Any of these can stall settlement or refunds until the authority cooperates. Treat the mint, freeze, and close authorities as trusted parties for the DvP's lifetime, and surface them in settlement UIs rather than presenting such mints as ordinary SPL tokens.
- **TransferHook.** Settle/Cancel/Reject/Reclaim/Recover issue `TransferChecked` CPIs and forward any trailing accounts to the token program as transfer-hook extras. Settle/Cancel/Reject split the trailing slice between the two legs via the `leg_a_extras_count: u8` data field (first `leg_a_extras_count` accounts go to leg A, rest to leg B); Reclaim and Recover have a single leg so all trailing accounts feed their one CPI. The client must resolve the hook's `ExtraAccountMetaList` off-chain. Note the generated IDL describes only the fixed accounts, not these trailing hook accounts (codama-derive drops instruction-level remaining accounts), so clients, indexers, and transaction builders must add them per this section rather than relying on the IDL's account list alone. Hook extras are capped at 32 accounts per leg (the CPI account arrays are stack-allocated). A mutable hook is a trusted-authority risk: the hook authority can update the `ExtraAccountMetaList` after funding to require more than the cap, which bricks every terminal transfer on that leg until the authority relents. Treat mutable-hook mints as trusted and surface this in client risk checks before funding. The program strips the signer bit from every forwarded hook extra (keeping only its writable flag), so a malicious hook mint that names a transaction signer, e.g. the settlement authority, as a static extra can never receive that signature: the hook sees the account writable-or-readonly but never as a signer, so it cannot sign transfers out of that wallet. The DvP PDA transfer authority is likewise safe (Token-2022 passes it to the hook read-only). Clients should still avoid passing unrelated writable accounts as hook extras.
- **User ATAs are caller-managed.** The program creates escrow ATAs at `CreateDvp` but never creates user-side ATAs. Settle/Cancel/Reject/Reclaim assume the user destination ATAs already exist for any leg whose balance will be transferred; uninitialized destinations cause the `TransferChecked` CPI to fail and revert the instruction. Callers should add `CreateIdempotent` pre-instructions as needed. For `SettleDvp` this means **all four** recipient ATAs: the two that receive the agreed amounts (`user_a_destination_ata_b`, `user_b_destination_ata_a` — the settlement destinations' ATAs, defaulting to the users') and the two surplus-refund ATAs (`user_a_ata_a`, `user_b_ata_b`). The surplus ATAs are not optional — anyone can dust an escrow, which forces a surplus refund, so a missing one reverts the whole settlement.
- **TransferHook + over-deposit caveat.** When Settle refunds a surplus, it issues a second `TransferChecked` CPI on the same mint with a different destination (the depositor's own ATA) and amount, but reuses the leg's extras slice. For hooks whose `ExtraAccountMetaList` resolves any account from the transfer's destination or amount (a valid but uncommon pattern), the surplus CPI will fail and revert Settle. Recovery: the depositor calls `Reclaim` to drain the leg, re-funds exactly `amount_x`, then Settle succeeds. Funds are never lost; only the over-deposit path is affected. Clients should fund hook-bearing escrows at exactly `amount_x` and warn on any surplus.

## Errors

| Code | Variant                          | When                                                                                                                            |
| ---- | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| 0    | `SignerNotParty`                 | Reclaim/Reject/Recover signer is not `user_a` or `user_b`                                                                       |
| 1    | `DvpExpired`                     | Settle after `expiry_timestamp`                                                                                                 |
| 2    | `SettlementAuthorityMismatch`    | Settle/Cancel signer is not `settlement_authority`                                                                              |
| 3    | `SettlementTooEarly`             | Settle when `now < earliest_settlement_timestamp`                                                                               |
| 4    | `LegNotFunded`                   | Settle when an escrow holds less than its target amount                                                                         |
| 5    | `ExpiryNotInFuture`              | Create with `expiry_timestamp <= now`                                                                                           |
| 6    | `EarliestAfterExpiry`            | Create with `earliest > expiry`                                                                                                 |
| 7    | `SelfDvp`                        | Create with `user_a == user_b`                                                                                                  |
| 8    | `SameMint`                       | Create with `mint_a == mint_b`                                                                                                  |
| 9    | `ZeroAmount`                     | Create with `amount_a == 0` or `amount_b == 0`                                                                                  |
| 10   | `BlockedMintExtension`           | Create or Settle with a Token-2022 mint carrying TransferFee or NonTransferable                                                 |
| 11   | `SettlementAuthorityIsParty`     | Create with `settlement_authority` equal to `user_a` or `user_b`                                                                |
| 12   | `SettlementAuthorityExecutable`  | Create with an executable `settlement_authority` (can't be credited closed-account rent)                                        |
| 13   | `NonceAlreadyUsed`               | Create reusing a `(seeds, nonce)` that already has a nonce tombstone (the address was used by a prior DvP)                      |
| 14   | `ExpiryTooFarInFuture`           | Create with `expiry_timestamp` more than one year past creation time                                                            |
| 15   | `RefStringTooLong`               | Create with a `ref_string` longer than 64 bytes                                                                                 |
| 16   | `DvpStillOpen`                   | Recover while the SwapDvp is still program-owned (use Reclaim instead)                                                          |
| 17   | `DvpNeverCreated`                | Recover with seed inputs that derive an address with no nonce tombstone                                                         |
| 18   | `EscrowPreloadedWithLamports`    | Create when a non-native escrow ATA holds lamports above its rent-exempt minimum                                                |
| 19   | `SettlementDestinationIsSwapDvp` | Create with a settlement destination equal to the `swap_dvp` PDA                                                                |
| 20   | `SwapDvpPreloadedWithLamports`   | Create when the `swap_dvp` account holds lamports above its rent reserve                                                        |
| 21   | `RecipientAtaMismatch`           | Settle/Cancel/Reject/Reclaim/Recover when a recipient or refund ATA's `owner`/`mint` no longer matches the expected wallet/mint |
| 22   | `PartyNotSignerCapable`          | Create with `user_a` or `user_b` that is not a system-owned, non-executable account                                             |
| 23   | `MintAuthorityChanged`           | Settle when a leg's mint authority no longer matches the value captured at Create                                               |

## Build & test

```sh
make build              # generate clients + cargo-build-sbf
make unit-test          # program crate's #[cfg(test)] modules + JS client tests
make integration-test   # build + LiteSVM integration tests
make fmt                # cargo fmt + clippy + pnpm format
make verify-program-id  # pre-deploy: deploy keypair matches the declared program ID
```

The integration tests live in `tests/integration-tests/` and run against the compiled `.so` via [LiteSVM](https://github.com/LiteSVM/litesvm). See `tests/integration-tests/src/` for one directory per instruction.

### Deploying

The address in `declare_id!` (and therefore the IDL and generated clients) is a temporary placeholder. Its keypair is not available, so the program has not been deployed to it and that ID is not the program's real address. A real deployment must:

1. Generate a fresh program keypair (`solana-keygen new -o target/deploy/dvp_swap_program-keypair.json`).
2. Set `declare_id!` to its pubkey and regenerate the IDL and clients (`make build`).
3. Run `make verify-program-id` to confirm the deploy keypair matches the declared ID.
4. `solana program deploy --program-id target/deploy/dvp_swap_program-keypair.json target/deploy/dvp_swap_program.so`.

`cargo-build-sbf` writes a random deploy keypair that does not match `declare_id!`, so deploying without these steps (or without an explicit `--program-id`) publishes to the wrong address.

## Layout

```
program/         on-chain program (no_std, pinocchio-based)
clients/rust/    Codama-generated Rust client
clients/typescript/  Codama-generated TypeScript client
idl/             generated Codama IDL
tests/integration-tests/   LiteSVM integration tests
```

## Audit

This program has been audited by [Cantina](https://cantina.xyz/portfolio/fe870bb4-d96d-4902-8aec-9dfcfa2d6a79).
