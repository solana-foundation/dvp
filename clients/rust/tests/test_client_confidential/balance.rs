use crate::{
    confidential_utils::{pending, send_v1, state},
    state_utils::AMOUNT_B,
    utils::{execute, ClientFixture, TestContext, TOKEN_2022_PROGRAM_ID as TOKEN},
};
use dvp_swap_program_client::confidential::test_utils::{
    checked_available_balance, ciphertext_matches, transfer_session, TransferAccounts,
    TransferRequest, AMOUNT_LO_BITS,
};
use dvp_swap_program_client::confidential::*;
use solana_signer::Signer;
use solana_zk_sdk::encryption::elgamal::ElGamalCiphertext;
use spl_token_2022_interface::{
    extension::confidential_transfer::{instruction as ct, ConfidentialTransferAccount},
    instruction as token,
};

const FUNDING_AMOUNT: u64 = (1u64 << 47) + 65_535;
// Small nonzero values in both limbs keep local history checks cheap.
const HISTORY_CREDIT_AMOUNT: u64 = (1 << AMOUNT_LO_BITS) + 1;

#[test]
fn client_recovers_real_funding_history_and_repairs_false_ae_in_both_formats() {
    for format in [TransactionFormat::V1, TransactionFormat::V0] {
        // Two real transfers push the pending high limb beyond the u32 decoding range.
        let (mut context, f, mut history) = funded_history(format);
        let source = f.source(&context);
        assert_eq!(
            read_escrow_balance(&source, 0, &f.keys, history.events())
                .unwrap()
                .pending,
            FUNDING_AMOUNT * 2
        );

        // Submit a valid transaction whose AE plaintext disagrees with the real balance.
        apply_false_ae(&mut context, &f, &mut history);
        let source = f.source(&context);
        assert!(matches!(
            checked_available_balance(&source, &f.keys),
            Err(ConfidentialError::BalanceMismatch)
        ));
        assert_eq!(
            read_escrow_balance(&source, 0, &f.keys, history.events())
                .unwrap()
                .available,
            FUNDING_AMOUNT * 2
        );

        // Repair the AE value using the verified transaction history.
        let repair = apply_session(
            &f.config,
            &f.apply_accounts(f.dvp.user_b.pubkey()),
            f.apply_args(),
            TransferSource {
                state: &source,
                keys: &f.keys,
                history: history.events(),
            },
        )
        .unwrap();
        execute(&mut context, &f.config, repair, &[&f.dvp.user_b]);
        let repaired = f.source(&context);
        assert_eq!(
            checked_available_balance(&repaired, &f.keys).unwrap(),
            FUNDING_AMOUNT * 2
        );
        let balance = read_escrow_balance(&repaired, 0, &f.keys, &[]).unwrap();
        assert_eq!(balance.pending, 0);
        assert_eq!(balance.pending_credit_counter, 0);
    }
}

#[test]
fn client_funding_spends_available_without_touching_large_pending_in_both_formats() {
    for format in [TransactionFormat::V1, TransactionFormat::V0] {
        let mut context = TestContext::new();
        let f = ClientFixture::new(&mut context, format, 0);

        // Make the payment spendable, then leave two large deposits pending.
        send_v1(
            &mut context,
            &[
                token::mint_to(
                    &TOKEN,
                    &f.dvp.accounts.mint_b,
                    &f.refund,
                    &f.config.payer,
                    &[],
                    AMOUNT_B,
                )
                .unwrap(),
                ct::deposit(
                    &TOKEN,
                    &f.refund,
                    &f.dvp.accounts.mint_b,
                    AMOUNT_B,
                    6,
                    &f.dvp.user_b.pubkey(),
                    &[],
                )
                .unwrap(),
                ct::apply_pending_balance(
                    &TOKEN,
                    &f.refund,
                    1,
                    &f.buyer_keys.balance(AMOUNT_B),
                    &f.dvp.user_b.pubkey(),
                    &[],
                )
                .unwrap(),
            ],
            &[&f.dvp.user_b],
        )
        .unwrap();
        // One maximum deposit fits in u32's high limb; two push pending beyond it.
        deposit_pending(&mut context, &f, MAX_TRANSFER_AMOUNT);
        deposit_pending(&mut context, &f, MAX_TRANSFER_AMOUNT);
        let wallet_keys = EscrowKeys {
            elgamal: f.buyer_keys.elgamal.clone(),
            ae: f.buyer_keys.ae.clone(),
            opening_lo: f.keys.opening_lo.clone(),
            opening_hi: f.keys.opening_hi.clone(),
        };
        let source = state(&context, &f.refund);
        assert!(ciphertext_matches(
            &source.pending_balance_hi.try_into().unwrap(),
            &wallet_keys,
            2 * (MAX_TRANSFER_AMOUNT >> AMOUNT_LO_BITS)
        ));

        // Fund escrow from available balance without decoding the large pending balance.
        let destination = f.source(&context);
        let session = transfer_session(
            &f.config,
            TransferAccounts {
                authority: f.dvp.user_b.pubkey(),
                source: f.refund,
                mint: f.dvp.accounts.mint_b,
                destination: f.dvp.accounts.dvp_ata_b,
            },
            TransferRequest {
                source: TransferSource {
                    state: &source,
                    keys: &wallet_keys,
                    history: &[],
                },
                recipient: &destination,
                amount: AMOUNT_B,
                auditor: None,
            },
            &[],
        )
        .unwrap();
        execute(&mut context, &f.config, session, &[&f.dvp.user_b]);

        // Only available was spent; the wallet's pending ciphertexts and counter are unchanged.
        let remaining = state(&context, &f.refund);
        assert_eq!(
            checked_available_balance(&remaining, &wallet_keys).unwrap(),
            0
        );
        assert_eq!(remaining.pending_balance_lo, source.pending_balance_lo);
        assert_eq!(remaining.pending_balance_hi, source.pending_balance_hi);
        assert_eq!(
            remaining.pending_balance_credit_counter,
            source.pending_balance_credit_counter
        );

        // Escrow receives just the payment as one new pending credit.
        assert_eq!(
            pending(&context, &f.dvp.accounts.dvp_ata_b, &f.dvp.keys),
            AMOUNT_B
        );
        assert_eq!(
            u64::from(f.source(&context).pending_balance_credit_counter),
            1
        );
    }
}

