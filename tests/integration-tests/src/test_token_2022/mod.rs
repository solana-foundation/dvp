//! Token-2022 integration tests for the DvP Swap Program.
//!
//! Coverage:
//! - **Positive lifecycle** on Token-2022 mints with each *allowed*
//!   amount-preserving extension applied to one leg in isolation
//!   (Pausable, PermanentDelegate, TransferHook, ConfidentialTransfer,
//!   plain T22 with no extensions). Both single-program-T22 and mixed
//!   (legacy ↔ T22) topologies are exercised.
//! - **CreateDvp negative** for every *blocked* extension on either leg
//!   (TransferFeeConfig, InterestBearingConfig, ScaledUiAmount,
//!   NonTransferable).
//! - **Owner mismatch**: legacy SPL mint paired with T22 token program.
//! - **Post-Create extension activation**: a mint is swapped to a
//!   blocked-extension layout *after* CreateDvp, and Settle/Reject must
//!   still drain funds (the unwind paths intentionally skip the
//!   extension check so funds can never get stranded).
//! - **Post-Create cross-program recreation**: a mint is recreated under
//!   the other token program after CreateDvp; Settle must reject it.

use dvp_swap_program_client::instructions::{
    CancelDvpBuilder, CreateDvpBuilder, ReclaimDvpBuilder, RecoverDvpBuilder, RejectDvpBuilder,
    SettleDvpBuilder,
};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::{
    state_utils::{
        assert_cancel_dvp, assert_create_dvp, assert_fund_a, assert_fund_a_amount, assert_fund_b,
        assert_fund_b_amount, assert_reclaim_a, assert_reject_dvp, assert_settle_dvp,
        setup_dvp_with_programs, AMOUNT_A, AMOUNT_B, INITIAL_BALANCE, REF_STRING,
    },
    utils::{
        assert_instruction_error, assert_program_error, get_token_balance, hook_extras_for_mint,
        malicious_hook_extras, set_mint, set_mint_2022_with_confidential_transfer,
        set_mint_2022_with_interest_bearing, set_mint_2022_with_non_transferable,
        set_mint_2022_with_pausable, set_mint_2022_with_permanent_delegate,
        set_mint_2022_with_scaled_ui_amount, set_mint_2022_with_transfer_fee,
        set_mint_2022_with_transfer_hook, set_mint_with_authority,
        set_token_2022_with_hook_account, set_token_2022_with_memo_required, set_token_balance,
        setup_hook_mint, setup_malicious_hook_mint, TestContext, BLOCKED_MINT_EXTENSION,
        MEMO_PROGRAM_ID, MINT_AUTHORITY_CHANGED, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
    },
};

// ---------------------------------------------------------------------
// Positive lifecycle on Token-2022.
// ---------------------------------------------------------------------

/// Both legs are bare Token-2022 mints (no extensions). The full
/// Create → fund → Settle path must work end-to-end.
#[test]
fn test_full_lifecycle_both_legs_token_2022() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);
    assert_settle_dvp(&mut context, &fixture);

    // Each user receives the *other* leg's mint at exactly amount_x.
    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
    // Originating ATAs are debited by exactly amount_x.
    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE - AMOUNT_A
    );
    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE - AMOUNT_B
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
}

/// Cross-program lifecycle: asset leg is legacy SPL Token, cash leg is
/// Token-2022. The per-leg `token_program_a` / `token_program_b`
/// account lets these coexist in a single trade.
#[test]
fn test_full_lifecycle_mixed_legacy_and_token_2022() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(&mut context, 0, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID);

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);
    assert_settle_dvp(&mut context, &fixture);

    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
}

/// Allowed extension: PermanentDelegate on `mint_b`. Trade settles
/// normally; the extension does not affect amount mechanics.
#[test]
fn test_settle_with_permanent_delegate_on_mint_b() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    // Overwrite mint_b in place with a PermanentDelegate-bearing T22 mint.
    set_mint_2022_with_permanent_delegate(
        &mut context,
        &fixture.mint_b,
        &fixture.settlement_authority.pubkey(),
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);
    assert_settle_dvp(&mut context, &fixture);

    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
}

/// Allowed extension: Pausable on `mint_a` (initialized in the unpaused
/// state). Trade settles normally.
#[test]
fn test_settle_with_pausable_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    set_mint_2022_with_pausable(
        &mut context,
        &fixture.mint_a,
        &fixture.settlement_authority.pubkey(),
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);
    assert_settle_dvp(&mut context, &fixture);

    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
}

/// Allowed extension: ConfidentialTransfer on `mint_a`. The extension
/// doesn't force confidential transfers, and the escrow can't be
/// configured to receive one, so the public-transfer lifecycle settles
/// normally.
#[test]
fn test_settle_with_confidential_transfer_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    set_mint_2022_with_confidential_transfer(
        &mut context,
        &fixture.mint_a,
        &fixture.settlement_authority.pubkey(),
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);
    assert_settle_dvp(&mut context, &fixture);

    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
}

/// Allowed at CreateDvp: TransferHook is *not* in the deny-list. The
/// program forwards transfer-hook extras through every `TransferChecked`
/// CPI (Settle/Cancel/Reject/Reclaim), so a hook-bearing mint can run
/// the full lifecycle when the client passes the resolved extras as
/// trailing accounts. End-to-end Settle/Cancel/Reject coverage requires
/// a deployed hook program in the SVM and is deferred; this test pins
/// only the Create-time acceptance.
#[test]
fn test_create_accepts_transfer_hook_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    // Hook program ID is arbitrary — Create only checks for hook
    // *presence* on the deny-list (it isn't on it), so any non-zero
    // pubkey suffices. Use the swap program's own ID as a placeholder
    // we already have in scope.
    set_mint_2022_with_transfer_hook(
        &mut context,
        &fixture.mint_a,
        &dvp_swap_program_client::DVP_SWAP_PROGRAM_ID,
        &fixture.settlement_authority.pubkey(),
    );

    assert_create_dvp(&mut context, &fixture);
}

