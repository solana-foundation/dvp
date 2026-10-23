use crate::{
    confidential_utils::{
        pending, set_mint_auditor, state, ConfidentialDvpFixture, Keys, MAX_PENDING,
    },
    state_utils::{AMOUNT_A, AMOUNT_B},
    utils::{execute, get_token_balance, ClientFixture, TestContext},
};
use dvp_swap_program_client::confidential::test_utils::{AMOUNT_LO_BITS, RECORD_PROGRAM_ID};
use dvp_swap_program_client::confidential::*;
use dvp_swap_program_client::verify::ExpectedConfidentialTerms;

#[test]
fn client_settle_exact_and_surplus_in_both_formats() {
    // Arbitrary nonzero limbs exercise both halves of payment and surplus.
    const PAYMENT_WITH_HIGH_LIMB: u64 = (3 << AMOUNT_LO_BITS) + 42;
    const SURPLUS_WITH_HIGH_LIMB: u64 = (1 << AMOUNT_LO_BITS) + 1;

    for format in [TransactionFormat::V1, TransactionFormat::V0] {
        for (amount_b, surplus, with_auditor) in [
            (AMOUNT_B, 0, false),
            (PAYMENT_WITH_HIGH_LIMB, SURPLUS_WITH_HIGH_LIMB, true),
        ] {
            // Fund the payment and optional surplus; exercise nonzero high limbs with an auditor.
            let mut context = TestContext::new();
            let fixture = ConfidentialDvpFixture::new(&mut context, true, false);
            let f = ClientFixture::with_fixture(
                &mut context,
                format,
                amount_b + surplus,
                fixture,
                amount_b,
                false,
                MAX_PENDING,
            );
            let auditor_keys = Keys::new();
            if with_auditor {
                set_mint_auditor(
                    &mut context,
                    &f.dvp.accounts.mint_b,
                    auditor_keys.elgamal.pubkey(),
                );
            }
            let mint_account = context.get_account(&f.dvp.accounts.mint_b).unwrap();
            let raw_swap = context.get_account(&f.dvp.accounts.swap_dvp).unwrap();
            let raw_escrow = context.get_account(&f.dvp.accounts.dvp_ata_b).unwrap();
            let raw_recipient = context.get_account(&f.recipient).unwrap();
            let raw_refund = context.get_account(&f.refund).unwrap();

            // Run the integrator recipe, including snapshot checks and auditor extraction.
            let session = crate::confidential_examples::settle::settle(
                &f.config,
                f.settle_accounts(),
                crate::confidential_examples::settle::Snapshot {
                    swap: &raw_swap,
                    escrow: &raw_escrow,
                    recipient: &raw_recipient,
                    surplus_recipient: &raw_refund,
                    mint: &mint_account,
                },
                &f.keys,
                &f.expected_terms(amount_b),
                &[],
                &[],
                &[],
            )
            .unwrap();
            assert!(!session.preparation.is_empty());
            if format == TransactionFormat::V0 {
                assert!(session
                    .preparation
                    .iter()
                    .flat_map(|p| &p.instructions)
                    .any(|ix| ix.program_id == RECORD_PROGRAM_ID));
                assert!(!session
                    .final_transaction
                    .message(&f.config, context.svm.latest_blockhash())
                    .unwrap()
                    .address_table_lookups()
                    .unwrap()
                    .is_empty());
            }

            // Settle both legs and verify recipient balances and escrow closure.
            let history = execute(&mut context, &f.config, session, &[&f.dvp.authority]);
            assert!(context.get_account(&f.dvp.accounts.swap_dvp).is_none());
            assert!(context.get_account(&f.dvp.accounts.dvp_ata_b).is_none());
            assert_eq!(get_token_balance(&context, &f.asset_recipient), AMOUNT_A);
            assert_eq!(pending(&context, &f.recipient, &f.recipient_keys), amount_b);
            assert_eq!(pending(&context, &f.refund, &f.buyer_keys), surplus);

            // Decode the actual preparation transactions and DvP Token CPIs.
            let mut replay = BalanceHistory::new(f.recipient);
            for transaction in &history {
                replay.push(transaction).unwrap();
            }
            assert!(matches!(replay.events(), [BalanceEvent::Credit { .. }]));

            // Recover the recipient's pending balance and compare it with the payment.
            let keys = EscrowKeys {
                elgamal: f.recipient_keys.elgamal.clone(),
                ae: f.recipient_keys.ae.clone(),
                opening_lo: f.keys.opening_lo.clone(),
                opening_hi: f.keys.opening_hi.clone(),
            };
            let recovered =
                recover_escrow_balance(&state(&context, &f.recipient), 0, &keys, replay.events())
                    .unwrap();
            assert_eq!(recovered.pending, amount_b);
        }
    }
}

