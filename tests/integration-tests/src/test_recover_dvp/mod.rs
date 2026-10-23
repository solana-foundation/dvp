use dvp_swap_program_client::instructions::{
    CreateDvpBuilder, ReclaimDvpBuilder, RecoverDvpBuilder,
};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_associated_token_account_interface::instruction::create_associated_token_account;
use spl_token_2022_interface::instruction::transfer_checked;

use crate::{
    state_utils::{
        assert_cancel_dvp, assert_create_dvp, assert_fund_a, assert_fund_b, assert_reject_dvp,
        assert_settle_dvp, setup_dvp, setup_dvp_with_programs, DvpFixture, AMOUNT_A, AMOUNT_B,
        INITIAL_BALANCE, REF_STRING,
    },
    utils::{
        assert_instruction_error, assert_program_error, get_token_balance, set_mint,
        set_token_balance, TestContext, DVP_NEVER_CREATED, DVP_STILL_OPEN, MEMO_PROGRAM_ID,
        NONCE_ALREADY_USED, RECIPIENT_ATA_MISMATCH, SIGNER_NOT_PARTY, TOKEN_2022_PROGRAM_ID,
        TOKEN_PROGRAM_ID,
    },
};

/// The fixture leg's token program for `mint`. Falls back to leg A's
/// program for a mint that is on neither leg.
fn leg_token_program(fixture: &DvpFixture, mint: &Pubkey) -> Pubkey {
    if *mint == fixture.mint_b {
        fixture.token_program_b
    } else {
        fixture.token_program_a
    }
}

/// RecoverDvp for a fixture DvP. `signer` selects the leg; `mint`,
/// `escrow`, and `dest` must match it.
fn recover_ix(
    fixture: &DvpFixture,
    signer: &Pubkey,
    mint: &Pubkey,
    escrow: &Pubkey,
    dest: &Pubkey,
) -> Instruction {
    RecoverDvpBuilder::new()
        .signer(*signer)
        .swap_dvp(fixture.swap_dvp)
        .nonce_tombstone(fixture.nonce_tombstone)
        .mint(*mint)
        .dvp_escrow_ata(*escrow)
        .signer_dest_ata(*dest)
        .token_program(leg_token_program(fixture, mint))
        .memo_program(MEMO_PROGRAM_ID)
        .settlement_authority(fixture.settlement_authority.pubkey())
        .user_a(fixture.user_a.pubkey())
        .user_b(fixture.user_b.pubkey())
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .nonce(fixture.nonce)
        .instruction()
}

/// Recreate a closed escrow ATA for the dead SwapDvp wallet, paid by
/// `payer`. Third parties can always do this; it is the attack's
/// enabling step.
fn recreate_escrow(
    context: &mut TestContext,
    payer: &Keypair,
    fixture: &DvpFixture,
    mint: &Pubkey,
) {
    context.airdrop_if_required(&payer.pubkey(), 1_000_000_000);
    let ix = create_associated_token_account(
        &payer.pubkey(),
        &fixture.swap_dvp,
        mint,
        &leg_token_program(fixture, mint),
    );
    context.send(ix, &[payer]).expect("recreate escrow ATA");
}

/// Same as reclaim: Recover's drain to the signer's own ATA is
/// authenticated by contents, so an owner-reassigned canonical ATA reverts
/// instead of delivering the recovered deposit to the new owner.
#[test]
fn test_recover_dvp_rejects_owner_reassigned_dest_ata() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);

    // Close the trade, then a late deposit lands in a recreated escrow.
    assert_reject_dvp(&mut context, &fixture, &fixture.user_a);
    recreate_escrow(&mut context, &fixture.user_a, &fixture, &fixture.mint_a);
    assert_fund_a(&mut context, &fixture);

    // user_a's canonical dest ATA is owner-reassigned to an attacker.
    let attacker = Keypair::new().pubkey();
    set_token_balance(
        &mut context,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &attacker,
        0,
        &fixture.token_program_a,
    );

    let ix = recover_ix(
        &fixture,
        &fixture.user_a.pubkey(),
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a_ata_a,
    );
    assert_program_error(context.send(ix, &[&fixture.user_a]), RECIPIENT_ATA_MISMATCH);
}