// ---------------------------------------------------------------------
// CreateDvp negative tests — one per blocked extension on either leg.
// ---------------------------------------------------------------------

/// Helper: derive the standard CreateDvp instruction from a fixture.
/// Used by the blocked-extension tests so they stay focused on the
/// rejection rather than rebuilding the full builder chain.
fn build_create_dvp_ix(
    context: &TestContext,
    fixture: &crate::state_utils::DvpFixture,
) -> solana_instruction::Instruction {
    CreateDvpBuilder::new()
        .payer(context.payer.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .nonce_tombstone(fixture.nonce_tombstone)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .user_a(fixture.user_a.pubkey())
        .user_b(fixture.user_b.pubkey())
        .settlement_authority(fixture.settlement_authority.pubkey())
        .amount_a(AMOUNT_A)
        .amount_b(AMOUNT_B)
        .expiry_timestamp(fixture.expiry)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction()
}

#[test]
fn test_create_rejects_transfer_fee_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    set_mint_2022_with_transfer_fee(
        &mut context,
        &fixture.mint_a,
        &fixture.settlement_authority.pubkey(),
        100,    // 1% fee
        10_000, // max
    );

    let ix = build_create_dvp_ix(&context, &fixture);
    assert_program_error(context.send(ix, &[]), BLOCKED_MINT_EXTENSION);
}

#[test]
fn test_create_rejects_transfer_fee_on_mint_b() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    set_mint_2022_with_transfer_fee(
        &mut context,
        &fixture.mint_b,
        &fixture.settlement_authority.pubkey(),
        100,
        10_000,
    );

    let ix = build_create_dvp_ix(&context, &fixture);
    assert_program_error(context.send(ix, &[]), BLOCKED_MINT_EXTENSION);
}

#[test]
fn test_create_rejects_interest_bearing_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    set_mint_2022_with_interest_bearing(
        &mut context,
        &fixture.mint_a,
        &fixture.settlement_authority.pubkey(),
    );

    let ix = build_create_dvp_ix(&context, &fixture);
    assert_program_error(context.send(ix, &[]), BLOCKED_MINT_EXTENSION);
}

#[test]
fn test_create_rejects_scaled_ui_amount_on_mint_b() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    set_mint_2022_with_scaled_ui_amount(
        &mut context,
        &fixture.mint_b,
        &fixture.settlement_authority.pubkey(),
    );

    let ix = build_create_dvp_ix(&context, &fixture);
    assert_program_error(context.send(ix, &[]), BLOCKED_MINT_EXTENSION);
}

/// `NonTransferable` permanently blocks transfers out of any escrow, so
/// a balance reaching it could never be settled, refunded, or reclaimed.
/// Create must reject the mint up front.
#[test]
fn test_create_rejects_non_transferable_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    set_mint_2022_with_non_transferable(&mut context, &fixture.mint_a);

    let ix = build_create_dvp_ix(&context, &fixture);
    assert_program_error(context.send(ix, &[]), BLOCKED_MINT_EXTENSION);
}

// ---------------------------------------------------------------------
// Mint / token-program ownership mismatch.
// ---------------------------------------------------------------------

/// Caller passes `token_program_a = TOKEN_2022_PROGRAM_ID` for a mint
/// that's actually owned by legacy SPL Token. `verify_account_owner`
/// rejects it with `InvalidAccountOwner`.
#[test]
fn test_create_rejects_mint_program_owner_mismatch_a() {
    let mut context = TestContext::new();
    let mut fixture = setup_dvp_with_programs(&mut context, 0, TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID);
    fixture.token_program_a = TOKEN_2022_PROGRAM_ID;
    // Re-derive `dvp_ata_a` for the new program so the ATA-canonicality
    // check passes and the owner check is the actual failure surfaced.
    fixture.dvp_ata_a =
        crate::utils::dvp_ata(&fixture.swap_dvp, &fixture.mint_a, &fixture.token_program_a);

    let result = context.send(build_create_dvp_ix(&context, &fixture), &[]);
    assert_instruction_error(result, "InvalidAccountOwner");
}

/// Symmetric leg-B mismatch.
#[test]
fn test_create_rejects_mint_program_owner_mismatch_b() {
    let mut context = TestContext::new();
    let mut fixture = setup_dvp_with_programs(&mut context, 0, TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID);
    fixture.token_program_b = TOKEN_2022_PROGRAM_ID;
    fixture.dvp_ata_b =
        crate::utils::dvp_ata(&fixture.swap_dvp, &fixture.mint_b, &fixture.token_program_b);

    let result = context.send(build_create_dvp_ix(&context, &fixture), &[]);
    assert_instruction_error(result, "InvalidAccountOwner");
}

// ---------------------------------------------------------------------
// Extension validation runs at Create AND Settle. The recovery
// paths (Cancel/Reject/Reclaim/Recover) skip it so the program never
// blocks recovery after a post-Create mint mutation. That is not a
// promise of full recovery: they still issue a real TransferChecked, so
// a mint recreated as NonTransferable or fee-bearing is unmovable or
// taxed by the token program itself (a trusted-authority risk, not one
// the program can override). The tests below pin both halves:
// Settle re-validates and rejects a leg that gained a blocked extension
// after Create (then the honest party recovers via Reject), while Reject
// itself does no extension check.
// ---------------------------------------------------------------------

