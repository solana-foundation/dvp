//! A Squads-style smart wallet must work as a DvP party. Its on-chain
//! identity is a vault PDA that can never sign a transaction directly,
//! only relay one via CPI. These tests drive every party-signed path
//! (Reject, Reclaim, Recover) through the smart-wallet fixture and prove
//! the deposit reaches the vault, so the recovery guarantees the README
//! makes hold for custodian integrations built on smart wallets.

use dvp_swap_program_client::instructions::{
    CreateDvpBuilder, ReclaimDvpBuilder, RecoverDvpBuilder, RejectDvpBuilder,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::{
    state_utils::{AMOUNT_A, AMOUNT_B, REF_STRING},
    utils::{
        create_ata, dvp_ata, get_token_balance, nonce_tombstone_pda, set_mint, set_token_balance,
        smart_wallet_vault_pda, swap_dvp_pda, wrap_via_smart_wallet, TestContext, MEMO_PROGRAM_ID,
        TOKEN_PROGRAM_ID,
    },
};

/// A DvP whose `user_a` is a smart-wallet vault PDA and `user_b` a plain
/// keypair. Both mints are bare legacy SPL Token.
struct VaultDvp {
    vault: Pubkey,
    vault_index: u8,
    user_b: Keypair,
    settlement_authority: Keypair,
    mint_a: Pubkey,
    mint_b: Pubkey,
    swap_dvp: Pubkey,
    nonce_tombstone: Pubkey,
    vault_ata_a: Pubkey,
    user_b_ata_b: Pubkey,
    dvp_ata_a: Pubkey,
    dvp_ata_b: Pubkey,
    nonce: u64,
}

/// Build and create a DvP with the vault as `user_a`. Pre-creates the
/// vault's mint_a ATA (recover/refund destination) and user_b's mint_b
/// ATA, but funds nothing: each test injects escrow balances to model
/// the raw-transfer deposits.
fn setup_vault_dvp(context: &mut TestContext, vault_index: u8) -> VaultDvp {
    let vault = smart_wallet_vault_pda(vault_index);
    let user_b = Keypair::new();
    let settlement_authority = Keypair::new();
    let mint_a = Keypair::new().pubkey();
    let mint_b = Keypair::new().pubkey();

    set_mint(context, &mint_a, &TOKEN_PROGRAM_ID);
    set_mint(context, &mint_b, &TOKEN_PROGRAM_ID);
    context.airdrop_if_required(&settlement_authority.pubkey(), 1_000_000_000);
    // The vault holds lamports so it can receive the closed escrow's rent
    // at Recover; a Squads vault likewise carries a balance.
    context.airdrop_if_required(&vault, 1_000_000_000);

    let vault_ata_a = create_ata(context, &vault, &mint_a, &TOKEN_PROGRAM_ID);
    let user_b_ata_b = create_ata(context, &user_b.pubkey(), &mint_b, &TOKEN_PROGRAM_ID);

    let nonce = 0;
    let (swap_dvp, _) = swap_dvp_pda(
        &settlement_authority.pubkey(),
        &vault,
        &user_b.pubkey(),
        &mint_a,
        &mint_b,
        nonce,
    );
    let dvp_ata_a = dvp_ata(&swap_dvp, &mint_a, &TOKEN_PROGRAM_ID);
    let dvp_ata_b = dvp_ata(&swap_dvp, &mint_b, &TOKEN_PROGRAM_ID);

    let create = CreateDvpBuilder::new()
        .payer(context.payer.pubkey())
        .swap_dvp(swap_dvp)
        .nonce_tombstone(nonce_tombstone_pda(&swap_dvp).0)
        .mint_a(mint_a)
        .mint_b(mint_b)
        .dvp_ata_a(dvp_ata_a)
        .dvp_ata_b(dvp_ata_b)
        .token_program_a(TOKEN_PROGRAM_ID)
        .token_program_b(TOKEN_PROGRAM_ID)
        .user_a(vault)
        .user_b(user_b.pubkey())
        .settlement_authority(settlement_authority.pubkey())
        .amount_a(AMOUNT_A)
        .amount_b(AMOUNT_B)
        .expiry_timestamp(context.now() + 3600)
        .nonce(nonce)
        .ref_string(REF_STRING.to_string())
        .instruction();
    context
        .send(create, &[])
        .expect("CreateDvp with vault party");

    VaultDvp {
        vault,
        vault_index,
        user_b,
        settlement_authority,
        mint_a,
        mint_b,
        swap_dvp,
        nonce_tombstone: nonce_tombstone_pda(&swap_dvp).0,
        vault_ata_a,
        user_b_ata_b,
        dvp_ata_a,
        dvp_ata_b,
        nonce,
    }
}

/// Write `amount` into an escrow to model a raw-transfer deposit landing.
/// The escrow's token-account owner is the SwapDvp PDA.
fn fund_escrow(
    context: &mut TestContext,
    dvp: &VaultDvp,
    escrow: &Pubkey,
    mint: &Pubkey,
    amount: u64,
) {
    set_token_balance(
        context,
        escrow,
        mint,
        &dvp.swap_dvp,
        amount,
        &TOKEN_PROGRAM_ID,
    );
}

/// The vault can Reclaim its funded leg by relaying the instruction
/// through the smart wallet; funds return to the vault's own ATA and the
/// DvP stays open.
#[test]
fn test_smart_wallet_vault_reclaims_leg() {
    let mut context = TestContext::new();
    let dvp = setup_vault_dvp(&mut context, 0);
    fund_escrow(&mut context, &dvp, &dvp.dvp_ata_a, &dvp.mint_a, AMOUNT_A);

    let inner = ReclaimDvpBuilder::new()
        .signer(dvp.vault)
        .swap_dvp(dvp.swap_dvp)
        .mint(dvp.mint_a)
        .dvp_source_ata(dvp.dvp_ata_a)
        .signer_dest_ata(dvp.vault_ata_a)
        .token_program(TOKEN_PROGRAM_ID)
        .memo_program(MEMO_PROGRAM_ID)
        .instruction();
    let ix = wrap_via_smart_wallet(inner, dvp.vault_index);
    context.send(ix, &[]).expect("Reclaim via smart wallet");

    assert_eq!(get_token_balance(&context, &dvp.vault_ata_a), AMOUNT_A);
    assert_eq!(get_token_balance(&context, &dvp.dvp_ata_a), 0);
    assert!(
        context.get_account(&dvp.swap_dvp).is_some(),
        "Reclaim leaves the DvP open"
    );
}

/// The vault can Reject as a party, closing the trade and refunding both
/// funded legs (leg A back to the vault's ATA).
#[test]
fn test_smart_wallet_vault_rejects_dvp() {
    let mut context = TestContext::new();
    let dvp = setup_vault_dvp(&mut context, 0);
    fund_escrow(&mut context, &dvp, &dvp.dvp_ata_a, &dvp.mint_a, AMOUNT_A);
    fund_escrow(&mut context, &dvp, &dvp.dvp_ata_b, &dvp.mint_b, AMOUNT_B);

    let inner = RejectDvpBuilder::new()
        .signer(dvp.vault)
        .swap_dvp(dvp.swap_dvp)
        .mint_a(dvp.mint_a)
        .mint_b(dvp.mint_b)
        .dvp_ata_a(dvp.dvp_ata_a)
        .dvp_ata_b(dvp.dvp_ata_b)
        .user_a_ata_a(dvp.vault_ata_a)
        .user_b_ata_b(dvp.user_b_ata_b)
        .token_program_a(TOKEN_PROGRAM_ID)
        .token_program_b(TOKEN_PROGRAM_ID)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(0)
        .instruction();
    let ix = wrap_via_smart_wallet(inner, dvp.vault_index);
    context.send(ix, &[]).expect("Reject via smart wallet");

    assert_eq!(get_token_balance(&context, &dvp.vault_ata_a), AMOUNT_A);
    assert_eq!(get_token_balance(&context, &dvp.user_b_ata_b), AMOUNT_B);
    assert!(
        context.get_account(&dvp.swap_dvp).is_none(),
        "Reject closes the DvP"
    );
}

/// The recovery race: user_b front-runs with Reject, recreates the dead
/// escrow, and the vault's late deposit lands post-close. Recover is the
/// only path left, and the vault reaches it through the smart wallet.
#[test]
fn test_smart_wallet_vault_recovers_late_deposit() {
    let mut context = TestContext::new();
    let dvp = setup_vault_dvp(&mut context, 0);

    // The counterparty rejects before the vault's funding lands.
    let reject = RejectDvpBuilder::new()
        .signer(dvp.user_b.pubkey())
        .swap_dvp(dvp.swap_dvp)
        .mint_a(dvp.mint_a)
        .mint_b(dvp.mint_b)
        .dvp_ata_a(dvp.dvp_ata_a)
        .dvp_ata_b(dvp.dvp_ata_b)
        .user_a_ata_a(dvp.vault_ata_a)
        .user_b_ata_b(dvp.user_b_ata_b)
        .token_program_a(TOKEN_PROGRAM_ID)
        .token_program_b(TOKEN_PROGRAM_ID)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(0)
        .instruction();
    context
        .send(reject, &[&dvp.user_b])
        .expect("counterparty Reject");
    assert!(context.get_account(&dvp.swap_dvp).is_none());

    // The escrow is recreated and the vault's in-flight deposit settles
    // into it, out of reach of Reclaim (SwapDvp gone) and same-nonce
    // Create (tombstone).
    create_ata(&mut context, &dvp.swap_dvp, &dvp.mint_a, &TOKEN_PROGRAM_ID);
    fund_escrow(&mut context, &dvp, &dvp.dvp_ata_a, &dvp.mint_a, AMOUNT_A);

    let inner = RecoverDvpBuilder::new()
        .signer(dvp.vault)
        .swap_dvp(dvp.swap_dvp)
        .nonce_tombstone(dvp.nonce_tombstone)
        .mint(dvp.mint_a)
        .dvp_escrow_ata(dvp.dvp_ata_a)
        .signer_dest_ata(dvp.vault_ata_a)
        .token_program(TOKEN_PROGRAM_ID)
        .memo_program(MEMO_PROGRAM_ID)
        .settlement_authority(dvp.settlement_authority.pubkey())
        .user_a(dvp.vault)
        .user_b(dvp.user_b.pubkey())
        .mint_a(dvp.mint_a)
        .mint_b(dvp.mint_b)
        .nonce(dvp.nonce)
        .instruction();
    let ix = wrap_via_smart_wallet(inner, dvp.vault_index);
    context.send(ix, &[]).expect("Recover via smart wallet");

    assert_eq!(get_token_balance(&context, &dvp.vault_ata_a), AMOUNT_A);
    assert!(
        context.get_account(&dvp.dvp_ata_a).is_none(),
        "Recover closes the recreated escrow"
    );
}