/// End-to-end funding-race reproduction. user_b front-runs user_a's
/// funding transfer with RejectDvp, recreates the dead escrow ATA, and
/// the already-signed transfer lands in it. Reclaim and same-nonce
/// Create are both dead ends; RecoverDvp must return the deposit.
#[test]
fn test_recover_dvp_after_reject_front_run() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);

    // The attacker (user_b) rejects before the victim's funding lands.
    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);
    assert!(context.get_account(&fixture.swap_dvp).is_none());
    assert!(context.get_account(&fixture.dvp_ata_a).is_none());

    // The attacker recreates the asset escrow for the dead wallet, and
    // the victim's in-flight transfer settles into it.
    recreate_escrow(&mut context, &fixture.user_b, &fixture, &fixture.mint_a);
    assert_fund_a(&mut context, &fixture);
    assert_eq!(get_token_balance(&context, &fixture.dvp_ata_a), AMOUNT_A);

    // ReclaimDvp needs the program-owned SwapDvp, which is gone.
    let reclaim = ReclaimDvpBuilder::new()
        .signer(fixture.user_a.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint(fixture.mint_a)
        .dvp_source_ata(fixture.dvp_ata_a)
        .signer_dest_ata(fixture.user_a_ata_a)
        .token_program(fixture.token_program_a)
        .memo_program(MEMO_PROGRAM_ID)
        .instruction();
    assert_instruction_error(
        context.send(reclaim, &[&fixture.user_a]),
        "InvalidAccountOwner",
    );

    // Re-creating the DvP state is blocked by the nonce tombstone.
    let recreate = CreateDvpBuilder::new()
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
        .expiry_timestamp(context.now() + 3600)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();
    assert_program_error(context.send(recreate, &[]), NONCE_ALREADY_USED);

    // RecoverDvp returns the deposit and closes the recreated escrow,
    // sweeping its rent (paid by the attacker) to the victim.
    let user_a_lamports_before = context
        .get_account(&fixture.user_a.pubkey())
        .map(|a| a.lamports)
        .unwrap_or(0);
    let ix = recover_ix(
        &fixture,
        &fixture.user_a.pubkey(),
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a_ata_a,
    );
    context.send(ix, &[&fixture.user_a]).expect("RecoverDvp");

    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.dvp_ata_a).is_none());
    let user_a_lamports_after = context
        .get_account(&fixture.user_a.pubkey())
        .map(|a| a.lamports)
        .unwrap_or(0);
    assert!(user_a_lamports_after > user_a_lamports_before);
}

/// Same shape on the cash leg after a Cancel-close: user_b recovers a
/// deposit that landed late in a recreated mint_b escrow.
#[test]
fn test_recover_dvp_leg_b_after_cancel() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);
    assert_cancel_dvp(&mut context, &fixture);

    let outsider = Keypair::new();
    recreate_escrow(&mut context, &outsider, &fixture, &fixture.mint_b);
    let fund = transfer_checked(
        &fixture.token_program_b,
        &fixture.user_b_ata_b,
        &fixture.mint_b,
        &fixture.dvp_ata_b,
        &fixture.user_b.pubkey(),
        &[],
        AMOUNT_B,
        6,
    )
    .expect("build TransferChecked");
    context.send(fund, &[&fixture.user_b]).expect("late fund B");

    let ix = recover_ix(
        &fixture,
        &fixture.user_b.pubkey(),
        &fixture.mint_b,
        &fixture.dvp_ata_b,
        &fixture.user_b_ata_b,
    );
    context.send(ix, &[&fixture.user_b]).expect("RecoverDvp");

    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.dvp_ata_b).is_none());
}

/// Late deposits after Settle are recoverable too: Settle closes the
/// trade like Reject/Cancel do.
#[test]
fn test_recover_dvp_after_settle() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);
    assert_settle_dvp(&mut context, &fixture);
    assert!(context.get_account(&fixture.swap_dvp).is_none());

    let outsider = Keypair::new();
    recreate_escrow(&mut context, &outsider, &fixture, &fixture.mint_a);
    let late_amount = 1_000;
    let fund = transfer_checked(
        &fixture.token_program_a,
        &fixture.user_a_ata_a,
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a.pubkey(),
        &[],
        late_amount,
        6,
    )
    .expect("build TransferChecked");
    context.send(fund, &[&fixture.user_a]).expect("late fund A");

    let before = get_token_balance(&context, &fixture.user_a_ata_a);
    let ix = recover_ix(
        &fixture,
        &fixture.user_a.pubkey(),
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a_ata_a,
    );
    context.send(ix, &[&fixture.user_a]).expect("RecoverDvp");

    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        before + late_amount
    );
    assert!(context.get_account(&fixture.dvp_ata_a).is_none());
}