#[test]
fn balance_recovery_rejects_truncated_funding_history() {
    let (mut context, f, mut history) = funded_history(TransactionFormat::V1);
    apply_false_ae(&mut context, &f, &mut history);
    let source = f.source(&context);

    // Removing the first credit must invalidate recovery against the on-chain balance.
    assert!(matches!(
        recover_escrow_balance(&source, 0, &f.keys, &history.events()[1..]),
        Err(ConfidentialError::IncompleteHistory)
    ));
}

#[test]
fn balance_recovery_rejects_duplicate_credit() {
    let (keys, snapshot, mut history) = pending_snapshot();
    assert_eq!(
        recover_escrow_balance(&snapshot, 0, &keys, &history)
            .unwrap()
            .pending,
        HISTORY_CREDIT_AMOUNT
    );

    // Counting the same transfer twice must not inflate the recovered balance.
    history.push(history[0].clone());
    assert!(matches!(
        recover_escrow_balance(&snapshot, 0, &keys, &history),
        Err(ConfidentialError::IncompleteHistory)
    ));
}

#[test]
fn balance_recovery_rejects_apply_before_credit() {
    let (keys, mut snapshot, mut history) = pending_snapshot();
    snapshot.available_balance = keys.elgamal.pubkey().encrypt(HISTORY_CREDIT_AMOUNT).into();
    snapshot.decryptable_available_balance = keys.ae.encrypt(HISTORY_CREDIT_AMOUNT).into();
    snapshot.pending_balance_lo = Default::default();
    snapshot.pending_balance_hi = Default::default();
    snapshot.pending_balance_credit_counter = 0.into();
    history.push(BalanceEvent::Apply);
    assert_eq!(
        recover_escrow_balance(&snapshot, 0, &keys, &history)
            .unwrap()
            .available,
        HISTORY_CREDIT_AMOUNT
    );

    // Apply before the credit leaves funds pending instead of available.
    history.swap(0, 1);
    assert!(matches!(
        recover_escrow_balance(&snapshot, 0, &keys, &history),
        Err(ConfidentialError::IncompleteHistory)
    ));
}

#[test]
fn balance_recovery_rejects_pending_counter_mismatch() {
    let (keys, mut snapshot, history) = pending_snapshot();
    let recovered = recover_escrow_balance(&snapshot, 0, &keys, &history).unwrap();
    assert_eq!(recovered.pending, HISTORY_CREDIT_AMOUNT);
    assert_eq!(recovered.pending_credit_counter, 1);

    // Keep all ciphertexts unchanged but report one extra pending credit.
    snapshot.pending_balance_credit_counter = (recovered.pending_credit_counter + 1).into();
    assert!(matches!(
        recover_escrow_balance(&snapshot, 0, &keys, &history),
        Err(ConfidentialError::IncompleteHistory)
    ));
}