#[test]
fn client_settle_credits_payment_and_surplus_to_the_same_recipient() {
    let mut context = TestContext::new();
    let f = combined_recipient_fixture(&mut context, 2);

    // Payment and surplus consume two pending credits on the same account.
    let session = combined_recipient_session(&context, &f).unwrap();
    execute(&mut context, &f.config, session, &[&f.dvp.authority]);
    assert_eq!(pending(&context, &f.refund, &f.buyer_keys), AMOUNT_B + 7);
    assert_eq!(
        u64::from(state(&context, &f.refund).pending_balance_credit_counter),
        2
    );
    assert!(context.get_account(&f.dvp.accounts.swap_dvp).is_none());
}

#[test]
fn verify_confidential_terms_compares_every_public_term() {
    use dvp_swap_program_client::verify::verify_confidential_terms;
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, TransactionFormat::V1, AMOUNT_B);
    let swap = context.get_account(&f.dvp.accounts.swap_dvp).unwrap();
    let escrow = context.get_account(&f.dvp.accounts.dvp_ata_b).unwrap();
    let terms = f.expected_terms(AMOUNT_B);
    let checked = verify_confidential_terms(&swap, &escrow, &f.keys, &terms).unwrap();
    assert_eq!(checked.swap_address, f.dvp.accounts.swap_dvp);
    assert_eq!(checked.escrow_a, f.dvp.accounts.dvp_ata_a);
    assert_eq!(checked.escrow_b, f.dvp.accounts.dvp_ata_b);

    let other = solana_pubkey::Pubkey::new_unique();
    type Change = fn(&mut ExpectedConfidentialTerms, solana_pubkey::Pubkey);
    let changed: [(Change, &str); 6] = [
        (|t, k| t.token_program_a = k, "token program A"),
        (|t, _| t.amount_a += 1, "amount A"),
        (|t, _| t.expiry_timestamp += 1, "expiry"),
        (
            |t, _| t.earliest_settlement_timestamp = Some(1),
            "earliest settlement",
        ),
        (
            |t, k| t.user_a_settlement_destination = k,
            "user A settlement destination",
        ),
        (
            |t, k| t.user_b_settlement_destination = k,
            "user B settlement destination",
        ),
    ];
    for (change, field) in changed {
        let mut wrong = terms.clone();
        change(&mut wrong, other);
        assert!(matches!(
            verify_confidential_terms(&swap, &escrow, &f.keys, &wrong),
            Err(ConfidentialError::Account(message)) if message == field
        ));
    }
    // Seed terms derive another PDA; amount B is bound by the ciphertexts.
    let mut wrong = terms.clone();
    wrong.nonce += 1;
    assert!(matches!(
        verify_confidential_terms(&swap, &escrow, &f.keys, &wrong),
        Err(ConfidentialError::Swap(_))
    ));
    let mut wrong = terms.clone();
    wrong.amount_b += 1;
    assert!(matches!(
        verify_confidential_terms(&swap, &escrow, &f.keys, &wrong),
        Err(ConfidentialError::AmountMismatch)
    ));
}