/// The exploit: a party leaves their own leg unfunded
/// until the counterparty commits, then closes the zero-supply mint and
/// recreates it at the same address with a deny-listed extension (e.g.
/// TransferFee, which short-delivers the leg). Settle must re-validate
/// and reject. This test uses `InterestBearingConfig` — also deny-listed,
/// and hitting the identical `validate_mint_extensions` branch that
/// short-circuits before any balance/ATA/fee logic, so it guards the
/// whole deny-list (TransferFee included), not just one extension. The
/// honest counterparty then recovers both legs via Reject, which stays
/// tolerant.
#[test]
fn test_settle_rejects_post_create_blocked_extension() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    // Swap mint_a in place to an InterestBearing-bearing T22 mint —
    // deny-listed, so Settle re-validation must reject it.
    set_mint_2022_with_interest_bearing(
        &mut context,
        &fixture.mint_a,
        &fixture.settlement_authority.pubkey(),
    );

    let settle_ix = SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(0)
        .instruction();
    assert_program_error(
        context.send(settle_ix, &[&fixture.settlement_authority]),
        BLOCKED_MINT_EXTENSION,
    );

    // Funds are not stranded: Reject recovers both legs (InterestBearing
    // does not block transfers, and Reject skips the extension check).
    assert_reject_dvp(&mut context, &fixture, &fixture.user_a);
    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
}

/// Inject cross-program mint recreation and an escrow balance. Settle
/// rejects the owner mismatch. Reject also fails because the pinned
/// Token-2022 binary checks the mint owner during TransferChecked;
/// both escrows and the live DvP must remain unchanged.
#[test]
fn test_settle_rejects_mint_recreated_under_other_token_program() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    // Only the honest counterparty funds; leg A stays empty until the
    // counterfeit below.
    assert_fund_b(&mut context, &fixture);

    // Inject adversarial state directly. This does not establish that
    // these mutations can be achieved through real token instructions.
    set_mint(&mut context, &fixture.mint_a, &TOKEN_PROGRAM_ID);
    set_token_balance(
        &mut context,
        &fixture.dvp_ata_a,
        &fixture.mint_a,
        &fixture.swap_dvp,
        AMOUNT_A,
        &TOKEN_2022_PROGRAM_ID,
    );

    let settle_ix = SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(0)
        .instruction();
    assert_instruction_error(
        context.send(settle_ix, &[&fixture.settlement_authority]),
        "InvalidAccountOwner",
    );

    let addresses = [
        fixture.swap_dvp,
        fixture.dvp_ata_a,
        fixture.dvp_ata_b,
        fixture.user_a_ata_a,
        fixture.user_b_ata_b,
    ];
    let before = addresses.map(|address| context.get_account(&address).unwrap());
    let reject_ix = RejectDvpBuilder::new()
        .signer(fixture.user_b.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(0)
        .instruction();
    assert_instruction_error(
        context.send(reject_ix, &[&fixture.user_b]),
        "IncorrectProgramId",
    );
    for (address, account) in addresses.into_iter().zip(before) {
        assert_eq!(context.get_account(&address).unwrap(), account);
    }
}

/// Models mint recreation with no tokens left in the old leg A accounts.
/// Unlike the injected non-zero escrow case above, no transfer of A is needed.
fn setup_dvp_with_empty_recreated_mint_a(
    context: &mut TestContext,
) -> crate::state_utils::DvpFixture {
    let fixture = setup_dvp_with_programs(context, 0, TOKEN_2022_PROGRAM_ID, TOKEN_2022_PROGRAM_ID);
    set_token_balance(
        context,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &fixture.user_a.pubkey(),
        0,
        &TOKEN_2022_PROGRAM_ID,
    );
    assert_create_dvp(context, &fixture);
    assert_fund_b(context, &fixture);

    // Model the recreated mint directly; token balances in the old ATAs stay zero.
    set_mint(context, &fixture.mint_a, &TOKEN_PROGRAM_ID);
    assert_eq!(get_token_balance(context, &fixture.dvp_ata_a), 0);
    fixture
}

/// Reject skips the empty leg A transfer, refunds B and closes both escrows.
#[test]
fn test_reject_recovers_leg_b_when_empty_leg_a_mint_recreated() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_empty_recreated_mint_a(&mut context);

    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);

    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_a), 0);
    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
    assert!(context.get_account(&fixture.dvp_ata_a).is_none());
    assert!(context.get_account(&fixture.dvp_ata_b).is_none());
}

/// Reclaim of B does not read the recreated mint A and leaves the trade open.
#[test]
fn test_reclaim_recovers_leg_b_when_empty_leg_a_mint_recreated() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_empty_recreated_mint_a(&mut context);
    let swap_before = context.get_account(&fixture.swap_dvp).unwrap();
    let escrow_a_before = context.get_account(&fixture.dvp_ata_a).unwrap();
    let ix = ReclaimDvpBuilder::new()
        .signer(fixture.user_b.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint(fixture.mint_b)
        .dvp_source_ata(fixture.dvp_ata_b)
        .signer_dest_ata(fixture.user_b_ata_b)
        .token_program(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .instruction();
    context.send(ix, &[&fixture.user_b]).expect("ReclaimDvp");

    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert_eq!(get_token_balance(&context, &fixture.dvp_ata_b), 0);
    assert_eq!(context.get_account(&fixture.swap_dvp).unwrap(), swap_before);
    assert_eq!(
        context.get_account(&fixture.dvp_ata_a).unwrap(),
        escrow_a_before
    );
}

/// A leg with no mint authority at Create can't be recreated with one and
/// counterfeit-minted into the escrow: Settle pins the authority.
#[test]
fn test_settle_rejects_mint_authority_gained_after_create() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    let attacker = Pubkey::new_unique();
    set_mint_with_authority(
        &mut context,
        &fixture.mint_a,
        &TOKEN_2022_PROGRAM_ID,
        Some(attacker),
    );
    set_token_balance(
        &mut context,
        &fixture.dvp_ata_a,
        &fixture.mint_a,
        &fixture.swap_dvp,
        AMOUNT_A,
        &TOKEN_2022_PROGRAM_ID,
    );

    let settle_ix = SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(0)
        .instruction();
    assert_program_error(
        context.send(settle_ix, &[&fixture.settlement_authority]),
        MINT_AUTHORITY_CHANGED,
    );
}