fn funded_history(format: TransactionFormat) -> (TestContext, ClientFixture, BalanceHistory) {
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, format, 0);
    let mut history = BalanceHistory::new(f.dvp.accounts.dvp_ata_b);
    // Each credit contributes 2^31 to the high limb; two exceed u32::MAX.
    fund_once(&mut context, &f, &mut history);
    fund_once(&mut context, &f, &mut history);
    (context, f, history)
}

fn fund_once(context: &mut TestContext, f: &ClientFixture, history: &mut BalanceHistory) {
    let wallet_keys = EscrowKeys {
        elgamal: f.buyer_keys.elgamal.clone(),
        ae: f.buyer_keys.ae.clone(),
        opening_lo: f.keys.opening_lo.clone(),
        opening_hi: f.keys.opening_hi.clone(),
    };
    let amount = FUNDING_AMOUNT;
    // Fund the wallet with Token instructions, then transfer through the client.
    send_v1(
        context,
        &[
            token::mint_to(
                &TOKEN,
                &f.dvp.accounts.mint_b,
                &f.refund,
                &f.config.payer,
                &[],
                amount,
            )
            .unwrap(),
            ct::deposit(
                &TOKEN,
                &f.refund,
                &f.dvp.accounts.mint_b,
                amount,
                6,
                &f.dvp.user_b.pubkey(),
                &[],
            )
            .unwrap(),
            ct::apply_pending_balance(
                &TOKEN,
                &f.refund,
                1,
                &f.buyer_keys.balance(amount),
                &f.dvp.user_b.pubkey(),
                &[],
            )
            .unwrap(),
        ],
        &[&f.dvp.user_b],
    )
    .unwrap();
    let source = state(context, &f.refund);
    let destination = f.source(context);
    let plan = transfer_session(
        &f.config,
        TransferAccounts {
            authority: f.dvp.user_b.pubkey(),
            source: f.refund,
            mint: f.dvp.accounts.mint_b,
            destination: f.dvp.accounts.dvp_ata_b,
        },
        TransferRequest {
            source: TransferSource {
                state: &source,
                keys: &wallet_keys,
                history: &[],
            },
            recipient: &destination,
            amount,
            auditor: None,
        },
        &[],
    )
    .unwrap();
    for tx in execute(context, &f.config, plan, &[&f.dvp.user_b]) {
        history.push(&tx).unwrap();
    }
}

fn apply_false_ae(context: &mut TestContext, f: &ClientFixture, history: &mut BalanceHistory) {
    let mut args = f.apply_args();
    args.expected_pending_balance_credit_counter = 2;
    args.new_decryptable_available_balance = f.keys.ae.encrypt(1).to_bytes();
    let bad_apply = TransactionSession {
        preparation: vec![],
        cleanup: vec![],
        final_transaction: PlannedTransaction {
            signers: vec![],
            instructions: vec![f.apply_accounts(f.dvp.user_a.pubkey()).instruction(args)],
        },
    };
    for tx in execute(context, &f.config, bad_apply, &[&f.dvp.user_a]) {
        history.push(&tx).unwrap();
    }
}

fn pending_snapshot() -> (EscrowKeys, ConfidentialTransferAccount, Vec<BalanceEvent>) {
    let keys = EscrowKeys::from_seed(&[21; 32]).unwrap();
    let encrypted = keys.encrypt_amount(HISTORY_CREDIT_AMOUNT).unwrap();
    let lo = ElGamalCiphertext::from_bytes(&encrypted.lo).unwrap();
    let hi = ElGamalCiphertext::from_bytes(&encrypted.hi).unwrap();
    let snapshot = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        pending_balance_lo: lo.into(),
        pending_balance_hi: hi.into(),
        pending_balance_credit_counter: 1.into(),
        decryptable_available_balance: keys.ae.encrypt(0).into(),
        ..Default::default()
    };
    (keys, snapshot, vec![BalanceEvent::Credit { lo, hi }])
}

fn deposit_pending(context: &mut TestContext, f: &ClientFixture, amount: u64) {
    // Identical deposits need a fresh blockhash to avoid duplicate transaction signatures.
    context.svm.expire_blockhash();
    send_v1(
        context,
        &[
            token::mint_to(
                &TOKEN,
                &f.dvp.accounts.mint_b,
                &f.refund,
                &f.config.payer,
                &[],
                amount,
            )
            .unwrap(),
            ct::deposit(
                &TOKEN,
                &f.refund,
                &f.dvp.accounts.mint_b,
                amount,
                6,
                &f.dvp.user_b.pubkey(),
                &[],
            )
            .unwrap(),
        ],
        &[&f.dvp.user_b],
    )
    .unwrap();
}