#[test]
fn settle_checks_the_account_set_against_the_swap() {
    use dvp_swap_program_client::instructions::SettleConfidentialDvp;
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, TransactionFormat::V1, AMOUNT_B);
    let source = f.source(&context);
    let swap = f.swap(&context);
    let recipient = context.get_account(&f.recipient).unwrap();
    let refund = context.get_account(&f.refund).unwrap();
    let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
    let other = solana_pubkey::Pubkey::new_unique();
    type Change = fn(&mut SettleConfidentialDvp, solana_pubkey::Pubkey);
    let changed: [(Change, &str); 12] = [
        (|a, k| a.settlement_authority = k, "settlement authority"),
        (|a, k| a.swap_dvp = k, "swap"),
        (|a, k| a.mint_a = k, "mint A"),
        (|a, k| a.mint_b = k, "mint B"),
        (|a, k| a.token_program_a = k, "token program A"),
        (|a, k| a.token_program_b = k, "token program B"),
        (|a, k| a.dvp_ata_a = k, "escrow A"),
        (|a, k| a.dvp_ata_b = k, "escrow B"),
        (|a, k| a.user_a_destination_ata_b = k, "user A destination"),
        (|a, k| a.user_b_destination_ata_a = k, "user B destination"),
        (|a, k| a.user_a_ata_a = k, "user A surplus account"),
        (|a, k| a.user_b_ata_b = k, "user B surplus account"),
    ];
    for (change, field) in changed {
        let mut accounts = f.settle_accounts();
        change(&mut accounts, other);
        let result = settle_session(
            &f.config,
            accounts,
            SettleRequest {
                source: TransferSource {
                    state: &source,
                    keys: &f.keys,
                    history: &[],
                },
                swap: &swap,
                expected_amount_b: AMOUNT_B,
                recipient: crate::utils::recipient_account(f.recipient, &recipient),
                surplus_recipient: crate::utils::recipient_account(f.refund, &refund),
                expected_recipient: None,
                expected_surplus_recipient: None,
                mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
                expected_auditor: None,
            },
            &[],
            &[],
        );
        assert!(matches!(
            result,
            Err(ConfidentialError::Account(message)) if message == field
        ));
    }
}

#[test]
fn settle_rejects_wrong_amount_or_keys() {
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, TransactionFormat::V1, AMOUNT_B);
    let source = f.source(&context);
    let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
    let swap = f.swap(&context);
    let recipient = context.get_account(&f.recipient).unwrap();
    let refund = context.get_account(&f.refund).unwrap();
    let build = |keys, expected_amount_b| {
        settle_session(
            &f.config,
            f.settle_accounts(),
            SettleRequest {
                source: TransferSource {
                    state: &source,
                    keys,
                    history: &[],
                },
                swap: &swap,
                expected_amount_b,
                recipient: crate::utils::recipient_account(f.recipient, &recipient),
                surplus_recipient: crate::utils::recipient_account(f.refund, &refund),
                expected_recipient: None,
                expected_surplus_recipient: None,
                mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
                expected_auditor: None,
            },
            &[],
            &[],
        )
    };

    assert!(matches!(
        build(&f.keys, AMOUNT_B + 1),
        Err(ConfidentialError::AmountMismatch)
    ));
    let wrong_keys = EscrowKeys::from_seed(&[23; 32]).unwrap();
    assert!(matches!(
        build(&wrong_keys, AMOUNT_B),
        Err(ConfidentialError::EscrowKeyMismatch)
    ));
}

#[test]
fn settle_binds_recipient_snapshots_and_expected_keys() {
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, TransactionFormat::V1, AMOUNT_B + 7);
    let source = f.source(&context);
    let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
    let swap = f.swap(&context);
    let recipient = context.get_account(&f.recipient).unwrap();
    let refund = context.get_account(&f.refund).unwrap();
    let other = Keys::new();
    let build = |payment, expected_recipient, expected_surplus_recipient| {
        settle_session(
            &f.config,
            f.settle_accounts(),
            SettleRequest {
                source: TransferSource {
                    state: &source,
                    keys: &f.keys,
                    history: &[],
                },
                swap: &swap,
                expected_amount_b: AMOUNT_B,
                recipient: payment,
                surplus_recipient: crate::utils::recipient_account(f.refund, &refund),
                expected_recipient,
                expected_surplus_recipient,
                mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
                expected_auditor: None,
            },
            &[],
            &[],
        )
    };
    let payment = crate::utils::recipient_account(f.recipient, &recipient);

    assert!(build(
        payment,
        Some(f.recipient_keys.elgamal.pubkey()),
        Some(f.buyer_keys.elgamal.pubkey())
    )
    .is_ok());
    // A key other than the agreed one would receive the amount.
    for (expected_recipient, expected_surplus_recipient) in [
        (Some(other.elgamal.pubkey()), None),
        (None, Some(other.elgamal.pubkey())),
    ] {
        assert!(matches!(
            build(payment, expected_recipient, expected_surplus_recipient),
            Err(ConfidentialError::Account("recipient ElGamal key"))
        ));
    }
    // A snapshot of another account than the instruction's destination.
    assert!(matches!(
        build(
            crate::utils::recipient_account(f.refund, &refund),
            None,
            None
        ),
        Err(ConfidentialError::Account("recipient address or owner"))
    ));
}