/// A recreated escrow with no deposit is still closed, so the trap is
/// removed and its rent goes to the depositor.
#[test]
fn test_recover_dvp_closes_empty_recreated_escrow() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);
    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);

    recreate_escrow(&mut context, &fixture.user_b, &fixture, &fixture.mint_a);

    let ix = recover_ix(
        &fixture,
        &fixture.user_a.pubkey(),
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a_ata_a,
    );
    context.send(ix, &[&fixture.user_a]).expect("RecoverDvp");

    assert!(context.get_account(&fixture.dvp_ata_a).is_none());
    assert_eq!(
        get_token_balance(&context, &fixture.user_a_ata_a),
        INITIAL_BALANCE
    );
}

/// The tombstone is never closed, so recovery keeps working if the
/// escrow is recreated and funded again after a first recovery.
#[test]
fn test_recover_dvp_is_repeatable() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);
    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);

    for _ in 0..2 {
        recreate_escrow(&mut context, &fixture.user_b, &fixture, &fixture.mint_a);
        assert_fund_a(&mut context, &fixture);

        let ix = recover_ix(
            &fixture,
            &fixture.user_a.pubkey(),
            &fixture.mint_a,
            &fixture.dvp_ata_a,
            &fixture.user_a_ata_a,
        );
        context.send(ix, &[&fixture.user_a]).expect("RecoverDvp");
        assert_eq!(
            get_token_balance(&context, &fixture.user_a_ata_a),
            INITIAL_BALANCE
        );
        assert!(context.get_account(&fixture.dvp_ata_a).is_none());
    }
}

/// Mixed-program DvP with leg B on Token-2022: recovery binds the token
/// program to the leg's mint, not to leg A's program.
#[test]
fn test_recover_dvp_token_2022_leg() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(&mut context, 0, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID);
    assert_create_dvp(&mut context, &fixture);
    assert_reject_dvp(&mut context, &fixture, &fixture.user_a);

    let outsider = Keypair::new();
    recreate_escrow(&mut context, &outsider, &fixture, &fixture.mint_b);
    let fund = transfer_checked(
        &fixture.token_program_b,
        &fixture.user_b_ata_b,
        &fixture.mint_b,
        &fixture.dvp_ata_b,
        &fixture.user_b.pubkey(),
        &[],
        AMOUNT_B,
        6,
    )
    .expect("build TransferChecked");
    context.send(fund, &[&fixture.user_b]).expect("late fund B");

    let ix = recover_ix(
        &fixture,
        &fixture.user_b.pubkey(),
        &fixture.mint_b,
        &fixture.dvp_ata_b,
        &fixture.user_b_ata_b,
    );
    context.send(ix, &[&fixture.user_b]).expect("RecoverDvp");

    assert_eq!(
        get_token_balance(&context, &fixture.user_b_ata_b),
        INITIAL_BALANCE
    );
    assert!(context.get_account(&fixture.dvp_ata_b).is_none());
}

/// Recovery binds the token program to the escrow, but the pinned
/// Token-2022 binary also checks the mint owner during TransferChecked.
/// An injected cross-program recreation therefore blocks recovery.
#[test]
fn test_recover_dvp_rejects_mint_recreated_under_other_token_program() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    assert_create_dvp(&mut context, &fixture);
    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);

    // Late-deposit setup: recreate the dead T22 escrow and land the
    // victim's in-flight transfer in it.
    recreate_escrow(&mut context, &fixture.user_b, &fixture, &fixture.mint_a);
    assert_fund_a(&mut context, &fixture);
    assert_eq!(get_token_balance(&context, &fixture.dvp_ata_a), AMOUNT_A);

    // Inject the owner change to exercise recovery against adversarial
    // state; this fixture does not establish that recreation is reachable.
    set_mint(&mut context, &fixture.mint_a, &TOKEN_PROGRAM_ID);

    let escrow_before = context.get_account(&fixture.dvp_ata_a).unwrap();
    let destination_before = context.get_account(&fixture.user_a_ata_a).unwrap();
    let ix = recover_ix(
        &fixture,
        &fixture.user_a.pubkey(),
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a_ata_a,
    );
    assert_instruction_error(context.send(ix, &[&fixture.user_a]), "IncorrectProgramId");
    assert_eq!(
        context.get_account(&fixture.dvp_ata_a).unwrap(),
        escrow_before
    );
    assert_eq!(
        context.get_account(&fixture.user_a_ata_a).unwrap(),
        destination_before
    );
}

