//! Regression tests for the client-side checked readers. They enforce the
//! same invariants as the on-chain program (program ownership, exact
//! `SwapDvp::LEN` size, canonical PDA derivation) so a forged System-owned
//! account can never be treated as a live DvP before funding.

use borsh::BorshSerialize;
use dvp_swap_program_client::accounts::SwapDvp;
use dvp_swap_program_client::verify::{
    decode_swap_dvp_account, find_swap_dvp_address, find_swap_dvp_escrow_ata, SWAP_DVP_ACCOUNT_LEN,
};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::state_utils::{assert_create_dvp, setup_dvp, AMOUNT_A, AMOUNT_B};
use crate::utils::{dvp_ata, swap_dvp_pda, TestContext};

/// The System Program ID is the all-zero pubkey. An account it owns is a
/// plain (non-program-owned) account, exactly the exploit setup.
const SYSTEM_PROGRAM_ID: Pubkey = Pubkey::new_from_array([0u8; 32]);

/// Attacker-chosen SwapDvp-looking terms, Borsh-serialized. With
/// `earliest_settlement_timestamp = None` this is the 450-byte layout, 8
/// bytes shorter than `SwapDvp::LEN` because Borsh drops the None payload.
fn forged_swap_dvp_data(attacker: &Pubkey) -> Vec<u8> {
    let forged = SwapDvp {
        bump: 255,
        user_a: Pubkey::new_unique().to_bytes().into(),
        user_b: Pubkey::new_unique().to_bytes().into(),
        mint_a: Pubkey::new_unique().to_bytes().into(),
        mint_b: Pubkey::new_unique().to_bytes().into(),
        settlement_authority: attacker.to_bytes().into(),
        token_program_a: spl_token_interface::ID.to_bytes().into(),
        token_program_b: spl_token_interface::ID.to_bytes().into(),
        amount_a: 1,
        amount_b: 1_000_000_000,
        expiry_timestamp: i64::MAX / 2,
        nonce: 7,
        ref_string: [0u8; 64],
        user_a_settlement_destination: attacker.to_bytes().into(),
        user_b_settlement_destination: attacker.to_bytes().into(),
        mint_a_authority: Pubkey::new_unique().to_bytes().into(),
        mint_b_authority: Pubkey::new_unique().to_bytes().into(),
        earliest_settlement_timestamp: None,
    };
    let mut data = Vec::new();
    forged.serialize(&mut data).unwrap();
    assert_eq!(data.len(), 450, "Borsh None layout is the short variant");
    data
}

/// The exploit setup: a plain System-owned account at a keypair the
/// attacker controls, filled with SwapDvp-looking bytes. The checked
/// decoder must reject it (wrong owner), even though the legacy
/// `from_bytes` happily parses the data.
#[test]
fn checked_decode_rejects_system_owned_forgery() {
    let mut context = TestContext::new();
    let attacker = Keypair::new();
    let data = forged_swap_dvp_data(&attacker.pubkey());

    // Legacy generated reader: decodes the forgery without complaint.
    // This is the unsafe behavior integrators must not rely on.
    assert!(SwapDvp::from_bytes(&data).is_ok());

    context
        .svm
        .set_account(
            attacker.pubkey(),
            Account {
                lamports: 10_000_000,
                data: data.clone(),
                owner: SYSTEM_PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();

    let account = context.get_account(&attacker.pubkey()).unwrap();
    let err = decode_swap_dvp_account(&attacker.pubkey(), &account)
        .expect_err("System-owned forgery must be rejected");
    assert!(
        err.to_string().contains("owned"),
        "expected owner error, got: {err}"
    );
}

/// Even a forgery placed in a program-owned-looking account fails the
/// strict size check: the on-chain layout is always `SWAP_DVP_ACCOUNT_LEN`
/// bytes.
#[test]
fn strict_try_from_bytes_rejects_short_layout() {
    let attacker = Pubkey::new_unique();
    let data = forged_swap_dvp_data(&attacker);
    assert!(SwapDvp::try_from_bytes(&data).is_err());

    // Appending the trailing `earliest` sentinel (it is the last field)
    // reconstructs the full fixed on-chain layout, which parses fine.
    let mut padded = data;
    padded.extend_from_slice(&i64::MAX.to_le_bytes());
    assert_eq!(padded.len(), SWAP_DVP_ACCOUNT_LEN);
    let parsed = SwapDvp::try_from_bytes(&padded).unwrap();
    assert_eq!(parsed.earliest_settlement_timestamp, None);
}

/// A genuine account created by CreateDvp passes every check, and the
/// derivation helpers reproduce the addresses the program derived.
#[test]
fn checked_decode_accepts_real_dvp_and_derivations_match() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 42);
    assert_create_dvp(&mut context, &fixture);

    // Derivations from agreed terms must match what the program created.
    let (derived, bump) = find_swap_dvp_address(
        &fixture.settlement_authority.pubkey(),
        &fixture.user_a.pubkey(),
        &fixture.user_b.pubkey(),
        &fixture.mint_a,
        &fixture.mint_b,
        fixture.nonce,
    );
    assert_eq!(derived, fixture.swap_dvp);
    assert_eq!(
        (derived, bump),
        swap_dvp_pda(
            &fixture.settlement_authority.pubkey(),
            &fixture.user_a.pubkey(),
            &fixture.user_b.pubkey(),
            &fixture.mint_a,
            &fixture.mint_b,
            fixture.nonce,
        )
    );
    assert_eq!(
        find_swap_dvp_escrow_ata(&derived, &fixture.mint_a, &fixture.token_program_a),
        dvp_ata(&fixture.swap_dvp, &fixture.mint_a, &fixture.token_program_a)
    );

    // The checked decoder accepts the genuine account and surfaces the
    // stored terms for the funder to compare against the agreed deal.
    let account = context.get_account(&fixture.swap_dvp).unwrap();
    assert_eq!(account.data.len(), SWAP_DVP_ACCOUNT_LEN);
    let dvp = decode_swap_dvp_account(&fixture.swap_dvp, &account).unwrap();
    assert_eq!(dvp.amount_a, AMOUNT_A);
    assert_eq!(dvp.amount_b, AMOUNT_B);
    assert_eq!(dvp.bump, bump);
    assert_eq!(
        Pubkey::new_from_array(dvp.settlement_authority.to_bytes()),
        fixture.settlement_authority.pubkey()
    );
}

/// PDA verification catches a program-owned account at a non-canonical
/// address (defense in depth).
#[test]
fn checked_decode_rejects_account_at_wrong_address() {
    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 43);
    assert_create_dvp(&mut context, &fixture);

    let real = context.get_account(&fixture.swap_dvp).unwrap();
    let elsewhere = Pubkey::new_unique();
    let err = decode_swap_dvp_account(&elsewhere, &real)
        .expect_err("account at non-canonical address must be rejected");
    assert!(
        err.to_string().contains("address"),
        "expected address error, got: {err}"
    );
}
