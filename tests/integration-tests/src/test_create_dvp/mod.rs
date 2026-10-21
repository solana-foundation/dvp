use dvp_swap_program_client::{accounts::SwapDvp, instructions::CreateDvpBuilder};
use litesvm::types::TransactionMetadata;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spl_associated_token_account_interface::instruction::create_associated_token_account;

use crate::{
    state_utils::{
        assert_cancel_dvp, assert_create_dvp, setup_dvp, AMOUNT_A, AMOUNT_B, REF_STRING,
    },
    utils::{
        assert_program_error, dvp_ata, get_token_balance, nonce_tombstone_pda, set_mint,
        set_native_mint, set_token_multisig, swap_dvp_pda, TestContext, EARLIEST_AFTER_EXPIRY,
        ESCROW_PRELOADED_WITH_LAMPORTS, EXPIRY_NOT_IN_FUTURE, EXPIRY_TOO_FAR_IN_FUTURE,
        NATIVE_MINT, NONCE_ALREADY_USED, PARTY_NOT_SIGNER_CAPABLE, REF_STRING_TOO_LONG, SAME_MINT,
        SELF_DVP, SETTLEMENT_AUTHORITY_EXECUTABLE, SETTLEMENT_AUTHORITY_IS_PARTY,
        SETTLEMENT_DESTINATION_IS_SWAP_DVP, SWAP_DVP_PRELOADED_WITH_LAMPORTS, SWAP_PROGRAM_ID,
        TOKEN_PROGRAM_ID, ZERO_AMOUNT,
    },
};

#[test]
fn test_create_dvp_success() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    assert_create_dvp(&mut context, &fixture);

    assert!(
        context.get_account(&fixture.swap_dvp).is_some(),
        "SwapDvp PDA must exist"
    );
    assert!(
        context.get_account(&fixture.dvp_ata_a).is_some(),
        "dvp_ata_a must exist"
    );
    assert!(
        context.get_account(&fixture.dvp_ata_b).is_some(),
        "dvp_ata_b must exist"
    );
    assert_eq!(get_token_balance(&context, &fixture.dvp_ata_a), 0);
    assert_eq!(get_token_balance(&context, &fixture.dvp_ata_b), 0);
}

// `validate_args` runs before PDA derivation in the processor, so the
// error tests below can reuse the fixture's PDA + escrow ATAs even when
// the args they pass would derive a different PDA. Only the offending
// arg differs from `assert_create_dvp`.

#[test]
fn test_create_dvp_rejects_expiry_at_now() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    let now = context.now();

    let ix = CreateDvpBuilder::new()
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
        .expiry_timestamp(now)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();

    assert_program_error(context.send(ix, &[]), EXPIRY_NOT_IN_FUTURE);
}

#[test]
fn test_create_dvp_rejects_expiry_too_far_in_future() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    // One year + 1s past now exceeds the MAX_DVP_DURATION_SECS cap.
    let one_year_plus_one = context.now() + 365 * 24 * 60 * 60 + 1;
    let ix = CreateDvpBuilder::new()
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
        .expiry_timestamp(one_year_plus_one)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();

    assert_program_error(context.send(ix, &[]), EXPIRY_TOO_FAR_IN_FUTURE);
}

#[test]
fn test_create_dvp_rejects_earliest_after_expiry() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let ix = CreateDvpBuilder::new()
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
        .earliest_settlement_timestamp(fixture.expiry + 1)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();

    assert_program_error(context.send(ix, &[]), EARLIEST_AFTER_EXPIRY);
}

#[test]
fn test_create_dvp_rejects_self_dvp() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let ix = CreateDvpBuilder::new()
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
        .user_b(fixture.user_a.pubkey())
        .settlement_authority(fixture.settlement_authority.pubkey())
        .amount_a(AMOUNT_A)
        .amount_b(AMOUNT_B)
        .expiry_timestamp(fixture.expiry)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();

    assert_program_error(context.send(ix, &[]), SELF_DVP);
}