/// The pin stores the exact Create-time key, so swapping one authority for
/// another (not only None -> Some) is rejected.
#[test]
fn test_settle_rejects_mint_authority_swapped_after_create() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    let original = Pubkey::new_unique();
    set_mint_with_authority(
        &mut context,
        &fixture.mint_a,
        &TOKEN_2022_PROGRAM_ID,
        Some(original),
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    let attacker = Pubkey::new_unique();
    set_mint_with_authority(
        &mut context,
        &fixture.mint_a,
        &TOKEN_2022_PROGRAM_ID,
        Some(attacker),
    );

    let settle_ix = SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(0)
        .instruction();
    assert_program_error(
        context.send(settle_ix, &[&fixture.settlement_authority]),
        MINT_AUTHORITY_CHANGED,
    );
}

/// A mint keeping its Create-time authority is untouched by the pin.
#[test]
fn test_settle_succeeds_when_mint_authority_unchanged() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    let live_authority = Pubkey::new_unique();
    set_mint_with_authority(
        &mut context,
        &fixture.mint_a,
        &TOKEN_2022_PROGRAM_ID,
        Some(live_authority),
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    assert_settle_dvp(&mut context, &fixture);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
}

/// The pin covers leg B too, not just leg A: mint_b gaining an authority
/// after Create must reject at Settle.
#[test]
fn test_settle_rejects_mint_b_authority_gained_after_create() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);

    let attacker = Pubkey::new_unique();
    set_mint_with_authority(
        &mut context,
        &fixture.mint_b,
        &TOKEN_2022_PROGRAM_ID,
        Some(attacker),
    );
    set_token_balance(
        &mut context,
        &fixture.dvp_ata_b,
        &fixture.mint_b,
        &fixture.swap_dvp,
        AMOUNT_B,
        &TOKEN_2022_PROGRAM_ID,
    );

    let settle_ix = SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(0)
        .instruction();
    assert_program_error(
        context.send(settle_ix, &[&fixture.settlement_authority]),
        MINT_AUTHORITY_CHANGED,
    );
}

/// Same property on the Reject unwind path: post-Create, `mint_a` is
/// swapped to a Pausable T22 mint (allowed, but the point is that
/// Reject does no extension check regardless). Reject must still drain
/// both legs back to their depositors.
#[test]
fn test_reject_succeeds_after_post_create_extension_change() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    set_mint_2022_with_pausable(
        &mut context,
        &fixture.mint_a,
        &fixture.settlement_authority.pubkey(),
    );

    assert_reject_dvp(&mut context, &fixture, &fixture.user_a);

    // Both depositors recover their full balances.
    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
}

// ---------------------------------------------------------------------
// TransferHook end-to-end: the test hook fixture (`HOOK_FIXTURE_PROGRAM_ID`)
// logs `hook accounts: N` on every Execute. With one declared extra in
// the `ExtraAccountMetaList` (the system program), Token-2022 invokes
// the hook with 6 accounts: source, mint, destination, authority,
// validation PDA, system program. Each test asserts the log appears the
// expected number of times for that lifecycle path.
// ---------------------------------------------------------------------

const HOOK_LOG: &str = "hook accounts: 6";

/// Installs the hook fixture on `mint_a` and rewrites the user ATAs that
/// hold it so they carry the `TransferHookAccount` extension. The escrow
/// `dvp_ata_a` is created by the swap program's CPI to the ATA program
/// *after* the mint already bears `TransferHook`, so it gets the
/// extension automatically — only the pre-existing user ATAs need
/// fix-up here.
fn setup_hook_on_mint_a(context: &mut TestContext, fixture: &crate::state_utils::DvpFixture) {
    setup_hook_mint(context, &fixture.mint_a);
    set_token_2022_with_hook_account(
        context,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &fixture.user_a.pubkey(),
        INITIAL_BALANCE,
    );
    set_token_2022_with_hook_account(
        context,
        &fixture.user_b_ata_a,
        &fixture.mint_a,
        &fixture.user_b.pubkey(),
        0,
    );
}

/// Funds leg A by issuing a `transfer_checked` with the hook trailing
/// accounts appended — the raw SPL transfer used as the canonical funding
/// path needs the same extras the swap program forwards on settle.
fn fund_a_with_hook(context: &mut TestContext, fixture: &crate::state_utils::DvpFixture) {
    let extras = hook_extras_for_mint(&fixture.mint_a);
    let mut ix = spl_token_2022_interface::instruction::transfer_checked(
        &TOKEN_2022_PROGRAM_ID,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a.pubkey(),
        &[],
        AMOUNT_A,
        6,
    )
    .expect("build transfer_checked");
    ix.accounts.extend_from_slice(&extras);
    context
        .send(ix, &[&fixture.user_a])
        .expect("fund A with hook");
}