/// The escrow/token-program pair must be consistent: passing the
/// drifted mint's current owner with the T22 escrow derives a different
/// ATA address and is rejected, so recovery can't silently switch
/// namespaces.
#[test]
fn test_recover_dvp_rejects_token_program_not_matching_escrow() {
    let mut context = TestContext::new();
    let fixture = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    assert_create_dvp(&mut context, &fixture);
    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);

    recreate_escrow(&mut context, &fixture.user_b, &fixture, &fixture.mint_a);
    assert_fund_a(&mut context, &fixture);
    set_mint(&mut context, &fixture.mint_a, &TOKEN_PROGRAM_ID);

    let ix = RecoverDvpBuilder::new()
        .signer(fixture.user_a.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .nonce_tombstone(fixture.nonce_tombstone)
        .mint(fixture.mint_a)
        .dvp_escrow_ata(fixture.dvp_ata_a)
        .signer_dest_ata(fixture.user_a_ata_a)
        .token_program(TOKEN_PROGRAM_ID)
        .memo_program(MEMO_PROGRAM_ID)
        .settlement_authority(fixture.settlement_authority.pubkey())
        .user_a(fixture.user_a.pubkey())
        .user_b(fixture.user_b.pubkey())
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .nonce(fixture.nonce)
        .instruction();
    assert!(
        context.send(ix, &[&fixture.user_a]).is_err(),
        "legacy program with the T22 escrow must be rejected"
    );
}

/// While the SwapDvp is live, ReclaimDvp is the recovery path and
/// RecoverDvp must refuse to run.
#[test]
fn test_recover_dvp_rejects_open_dvp() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);

    let ix = recover_ix(
        &fixture,
        &fixture.user_a.pubkey(),
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a_ata_a,
    );
    assert_program_error(context.send(ix, &[&fixture.user_a]), DVP_STILL_OPEN);
}

/// Seeds that never had a DvP derive an address with no tombstone.
/// Without this check anyone could invent seeds naming themselves a
/// party and sign arbitrary PDA-owned token accounts.
#[test]
fn test_recover_dvp_rejects_seeds_never_created() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);
    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);

    // Same parties and mints, different nonce: different PDA, no
    // tombstone. The swap_dvp/tombstone/escrow accounts must match the
    // fabricated nonce's derivation to reach the tombstone check.
    let fake_nonce: u64 = 99;
    let (fake_swap_dvp, _) = crate::utils::swap_dvp_pda(
        &fixture.settlement_authority.pubkey(),
        &fixture.user_a.pubkey(),
        &fixture.user_b.pubkey(),
        &fixture.mint_a,
        &fixture.mint_b,
        fake_nonce,
    );
    let fake_tombstone = crate::utils::nonce_tombstone_pda(&fake_swap_dvp).0;
    let fake_escrow =
        crate::utils::dvp_ata(&fake_swap_dvp, &fixture.mint_a, &fixture.token_program_a);

    let ix = RecoverDvpBuilder::new()
        .signer(fixture.user_a.pubkey())
        .swap_dvp(fake_swap_dvp)
        .nonce_tombstone(fake_tombstone)
        .mint(fixture.mint_a)
        .dvp_escrow_ata(fake_escrow)
        .signer_dest_ata(fixture.user_a_ata_a)
        .token_program(fixture.token_program_a)
        .memo_program(MEMO_PROGRAM_ID)
        .settlement_authority(fixture.settlement_authority.pubkey())
        .user_a(fixture.user_a.pubkey())
        .user_b(fixture.user_b.pubkey())
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .nonce(fake_nonce)
        .instruction();
    assert_program_error(context.send(ix, &[&fixture.user_a]), DVP_NEVER_CREATED);
}

/// A third party signing with the real seeds is not a party.
#[test]
fn test_recover_dvp_rejects_third_party() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);
    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);
    recreate_escrow(&mut context, &fixture.user_b, &fixture, &fixture.mint_a);
    assert_fund_a(&mut context, &fixture);

    let outsider = Keypair::new();
    context.airdrop_if_required(&outsider.pubkey(), 1_000_000_000);
    let ix = recover_ix(
        &fixture,
        &outsider.pubkey(),
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_a_ata_a,
    );
    assert_program_error(context.send(ix, &[&outsider]), SIGNER_NOT_PARTY);
}

/// The counterparty cannot sweep the victim's leg: user_b's signature
/// selects the mint_b leg, so passing the mint_a escrow fails.
#[test]
fn test_recover_dvp_rejects_counterparty_taking_other_leg() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);
    assert_reject_dvp(&mut context, &fixture, &fixture.user_b);
    recreate_escrow(&mut context, &fixture.user_b, &fixture, &fixture.mint_a);
    assert_fund_a(&mut context, &fixture);

    let ix = recover_ix(
        &fixture,
        &fixture.user_b.pubkey(),
        &fixture.mint_a,
        &fixture.dvp_ata_a,
        &fixture.user_b_ata_a,
    );
    assert_instruction_error(context.send(ix, &[&fixture.user_b]), "InvalidAccountData");
}