#[test]
fn test_create_dvp_rejects_same_mint() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let ix = CreateDvpBuilder::new()
        .payer(context.payer.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .nonce_tombstone(fixture.nonce_tombstone)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_a)
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
        .instruction();

    assert_program_error(context.send(ix, &[]), SAME_MINT);
}

#[test]
fn test_create_dvp_rejects_zero_amount_a() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let ix = CreateDvpBuilder::new()
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
        .amount_a(0)
        .amount_b(AMOUNT_B)
        .expiry_timestamp(fixture.expiry)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();

    assert_program_error(context.send(ix, &[]), ZERO_AMOUNT);
}

#[test]
fn test_create_dvp_rejects_zero_amount_b() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let ix = CreateDvpBuilder::new()
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
        .amount_b(0)
        .expiry_timestamp(fixture.expiry)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();

    assert_program_error(context.send(ix, &[]), ZERO_AMOUNT);
}

#[test]
fn test_create_dvp_rejects_executable_settlement_authority() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    // Point settlement_authority at an executable account (the program
    // under test). An executable can't be credited the closed-account rent
    // at Settle/Cancel, so CreateDvp must reject it up front.
    let ix = CreateDvpBuilder::new()
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
        .settlement_authority(SWAP_PROGRAM_ID)
        .amount_a(AMOUNT_A)
        .amount_b(AMOUNT_B)
        .expiry_timestamp(fixture.expiry)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();

    assert_program_error(context.send(ix, &[]), SETTLEMENT_AUTHORITY_EXECUTABLE);
}

#[test]
fn test_create_dvp_rejects_settlement_authority_as_party() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    // settlement_authority must be a neutral third party. Pointing it at
    // either swap counterparty would let a party settle its own trade, so
    // CreateDvp must reject both user_a and user_b up front.
    for party in [fixture.user_a.pubkey(), fixture.user_b.pubkey()] {
        let ix = CreateDvpBuilder::new()
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
            .settlement_authority(party)
            .amount_a(AMOUNT_A)
            .amount_b(AMOUNT_B)
            .expiry_timestamp(fixture.expiry)
            .nonce(fixture.nonce)
            .ref_string(REF_STRING.to_string())
            .instruction();

        assert_program_error(context.send(ix, &[]), SETTLEMENT_AUTHORITY_IS_PARTY);
    }
}

/// Once a DvP is closed, its `(seeds, nonce)` PDA address can never be
/// re-instantiated: the nonce tombstone outlives the trade. This blocks
/// the stale-deposit capture attack — an attacker can't recreate the
/// same address with predatory terms to drain a deposit the victim
/// queued against the old escrow.
#[test]
fn test_create_dvp_rejects_reused_nonce_after_close() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &fixture);

    // Close the trade: the SwapDvp PDA and escrows go away, but the
    // nonce tombstone remains.
    assert_cancel_dvp(&mut context, &fixture);
    assert!(context.get_account(&fixture.swap_dvp).is_none());

    // Re-creating the same nonce — even with predatory terms — must fail.
    let ix = CreateDvpBuilder::new()
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
        .amount_a(1)
        .amount_b(AMOUNT_B)
        .expiry_timestamp(fixture.expiry)
        .nonce(fixture.nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();

    assert_program_error(context.send(ix, &[]), NONCE_ALREADY_USED);
}

/// A front-runner pre-creates the canonical asset escrow ATA before
/// CreateDvp lands. CreateDvp must accept the existing account
/// (idempotent path) instead of bricking the trade.
#[test]
fn test_create_dvp_succeeds_when_escrow_ata_was_pre_created() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let frontrunner = Keypair::new();
    context.airdrop_if_required(&frontrunner.pubkey(), 1_000_000_000);
    let pre_create_ix = create_associated_token_account(
        &frontrunner.pubkey(),
        &fixture.swap_dvp,
        &fixture.mint_a,
        &fixture.token_program_a,
    );
    context
        .send(pre_create_ix, &[&frontrunner])
        .expect("front-runner pre-creates dvp_ata_a");
    assert!(
        context.get_account(&fixture.dvp_ata_a).is_some(),
        "dvp_ata_a must exist after front-run"
    );

    assert_create_dvp(&mut context, &fixture);

    assert!(context.get_account(&fixture.swap_dvp).is_some());
    assert!(context.get_account(&fixture.dvp_ata_b).is_some());
    assert_eq!(get_token_balance(&context, &fixture.dvp_ata_a), 0);
    assert_eq!(get_token_balance(&context, &fixture.dvp_ata_b), 0);
}

