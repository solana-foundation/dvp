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
/// decoder must reject it (wrong owner). The generated `from_bytes` now
/// rejects this short Borsh layout too, but it never checks the owner.
#[test]
fn checked_decode_rejects_system_owned_forgery() {
    let mut context = TestContext::new();
    let attacker = Keypair::new();
    let data = forged_swap_dvp_data(&attacker.pubkey());

    // The generated reader accepts only the 458-byte layout, so it rejects
    // this 450-byte forgery by size alone.
    assert!(SwapDvp::from_bytes(&data).is_err());

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
    let decoded = decode_swap_dvp_account(&fixture.swap_dvp, &account).unwrap();
    let dvp = decoded.base();
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

#[test]
fn discovery_skips_system_created_program_owned_accounts() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use dvp_swap_program_client::{
        programs::DVP_SWAP_PROGRAM_ID,
        verify::{fetch_swap_dvp_accounts, CONFIDENTIAL_SWAP_DVP_ACCOUNT_LEN},
    };
    use solana_client::{rpc_client::RpcClient, rpc_request::RpcRequest};

    let mut context = TestContext::new();
    let fixture = setup_dvp(&mut context, 44);
    assert_create_dvp(&mut context, &fixture);
    let real = context.get_account(&fixture.swap_dvp).unwrap();
    let rpc_account = |address: Pubkey, account: &Account| {
        serde_json::json!({
            "pubkey": address.to_string(),
            "account": {"lamports": account.lamports, "owner": account.owner.to_string(),
                "data": [STANDARD.encode(&account.data), "base64"],
                "executable": false, "rentEpoch": 0, "space": account.data.len()}
        })
    };

    // Anyone can allocate zero-filled DvP-owned accounts through System CreateAccount.
    let mut responses = vec![];
    for size in [SWAP_DVP_ACCOUNT_LEN, CONFIDENTIAL_SWAP_DVP_ACCOUNT_LEN] {
        let fake = Keypair::new();
        let ix = solana_system_interface::instruction::create_account(
            &context.payer.pubkey(),
            &fake.pubkey(),
            context.svm.minimum_balance_for_rent_exemption(size),
            size as u64,
            &DVP_SWAP_PROGRAM_ID,
        );
        crate::confidential_utils::send_v1(&mut context, &[ix], &[&fake]).unwrap();
        let account = context.get_account(&fake.pubkey()).unwrap();
        assert_eq!(account.owner, DVP_SWAP_PROGRAM_ID);
        assert!(account.data.iter().all(|byte| *byte == 0));
        let mut accounts = vec![rpc_account(fake.pubkey(), &account)];
        if size == SWAP_DVP_ACCOUNT_LEN {
            accounts.push(rpc_account(fixture.swap_dvp, &real));
        }
        responses.push((RpcRequest::GetProgramAccounts, serde_json::json!(accounts)));
    }

    // Feed actual on-chain snapshots through the public RPC discovery API.
    let rpc = RpcClient::new_mock_with_mocks_map("succeeds", responses.into_iter().collect());
    let swaps = fetch_swap_dvp_accounts(&rpc).unwrap();
    assert_eq!(swaps.len(), 1);
    assert_eq!(swaps[0].0, fixture.swap_dvp);
    assert_eq!(
        swaps[0].1,
        decode_swap_dvp_account(&fixture.swap_dvp, &real).unwrap()
    );
}

#[test]
fn discovery_preserves_rpc_errors() {
    let rpc = solana_client::rpc_client::RpcClient::new_mock("fails");
    let error = dvp_swap_program_client::verify::fetch_swap_dvp_accounts(&rpc).unwrap_err();
    assert!(error
        .downcast_ref::<solana_client::client_error::ClientError>()
        .is_some());
}