#[test]
fn test_settle_with_hook_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    setup_hook_on_mint_a(&mut context, &fixture);

    assert_create_dvp(&mut context, &fixture);
    fund_a_with_hook(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    let leg_a_extras = hook_extras_for_mint(&fixture.mint_a);
    let ix = SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(leg_a_extras.len() as u8)
        .add_remaining_accounts(&leg_a_extras)
        .instruction();
    let meta = context
        .send(ix, &[&fixture.settlement_authority])
        .expect("settle with hook");

    let hits = meta.logs.iter().filter(|l| l.contains(HOOK_LOG)).count();
    assert_eq!(hits, 1, "hook should run exactly once for the asset leg");

    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
}

/// A malicious hook mint declares the settlement authority as a
/// signer-bearing hook extra so a generic resolver forwards its signature
/// into the hook CPI, letting the hook drain the operator wallet during a
/// routine Settle. The program strips the signer bit from every forwarded
/// hook extra, so the hook's drain CPI is missing a required signer and
/// the whole Settle reverts — the authority keeps its lamports.
#[test]
fn test_settle_rejects_signer_bearing_hook_extra() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    // Benign EAML first so the leg-A funding transfer (authority: user_a,
    // not the settlement authority) doesn't trip the drain path.
    setup_hook_on_mint_a(&mut context, &fixture);

    assert_create_dvp(&mut context, &fixture);
    fund_a_with_hook(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    // Swap in the malicious EAML: extra[0] = the settlement authority
    // (signer), extra[1] = the attacker's drain target.
    let attacker = Keypair::new();
    let victim = fixture.settlement_authority.pubkey();
    setup_malicious_hook_mint(&mut context, &fixture.mint_a, &victim, &attacker.pubkey());

    let authority_before = context.get_account(&victim).unwrap().lamports;

    let leg_a_extras = malicious_hook_extras(&fixture.mint_a, &victim, &attacker.pubkey());
    let ix = SettleDvpBuilder::new()
        .settlement_authority(victim)
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(leg_a_extras.len() as u8)
        .add_remaining_accounts(&leg_a_extras)
        .instruction();
    let result = context.send(ix, &[&fixture.settlement_authority]);

    // The hook's drain CPI fails without the signer bit, reverting Settle.
    assert!(
        result.is_err(),
        "settle must revert when a hook tries to abuse a forwarded signer"
    );
    // No lamports left the authority, and the attacker got nothing.
    assert_eq!(
        context.get_account(&victim).unwrap().lamports,
        authority_before,
        "settlement authority wallet must be untouched"
    );
    assert_eq!(
        context
            .get_account(&attacker.pubkey())
            .map(|a| a.lamports)
            .unwrap_or(0),
        0,
        "attacker must not receive any drained lamports"
    );
    // DvP is untouched and can still be settled/cancelled normally.
    assert!(context.get_account(&fixture.swap_dvp).is_some());
}

#[test]
fn test_cancel_with_hook_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    setup_hook_on_mint_a(&mut context, &fixture);

    assert_create_dvp(&mut context, &fixture);
    fund_a_with_hook(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    let leg_a_extras = hook_extras_for_mint(&fixture.mint_a);
    let ix = CancelDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(leg_a_extras.len() as u8)
        .add_remaining_accounts(&leg_a_extras)
        .instruction();
    let meta = context
        .send(ix, &[&fixture.settlement_authority])
        .expect("cancel with hook");

    let hits = meta.logs.iter().filter(|l| l.contains(HOOK_LOG)).count();
    assert_eq!(hits, 1, "hook should run once for the leg A refund");

    // Both depositors recover their deposit; DvP is closed.
    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
}