/// A non-native escrow holding raw SOL above its rent minimum
/// must not be adopted. The close paths sweep the escrow's full lamport
/// balance to the closer, so a party could Reject and pocket the preload.
#[test]
fn test_create_dvp_rejects_escrow_preloaded_with_sol() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let frontrunner = Keypair::new();
    context.airdrop_if_required(&frontrunner.pubkey(), 1_000_000_000);
    let pre_create_ix = create_associated_token_account(
        &frontrunner.pubkey(),
        &fixture.swap_dvp,
        &fixture.mint_a,
        &fixture.token_program_a,
    );
    context
        .send(pre_create_ix, &[&frontrunner])
        .expect("pre-create dvp_ata_a");

    // The victim's mis-funding: raw SOL onto the non-native escrow.
    context
        .svm
        .airdrop(&fixture.dvp_ata_a, 1_000_000_000)
        .expect("preload SOL");

    let ix = CreateDvpBuilder::new()
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
        .instruction();

    assert_program_error(context.send(ix, &[]), ESCROW_PRELOADED_WITH_LAMPORTS);
}

/// Preloaded lamports on a WSOL escrow are the deposit mechanism
/// (SyncNative adopts them as token balance), so CreateDvp accepts them.
#[test]
fn test_create_dvp_accepts_preloaded_wsol_escrow() {
    let mut context = TestContext::new();
    set_native_mint(&mut context);

    let user_a = Keypair::new();
    let user_b = Keypair::new();
    let settlement_authority = Keypair::new();

    let mint_a = NATIVE_MINT; // WSOL leg
    let mint_b = Keypair::new().pubkey();
    set_mint(&mut context, &mint_b, &TOKEN_PROGRAM_ID);

    let nonce: u64 = 0;
    let (swap_dvp, _) = swap_dvp_pda(
        &settlement_authority.pubkey(),
        &user_a.pubkey(),
        &user_b.pubkey(),
        &mint_a,
        &mint_b,
        nonce,
    );
    let dvp_ata_a = dvp_ata(&swap_dvp, &mint_a, &TOKEN_PROGRAM_ID);
    let dvp_ata_b = dvp_ata(&swap_dvp, &mint_b, &TOKEN_PROGRAM_ID);

    // Pre-create the WSOL escrow and preload the deposit as raw lamports.
    let pre_create_ix = create_associated_token_account(
        &context.payer.pubkey(),
        &swap_dvp,
        &mint_a,
        &TOKEN_PROGRAM_ID,
    );
    context
        .send(pre_create_ix, &[])
        .expect("pre-create WSOL escrow");
    let deposit: u64 = 5_000_000;
    context
        .svm
        .airdrop(&dvp_ata_a, deposit)
        .expect("preload WSOL deposit");

    let ix = CreateDvpBuilder::new()
        .payer(context.payer.pubkey())
        .swap_dvp(swap_dvp)
        .nonce_tombstone(nonce_tombstone_pda(&swap_dvp).0)
        .mint_a(mint_a)
        .mint_b(mint_b)
        .dvp_ata_a(dvp_ata_a)
        .dvp_ata_b(dvp_ata_b)
        .token_program_a(TOKEN_PROGRAM_ID)
        .token_program_b(TOKEN_PROGRAM_ID)
        .user_a(user_a.pubkey())
        .user_b(user_b.pubkey())
        .settlement_authority(settlement_authority.pubkey())
        .amount_a(deposit)
        .amount_b(AMOUNT_B)
        .expiry_timestamp(context.now() + 3600)
        .nonce(nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();
    context
        .send(ix, &[])
        .expect("CreateDvp with preloaded WSOL escrow");

    assert!(context.get_account(&swap_dvp).is_some());
}

/// Raw SOL preloaded onto the future swap_dvp address must not
/// be adopted. The terminal close paths sweep the PDA's full balance
/// to the closer, so a party could Reject and pocket the preload.
#[test]
fn test_create_dvp_rejects_swap_dvp_preloaded_with_sol() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    context
        .svm
        .airdrop(&fixture.swap_dvp, 1_000_000_000)
        .expect("preload SOL onto future PDA");

    let ix = CreateDvpBuilder::new()
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
        .instruction();

    assert_program_error(context.send(ix, &[]), SWAP_DVP_PRELOADED_WITH_LAMPORTS);
}