#[test]
fn settle_binds_the_mint_and_expected_auditor() {
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, TransactionFormat::V1, AMOUNT_B);
    let source = f.source(&context);
    let swap = f.swap(&context);
    let recipient = context.get_account(&f.recipient).unwrap();
    let refund = context.get_account(&f.refund).unwrap();
    let (auditor, other) = (Keys::new(), Keys::new());
    let build = |mint: MintAccount<'_>, expected_auditor| {
        settle_session(
            &f.config,
            f.settle_accounts(),
            SettleRequest {
                source: TransferSource {
                    state: &source,
                    keys: &f.keys,
                    history: &[],
                },
                swap: &swap,
                expected_amount_b: AMOUNT_B,
                recipient: crate::utils::recipient_account(f.recipient, &recipient),
                surplus_recipient: crate::utils::recipient_account(f.refund, &refund),
                expected_recipient: None,
                expected_surplus_recipient: None,
                mint,
                expected_auditor,
            },
            &[],
            &[],
        )
    };
    let without = context.get_account(&f.dvp.accounts.mint_b).unwrap();
    set_mint_auditor(
        &mut context,
        &f.dvp.accounts.mint_b,
        auditor.elgamal.pubkey(),
    );
    let with = context.get_account(&f.dvp.accounts.mint_b).unwrap();
    let mint = |account| crate::utils::mint_account(f.dvp.accounts.mint_b, account);

    assert!(build(mint(&without), Some(None)).is_ok());
    assert!(build(mint(&with), None).is_ok());
    assert!(build(mint(&with), Some(Some(auditor.elgamal.pubkey()))).is_ok());
    // An RPC must neither add, remove nor substitute the configured auditor.
    for (account, expected) in [
        (&with, Some(None)),
        (&without, Some(Some(auditor.elgamal.pubkey()))),
        (&with, Some(Some(other.elgamal.pubkey()))),
    ] {
        assert!(matches!(
            build(mint(account), expected),
            Err(ConfidentialError::Account("mint auditor"))
        ));
    }
    // A snapshot of another account than the swap's mint B.
    let mint_a = context.get_account(&f.dvp.accounts.mint_a).unwrap();
    assert!(matches!(
        build(
            crate::utils::mint_account(f.dvp.accounts.mint_a, &mint_a),
            None
        ),
        Err(ConfidentialError::Account("mint address or owner"))
    ));
}

#[test]
fn settle_rejects_insufficient_combined_recipient_capacity() {
    let mut context = TestContext::new();
    let f = combined_recipient_fixture(&mut context, 1);

    assert!(matches!(
        combined_recipient_session(&context, &f),
        Err(ConfidentialError::RecipientPendingCounterFull { required: 2 })
    ));
    assert_eq!(pending(&context, &f.refund, &f.buyer_keys), 0);
}

fn combined_recipient_fixture(context: &mut TestContext, capacity: u64) -> ClientFixture {
    use solana_signer::Signer;
    let mut fixture = ConfidentialDvpFixture::new(context, true, false);
    fixture.args.user_a_settlement_destination = Some(fixture.user_b.pubkey());
    let mut f = ClientFixture::with_fixture(
        context,
        TransactionFormat::V1,
        AMOUNT_B + 7,
        fixture,
        AMOUNT_B,
        false,
        capacity,
    );
    f.recipient = f.refund;
    f
}

fn combined_recipient_session(
    context: &TestContext,
    f: &ClientFixture,
) -> Result<TransactionSession, ConfidentialError> {
    let source = f.source(context);
    let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
    let recipient = context.get_account(&f.refund).unwrap();
    let swap = f.swap(context);
    settle_session(
        &f.config,
        f.settle_accounts(),
        SettleRequest {
            source: TransferSource {
                state: &source,
                keys: &f.keys,
                history: &[],
            },
            swap: &swap,
            expected_amount_b: AMOUNT_B,
            recipient: crate::utils::recipient_account(f.refund, &recipient),
            surplus_recipient: crate::utils::recipient_account(f.refund, &recipient),
            expected_recipient: None,
            expected_surplus_recipient: None,
            mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
            expected_auditor: None,
        },
        &[],
        &[],
    )
}