#[test]
fn test_reject_with_hook_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    setup_hook_on_mint_a(&mut context, &fixture);

    assert_create_dvp(&mut context, &fixture);
    fund_a_with_hook(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    let leg_a_extras = hook_extras_for_mint(&fixture.mint_a);
    let ix = RejectDvpBuilder::new()
        .signer(fixture.user_a.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(leg_a_extras.len() as u8)
        .add_remaining_accounts(&leg_a_extras)
        .instruction();
    let meta = context
        .send(ix, &[&fixture.user_a])
        .expect("reject with hook");

    let hits = meta.logs.iter().filter(|l| l.contains(HOOK_LOG)).count();
    assert_eq!(hits, 1, "hook should run once for the leg A refund");
    assert!(context.get_account(&fixture.swap_dvp).is_none());
}

/// The same signer-leak vector, exercised through the Cancel/Reject
/// shared `refund_and_close_dvp` path. A malicious hook mint names the
/// rejecting party (`user_a`) as a signer-bearing extra; the program
/// strips the signer bit so the hook's drain CPI is missing a required
/// signer and Reject reverts, leaving the rejecting user's wallet intact.
#[test]
fn test_reject_rejects_signer_bearing_hook_extra() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    // Benign EAML for funding (authorized by user_a), then the malicious
    // EAML for the Reject under attack.
    setup_hook_on_mint_a(&mut context, &fixture);

    assert_create_dvp(&mut context, &fixture);
    fund_a_with_hook(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    let attacker = Keypair::new();
    let victim = fixture.user_a.pubkey();
    setup_malicious_hook_mint(&mut context, &fixture.mint_a, &victim, &attacker.pubkey());

    let victim_before = context.get_account(&victim).unwrap().lamports;

    let leg_a_extras = malicious_hook_extras(&fixture.mint_a, &victim, &attacker.pubkey());
    let ix = RejectDvpBuilder::new()
        .signer(victim)
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(leg_a_extras.len() as u8)
        .add_remaining_accounts(&leg_a_extras)
        .instruction();
    let result = context.send(ix, &[&fixture.user_a]);

    assert!(
        result.is_err(),
        "reject must revert when a hook tries to abuse a forwarded signer"
    );
    assert_eq!(
        context.get_account(&victim).unwrap().lamports,
        victim_before,
        "rejecting user wallet must be untouched"
    );
    assert_eq!(
        context
            .get_account(&attacker.pubkey())
            .map(|a| a.lamports)
            .unwrap_or(0),
        0,
        "attacker must not receive any drained lamports"
    );
    // Reject reverted, so the DvP is still open.
    assert!(context.get_account(&fixture.swap_dvp).is_some());
}

#[test]
fn test_reclaim_with_hook_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    setup_hook_on_mint_a(&mut context, &fixture);

    assert_create_dvp(&mut context, &fixture);
    fund_a_with_hook(&mut context, &fixture);

    let leg_a_extras = hook_extras_for_mint(&fixture.mint_a);
    let ix = ReclaimDvpBuilder::new()
        .signer(fixture.user_a.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint(fixture.mint_a)
        .dvp_source_ata(fixture.dvp_ata_a)
        .signer_dest_ata(fixture.user_a_ata_a)
        .token_program(fixture.token_program_a)
        .memo_program(MEMO_PROGRAM_ID)
        .add_remaining_accounts(&leg_a_extras)
        .instruction();
    let meta = context
        .send(ix, &[&fixture.user_a])
        .expect("reclaim with hook");

    let hits = meta.logs.iter().filter(|l| l.contains(HOOK_LOG)).count();
    assert_eq!(hits, 1, "hook should run once for the reclaim transfer");
    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
}

/// The same signer-leak vector, but on the depositor recovery path: a
/// malicious hook mint names the reclaiming party (`user_a`) as a
/// signer-bearing extra so a generic resolver forwards their signature
/// into the hook CPI, letting the hook drain the depositor's wallet
/// during a routine Reclaim. The program strips the signer bit, so the
/// drain CPI is missing a required signer and Reclaim reverts.
#[test]
fn test_reclaim_rejects_signer_bearing_hook_extra() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    // Benign EAML for funding (authorized by user_a, before the malicious
    // swap-in), then the malicious EAML for the Reclaim under attack.
    setup_hook_on_mint_a(&mut context, &fixture);

    assert_create_dvp(&mut context, &fixture);
    fund_a_with_hook(&mut context, &fixture);

    let attacker = Keypair::new();
    let victim = fixture.user_a.pubkey();
    setup_malicious_hook_mint(&mut context, &fixture.mint_a, &victim, &attacker.pubkey());

    let victim_before = context.get_account(&victim).unwrap().lamports;

    let leg_a_extras = malicious_hook_extras(&fixture.mint_a, &victim, &attacker.pubkey());
    let ix = ReclaimDvpBuilder::new()
        .signer(victim)
        .swap_dvp(fixture.swap_dvp)
        .mint(fixture.mint_a)
        .dvp_source_ata(fixture.dvp_ata_a)
        .signer_dest_ata(fixture.user_a_ata_a)
        .token_program(fixture.token_program_a)
        .memo_program(MEMO_PROGRAM_ID)
        .add_remaining_accounts(&leg_a_extras)
        .instruction();
    let result = context.send(ix, &[&fixture.user_a]);

    assert!(
        result.is_err(),
        "reclaim must revert when a hook tries to abuse a forwarded signer"
    );
    assert_eq!(
        context.get_account(&victim).unwrap().lamports,
        victim_before,
        "depositor wallet must be untouched"
    );
    assert_eq!(
        context
            .get_account(&attacker.pubkey())
            .map(|a| a.lamports)
            .unwrap_or(0),
        0,
        "attacker must not receive any drained lamports"
    );
}

/// Recreate the escrow ATA for the (now closed) SwapDvp wallet so a late
/// deposit can land in it, as the funding-race attack does.
fn recreate_dead_escrow_a(context: &mut TestContext, fixture: &crate::state_utils::DvpFixture) {
    let ix = spl_associated_token_account_interface::instruction::create_associated_token_account(
        &context.payer.pubkey(),
        &fixture.swap_dvp,
        &fixture.mint_a,
        &fixture.token_program_a,
    );
    context.send(ix, &[]).expect("recreate dead escrow ATA");
}