/// A preload at or below the rent reserve is harmless (the payer tops
/// up to exactly the reserve), so it must not block creation. The preload
/// itself must cover rent for a zero-data system account.
#[test]
fn test_create_dvp_accepts_swap_dvp_preloaded_below_rent_reserve() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let preload = context.svm.minimum_balance_for_rent_exemption(0);
    context
        .svm
        .airdrop(&fixture.swap_dvp, preload)
        .expect("dust the future PDA");

    assert_create_dvp(&mut context, &fixture);
    assert!(context.get_account(&fixture.swap_dvp).is_some());
}

/// A settlement destination equal to the SwapDvp PDA would make
/// the delivery ATA the escrow itself, so Settle's transfer becomes a
/// self-transfer no-op. On a WSOL leg the close would then pay the
/// undelivered leg to settlement_authority.
#[test]
fn test_create_dvp_rejects_user_a_destination_equal_to_swap_dvp() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let ix = CreateDvpBuilder::new()
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
        .user_a_settlement_destination(fixture.swap_dvp)
        .instruction();

    assert_program_error(context.send(ix, &[]), SETTLEMENT_DESTINATION_IS_SWAP_DVP);
}

#[test]
fn test_create_dvp_rejects_user_b_destination_equal_to_swap_dvp() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let ix = CreateDvpBuilder::new()
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
        .user_b_settlement_destination(fixture.swap_dvp)
        .instruction();

    assert_program_error(context.send(ix, &[]), SETTLEMENT_DESTINATION_IS_SWAP_DVP);
}

/// The ref string is stored zero-padded, and the account decodes with
/// the generated Borsh client — the only check that the hand-rolled
/// on-chain serialization and the IDL-derived client layout agree.
#[test]
fn test_create_dvp_stores_ref_string() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    assert_create_dvp(&mut context, &fixture);

    let account = context.get_account(&fixture.swap_dvp).expect("SwapDvp");
    let dvp = SwapDvp::from_bytes(&account.data).expect("client must decode the account");

    let mut expected = [0u8; 64];
    expected[..REF_STRING.len()].copy_from_slice(REF_STRING.as_bytes());
    assert_eq!(dvp.ref_string, expected);
    assert_eq!(dvp.amount_a, AMOUNT_A);
    assert_eq!(dvp.amount_b, AMOUNT_B);
    assert_eq!(dvp.earliest_settlement_timestamp, None);
    // Destinations were not provided, so they resolved to the users.
    assert_eq!(dvp.user_a_settlement_destination, fixture.user_a.pubkey());
    assert_eq!(dvp.user_b_settlement_destination, fixture.user_b.pubkey());
}

/// `ref_string` is optional: omitting it stores all zeros.
#[test]
fn test_create_dvp_without_ref_string_stores_zeros() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let ix = CreateDvpBuilder::new()
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
        .instruction();
    context.send(ix, &[]).expect("CreateDvp without ref_string");

    let account = context.get_account(&fixture.swap_dvp).expect("SwapDvp");
    let dvp = SwapDvp::from_bytes(&account.data).expect("client must decode the account");
    assert_eq!(dvp.ref_string, [0u8; 64]);
}

