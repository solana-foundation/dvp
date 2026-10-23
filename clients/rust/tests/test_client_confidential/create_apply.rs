use crate::confidential_utils::ConfidentialDvpFixture;
use crate::state_utils::AMOUNT_B;
use crate::utils::{execute, ClientFixture, TestContext};
use dvp_swap_program_client::{confidential::*, verify::verify_confidential_funding};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

/// Rust-created swap and escrow B, verified before funding by both clients.
const FUNDING_VECTOR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../test-vectors/confidential-funding.json"
);
const FUNDING_MASTER_KEY: [u8; 32] = [0x5a; 32];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn funding_keys(master: &[u8], swap: &Pubkey) -> EscrowKeys {
    let seed = derive_shared_seed(swap, |message| -> Result<[u8; 32], ()> {
        let mut mac = Hmac::<Sha256>::new_from_slice(master).unwrap();
        mac.update(message);
        Ok(mac.finalize().into_bytes().into())
    })
    .unwrap();
    EscrowKeys::from_seed(&seed).unwrap()
}

fn account_json(address: &Pubkey, account: &Account) -> serde_json::Value {
    serde_json::json!({
        "address": address.to_string(),
        "owner": account.owner.to_string(),
        "lamports": account.lamports.to_string(),
        "data": hex(&account.data),
    })
}

fn account_from_json(value: &serde_json::Value) -> (Pubkey, Account) {
    let address = value["address"].as_str().unwrap().parse().unwrap();
    let account = Account {
        lamports: value["lamports"].as_str().unwrap().parse().unwrap(),
        data: unhex(value["data"].as_str().unwrap()),
        owner: value["owner"].as_str().unwrap().parse().unwrap(),
        executable: false,
        rent_epoch: 0,
    };
    (address, account)
}

/// Regenerate with `cargo test -p dvp-swap-program-client --all-features --test integration
/// write_confidential_funding_vector -- --ignored`, then run both client suites.
#[test]
#[ignore]
fn write_confidential_funding_vector() {
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, true, false);
    let keys = funding_keys(&FUNDING_MASTER_KEY, &f.accounts.swap_dvp);
    let config = SessionConfig::new(
        context.payer.pubkey(),
        TransactionFormat::V1,
        context.svm.get_sysvar(),
    );
    let create = create_session(&config, &f.accounts, f.args.clone(), &keys, AMOUNT_B).unwrap();
    execute(&mut context, &config, create, &[]);
    let swap = context.get_account(&f.accounts.swap_dvp).unwrap();
    let escrow = context.get_account(&f.accounts.dvp_ata_b).unwrap();
    std::fs::write(
        FUNDING_VECTOR,
        serde_json::to_string_pretty(&serde_json::json!({
            "master_key": hex(&FUNDING_MASTER_KEY),
            "amount_b": AMOUNT_B.to_string(),
            "swap": account_json(&f.accounts.swap_dvp, &swap),
            "escrow_b": account_json(&f.accounts.dvp_ata_b, &escrow),
        }))
        .unwrap()
            + "\n",
    )
    .unwrap();
}

#[test]
fn rust_created_funding_vector_verifies() {
    let vector: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(FUNDING_VECTOR).unwrap()).unwrap();
    let (swap_address, swap) = account_from_json(&vector["swap"]);
    let (escrow_address, escrow) = account_from_json(&vector["escrow_b"]);
    let amount_b = vector["amount_b"].as_str().unwrap().parse().unwrap();
    let keys = funding_keys(
        &unhex(vector["master_key"].as_str().unwrap()),
        &swap_address,
    );
    verify_confidential_funding(
        &swap_address,
        &swap,
        &escrow_address,
        &escrow,
        &keys,
        amount_b,
    )
    .unwrap();
}

#[test]
fn client_create_apply_and_verify_in_both_formats() {
    for format in [TransactionFormat::V1, TransactionFormat::V0] {
        // Create the swap, fund escrow B, and apply its pending balance.
        let mut context = TestContext::new();
        let f = ClientFixture::new(&mut context, format, AMOUNT_B);
        let state = f.source(&context);
        let raw = context.get_account(&f.dvp.accounts.dvp_ata_b).unwrap();
        let raw_swap = context.get_account(&f.dvp.accounts.swap_dvp).unwrap();

        // Verify the funded accounts and applied balance.
        let (terms, checked_ct) = verify_confidential_funding(
            &f.dvp.accounts.swap_dvp,
            &raw_swap,
            &f.dvp.accounts.dvp_ata_b,
            &raw,
            &f.keys,
            AMOUNT_B,
        )
        .unwrap();
        assert_eq!(terms, f.swap(&context));
        assert_eq!(checked_ct, state);
        assert_eq!(
            read_escrow_account(
                &f.dvp.accounts.dvp_ata_b,
                &raw.owner,
                &raw.data,
                &f.dvp.accounts.swap_dvp,
                &f.dvp.accounts.mint_b
            )
            .unwrap()
            .0,
            state
        );
        verify_confidential_swap(&f.swap(&context), &state, &f.keys, AMOUNT_B).unwrap();
        let balance = read_escrow_balance(&state, 0, &f.keys, &[]).unwrap();
        assert_eq!(balance.available, AMOUNT_B);
        assert_eq!(balance.pending, 0);
    }
}

#[test]
fn escrow_account_rejects_wrong_owner_or_address() {
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, TransactionFormat::V1, 0);
    let raw = context.get_account(&f.dvp.accounts.dvp_ata_b).unwrap();

    // Keep valid escrow data and change only its owner or address.
    assert!(matches!(
        read_escrow_account(
            &f.dvp.accounts.dvp_ata_b,
            &f.config.payer,
            &raw.data,
            &f.dvp.accounts.swap_dvp,
            &f.dvp.accounts.mint_b
        ),
        Err(ConfidentialError::Account("escrow owner or address"))
    ));
    assert!(matches!(
        read_escrow_account(
            &f.refund,
            &raw.owner,
            &raw.data,
            &f.dvp.accounts.swap_dvp,
            &f.dvp.accounts.mint_b
        ),
        Err(ConfidentialError::Account("escrow owner or address"))
    ));
}

#[test]
fn confidential_swap_rejects_wrong_amount_or_keys() {
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, TransactionFormat::V1, AMOUNT_B);
    let state = f.source(&context);
    let swap = f.swap(&context);

    assert!(matches!(
        verify_confidential_swap(&swap, &state, &f.keys, AMOUNT_B + 1),
        Err(ConfidentialError::AmountMismatch)
    ));
    let wrong_keys = EscrowKeys::from_seed(&[0x43; 32]).unwrap();
    assert!(matches!(
        verify_confidential_swap(&swap, &state, &wrong_keys, AMOUNT_B),
        Err(ConfidentialError::EscrowKeyMismatch)
    ));
}

#[test]
fn funding_verification_rejects_wrong_escrow_address() {
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, TransactionFormat::V1, AMOUNT_B);
    let raw = context.get_account(&f.dvp.accounts.dvp_ata_b).unwrap();
    let raw_swap = context.get_account(&f.dvp.accounts.swap_dvp).unwrap();

    // Substitute the refund address while keeping valid escrow data.
    assert!(matches!(
        verify_confidential_funding(
            &f.dvp.accounts.swap_dvp,
            &raw_swap,
            &f.refund,
            &raw,
            &f.keys,
            AMOUNT_B
        ),
        Err(ConfidentialError::Account("escrow owner or address"))
    ));
}