/// The same signer-leak vector on the RecoverDvp path. RecoverDvp also
/// forwards hook extras into its drain `TransferChecked` CPI, so the
/// signer stripping must hold there too. A late deposit lands in a
/// recreated escrow of a closed DvP; a malicious hook mint then names the
/// recovering party as a signer-bearing extra so a generic resolver would
/// forward their signature into the hook CPI. The program strips the
/// signer bit, so the drain CPI is missing a required signer and
/// RecoverDvp reverts, leaving the recovering user's wallet intact.
#[test]
fn test_recover_dvp_rejects_signer_bearing_hook_extra() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    // Benign EAML so the late funding transfer (authority: user_a) works,
    // then the malicious EAML for the RecoverDvp under attack.
    setup_hook_on_mint_a(&mut context, &fixture);

    assert_create_dvp(&mut context, &fixture);
    // Reject before funding closes the DvP with empty escrows (no hook
    // CPI), giving RecoverDvp its post-close precondition.
    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);

    // Recreate the dead escrow and let user_a's in-flight funding land in
    // it, so RecoverDvp has a non-zero balance to drain via the hook.
    recreate_dead_escrow_a(&mut context, &fixture);
    fund_a_with_hook(&mut context, &fixture);

    // Swap in the malicious EAML: extra[0] = the recovering party
    // (signer), extra[1] = the attacker's drain target.
    let attacker = Keypair::new();
    let victim = fixture.user_a.pubkey();
    setup_malicious_hook_mint(&mut context, &fixture.mint_a, &victim, &attacker.pubkey());

    let victim_before = context.get_account(&victim).unwrap().lamports;

    let leg_a_extras = malicious_hook_extras(&fixture.mint_a, &victim, &attacker.pubkey());
    let ix = RecoverDvpBuilder::new()
        .signer(victim)
        .swap_dvp(fixture.swap_dvp)
        .nonce_tombstone(fixture.nonce_tombstone)
        .mint(fixture.mint_a)
        .dvp_escrow_ata(fixture.dvp_ata_a)
        .signer_dest_ata(fixture.user_a_ata_a)
        .token_program(fixture.token_program_a)
        .memo_program(MEMO_PROGRAM_ID)
        .settlement_authority(fixture.settlement_authority.pubkey())
        .user_a(fixture.user_a.pubkey())
        .user_b(fixture.user_b.pubkey())
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .nonce(fixture.nonce)
        .add_remaining_accounts(&leg_a_extras)
        .instruction();
    let result = context.send(ix, &[&fixture.user_a]);

    assert!(
        result.is_err(),
        "recover must revert when a hook tries to abuse a forwarded signer"
    );
    assert_eq!(
        context.get_account(&victim).unwrap().lamports,
        victim_before,
        "recovering user wallet must be untouched"
    );
    assert_eq!(
        context
            .get_account(&attacker.pubkey())
            .map(|a| a.lamports)
            .unwrap_or(0),
        0,
        "attacker must not receive any drained lamports"
    );
}

/// Settle invokes `transfer_checked_cpi` a *second* time on leg A when
/// the escrow holds more than `amount_a` (over-funding), to refund the
/// surplus to user_a. That second CPI must also forward the hook extras
/// — if `leg_a_extras` were silently dropped from the surplus CPI, the
/// `set_transferring` step on user_a_ata_a would still succeed (the
/// account carries the extension), but the hook would not fire. Hence
/// asserting `hits == 2` pins that the surplus path forwards the extras.
#[test]
fn test_settle_with_hook_and_surplus_on_mint_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    setup_hook_on_mint_a(&mut context, &fixture);

    assert_create_dvp(&mut context, &fixture);

    // Over-fund leg A by `surplus`; leg B funded exactly.
    let surplus = 1_234u64;
    let extras = hook_extras_for_mint(&fixture.mint_a);
    let mut fund_ix = spl_token_2022_interface::instruction::transfer_checked(
        &TOKEN_2022_PROGRAM_ID,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a.pubkey(),
        &[],
        AMOUNT_A + surplus,
        6,
    )
    .expect("build transfer_checked");
    fund_ix.accounts.extend_from_slice(&extras);
    context
        .send(fund_ix, &[&fixture.user_a])
        .expect("fund A with surplus + hook");
    assert_fund_b(&mut context, &fixture);

    let leg_a_extras = hook_extras_for_mint(&fixture.mint_a);
    let ix = SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(leg_a_extras.len() as u8)
        .add_remaining_accounts(&leg_a_extras)
        .instruction();
    let meta = context
        .send(ix, &[&fixture.settlement_authority])
        .expect("settle with hook + surplus");

    let hits = meta.logs.iter().filter(|l| l.contains(HOOK_LOG)).count();
    assert_eq!(
        hits, 2,
        "hook should run twice: once for the leg transfer, once for the surplus refund"
    );

    // Surplus landed back on user_a; counterparty got exactly AMOUNT_A.
    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE - AMOUNT_A
    );
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
}

/// Same hook fixture on both mints: the test asserts the log fires
/// exactly twice (once per leg). If `split_leg_remaining_accounts`
/// flipped the slices, each leg would receive the *other* mint's
/// validation PDA. `invoke_execute` searches the trailing accounts for
/// the validation PDA derived from the *transferring* mint; if not
/// found, it CPIs the hook with only 4 accounts ("hook accounts: 4")
/// instead of 6. So a swapped split flips the count and breaks this
/// assertion.
#[test]
fn test_settle_with_hooks_on_both_legs() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    // Mint A hook + ATAs that hold mint_a.
    setup_hook_on_mint_a(&mut context, &fixture);
    // Mint B hook + ATAs that hold mint_b.
    setup_hook_mint(&mut context, &fixture.mint_b);
    set_token_2022_with_hook_account(
        &mut context,
        &fixture.user_b_ata_b,
        &fixture.mint_b,
        &fixture.user_b.pubkey(),
        INITIAL_BALANCE,
    );
    set_token_2022_with_hook_account(
        &mut context,
        &fixture.user_a_ata_b,
        &fixture.mint_b,
        &fixture.user_a.pubkey(),
        0,
    );

    assert_create_dvp(&mut context, &fixture);
    fund_a_with_hook(&mut context, &fixture);

    // Fund B by raw transfer_checked + hook extras for mint_b.
    let extras_b = hook_extras_for_mint(&fixture.mint_b);
    let mut fund_b_ix = spl_token_2022_interface::instruction::transfer_checked(
        &TOKEN_2022_PROGRAM_ID,
        &fixture.user_b_ata_b,
        &fixture.mint_b,
        &fixture.dvp_ata_b,
        &fixture.user_b.pubkey(),
        &[],
        AMOUNT_B,
        6,
    )
    .expect("build transfer_checked");
    fund_b_ix.accounts.extend_from_slice(&extras_b);
    context
        .send(fund_b_ix, &[&fixture.user_b])
        .expect("fund B with hook");

    let leg_a_extras = hook_extras_for_mint(&fixture.mint_a);
    let leg_b_extras = hook_extras_for_mint(&fixture.mint_b);
    let mut remaining = leg_a_extras.clone();
    remaining.extend_from_slice(&leg_b_extras);
    let ix = SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(leg_a_extras.len() as u8)
        .add_remaining_accounts(&remaining)
        .instruction();
    let meta = context
        .send(ix, &[&fixture.settlement_authority])
        .expect("settle with hooks on both legs");

    let hits = meta.logs.iter().filter(|l| l.contains(HOOK_LOG)).count();
    assert_eq!(hits, 2, "hook should run exactly once per leg");

    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
}