#[test]
fn test_create_dvp_accepts_max_len_ref_string() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let max_ref = "R".repeat(64);
    let ix = CreateDvpBuilder::new()
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
        .ref_string(max_ref.clone())
        .instruction();
    context.send(ix, &[]).expect("CreateDvp with 64-byte ref");

    let account = context.get_account(&fixture.swap_dvp).expect("SwapDvp");
    let dvp = SwapDvp::from_bytes(&account.data).expect("client must decode the account");
    assert_eq!(dvp.ref_string, max_ref.as_bytes());
}

/// Build and send CreateDvp with `user_a` set to `party_a`, deriving the
/// PDA and escrows from it so the instruction is internally coherent.
/// user_b, the settlement authority, and both mints are fresh.
fn create_with_party_a(
    context: &mut TestContext,
    party_a: solana_pubkey::Pubkey,
) -> Result<TransactionMetadata, String> {
    let user_b = Keypair::new();
    let settlement_authority = Keypair::new();
    let mint_a = Keypair::new().pubkey();
    let mint_b = Keypair::new().pubkey();
    set_mint(context, &mint_a, &TOKEN_PROGRAM_ID);
    set_mint(context, &mint_b, &TOKEN_PROGRAM_ID);
    context.airdrop_if_required(&settlement_authority.pubkey(), 1_000_000_000);

    let nonce = 0;
    let (swap_dvp, _) = swap_dvp_pda(
        &settlement_authority.pubkey(),
        &party_a,
        &user_b.pubkey(),
        &mint_a,
        &mint_b,
        nonce,
    );
    let ix = CreateDvpBuilder::new()
        .payer(context.payer.pubkey())
        .swap_dvp(swap_dvp)
        .nonce_tombstone(nonce_tombstone_pda(&swap_dvp).0)
        .mint_a(mint_a)
        .mint_b(mint_b)
        .dvp_ata_a(dvp_ata(&swap_dvp, &mint_a, &TOKEN_PROGRAM_ID))
        .dvp_ata_b(dvp_ata(&swap_dvp, &mint_b, &TOKEN_PROGRAM_ID))
        .token_program_a(TOKEN_PROGRAM_ID)
        .token_program_b(TOKEN_PROGRAM_ID)
        .user_a(party_a)
        .user_b(user_b.pubkey())
        .settlement_authority(settlement_authority.pubkey())
        .amount_a(AMOUNT_A)
        .amount_b(AMOUNT_B)
        .expiry_timestamp(context.now() + 3600)
        .nonce(nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();
    context.send(ix, &[])
}

/// An SPL Token multisig can control a funding account but can never sign
/// a transaction, so its late deposits could never be recovered. Reject
/// it as a party at creation.
#[test]
fn test_create_dvp_rejects_token_multisig_party() {
    let mut context = TestContext::new();
    let multisig = Keypair::new().pubkey();
    set_token_multisig(&mut context, &multisig, &TOKEN_PROGRAM_ID);

    let result = create_with_party_a(&mut context, multisig);
    assert_program_error(result, PARTY_NOT_SIGNER_CAPABLE);
}

/// An executable account can never sign, so it is not a valid party.
#[test]
fn test_create_dvp_rejects_executable_party() {
    let mut context = TestContext::new();
    let result = create_with_party_a(&mut context, SWAP_PROGRAM_ID);
    assert_program_error(result, PARTY_NOT_SIGNER_CAPABLE);
}

/// A mint (owned by a token program) is not a wallet identity and can
/// never sign, so it is rejected as a party.
#[test]
fn test_create_dvp_rejects_mint_party() {
    let mut context = TestContext::new();
    let mint_party = Keypair::new().pubkey();
    set_mint(&mut context, &mint_party, &TOKEN_PROGRAM_ID);

    let result = create_with_party_a(&mut context, mint_party);
    assert_program_error(result, PARTY_NOT_SIGNER_CAPABLE);
}

#[test]
fn test_create_dvp_rejects_ref_string_over_max_len() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 0);

    let ix = CreateDvpBuilder::new()
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
        .ref_string("R".repeat(65))
        .instruction();

    assert_program_error(context.send(ix, &[]), REF_STRING_TOO_LONG);
}