/// `split_leg_remaining_accounts` rejects an extras count greater than
/// the number of trailing accounts actually passed. Direct boundary test
/// for the bound at `processor/shared/utils.rs:split_leg_remaining_accounts`.
#[test]
fn test_settle_rejects_extras_count_overrun() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    // leg_a_extras_count = 5 but no trailing accounts passed.
    let ix = SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(5)
        .instruction();
    let result = context.send(ix, &[&fixture.settlement_authority]);
    assert_instruction_error(result, "InvalidInstructionData");
}

// Closed-mint recovery: a closed leg mint must not block refunding the
// other leg.

/// Overwrite `mint` as an empty System-owned account (a closed mint).
fn close_mint(context: &mut TestContext, mint: &solana_pubkey::Pubkey) {
    context
        .svm
        .set_account(
            *mint,
            Account {
                lamports: 1,
                data: vec![],
                owner: solana_sdk_ids::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

/// leg A unfunded with its mint closed, leg B funded; Cancel refunds B.
#[test]
fn test_cancel_recovers_funded_leg_when_other_legs_mint_closed() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    close_mint(&mut context, &fixture.mint_a);

    // Past expiry, so Reclaim is no longer an option.
    let advance = fixture.expiry - context.now() + 1;
    context.advance_clock(advance);

    assert_cancel_dvp(&mut context, &fixture);

    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
    assert!(context.get_account(&fixture.dvp_ata_a).is_none());
    assert!(context.get_account(&fixture.dvp_ata_b).is_none());
}

/// A memo-required refund destination must not block Cancel. Both legs
/// funded; user_a's refund ATA requires an incoming memo. Cancel must
/// still refund both legs.
#[test]
fn test_cancel_with_memo_required_destination() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    set_token_2022_with_memo_required(
        &mut context,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &fixture.user_a.pubkey(),
        INITIAL_BALANCE - AMOUNT_A,
    );

    assert_cancel_dvp(&mut context, &fixture);

    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
}

/// A memo-required refund destination must not block Reject.
#[test]
fn test_reject_with_memo_required_destination() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);

    set_token_2022_with_memo_required(
        &mut context,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &fixture.user_a.pubkey(),
        INITIAL_BALANCE - AMOUNT_A,
    );

    assert_reject_dvp(&mut context, &fixture, &fixture.user_a);

    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
}

/// A memo-required destination must not block Reclaim.
#[test]
fn test_reclaim_with_memo_required_destination() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);

    set_token_2022_with_memo_required(
        &mut context,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &fixture.user_a.pubkey(),
        INITIAL_BALANCE - AMOUNT_A,
    );

    assert_reclaim_a(&mut context, &fixture);

    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
}

/// Every Settle destination (both deliveries and both surplus refunds)
/// requires a memo; Settle must emit one before each transfer.
#[test]
fn test_settle_with_memo_required_destinations() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    let surplus_a: u64 = 5_000;
    let surplus_b: u64 = 3_000;

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a_amount(&mut context, &fixture, AMOUNT_A + surplus_a);
    assert_fund_b_amount(&mut context, &fixture, AMOUNT_B + surplus_b);

    set_token_2022_with_memo_required(
        &mut context,
        &fixture.user_a_ata_b,
        &fixture.mint_b,
        &fixture.user_a.pubkey(),
        0,
    );
    set_token_2022_with_memo_required(
        &mut context,
        &fixture.user_b_ata_a,
        &fixture.mint_a,
        &fixture.user_b.pubkey(),
        0,
    );
    set_token_2022_with_memo_required(
        &mut context,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &fixture.user_a.pubkey(),
        INITIAL_BALANCE - AMOUNT_A - surplus_a,
    );
    set_token_2022_with_memo_required(
        &mut context,
        &fixture.user_b_ata_b,
        &fixture.mint_b,
        &fixture.user_b.pubkey(),
        INITIAL_BALANCE - AMOUNT_B - surplus_b,
    );

    assert_settle_dvp(&mut context, &fixture);

    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE - AMOUNT_A
    );
    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE - AMOUNT_B
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
}

/// Symmetric: leg B unfunded with its mint closed, leg A funded; Reject refunds A.
#[test]
fn test_reject_recovers_funded_leg_when_other_legs_mint_closed() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);

    close_mint(&mut context, &fixture.mint_b);

    let advance = fixture.expiry - context.now() + 1;
    context.advance_clock(advance);

    assert_reject_dvp(&mut context, &fixture, &fixture.user_a);

    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.swap_dvp).is_none());
    assert!(context.get_account(&fixture.dvp_ata_a).is_none());
    assert!(context.get_account(&fixture.dvp_ata_b).is_none());
}
