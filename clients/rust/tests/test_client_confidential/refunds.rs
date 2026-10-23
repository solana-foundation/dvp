use crate::{
    confidential_utils::{pending, state},
    state_utils::AMOUNT_B,
    utils::{
        dvp_ata, execute, send_plan, ClientFixture, TestContext, MEMO_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID as TOKEN,
    },
};
use dvp_swap_program_client::confidential::test_utils::{
    checked_available_balance, ciphertext_matches, AMOUNT_LO_BITS,
};
use dvp_swap_program_client::{confidential::*, instructions::*};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token_2022_interface::extension::confidential_transfer::ConfidentialTransferAccount;

#[test]
fn client_reclaims_leg_a_without_proofs_in_both_formats() {
    for format in [TransactionFormat::V1, TransactionFormat::V0] {
        let mut context = TestContext::new();
        let f = ClientFixture::new(&mut context, format, 0);
        let accounts = ReclaimConfidentialDvp {
            signer: f.dvp.user_a.pubkey(),
            swap_dvp: f.dvp.accounts.swap_dvp,
            mint: f.dvp.accounts.mint_a,
            dvp_source_ata: f.dvp.accounts.dvp_ata_a,
            signer_dest_ata: f.asset_refund,
            token_program: f.dvp.accounts.token_program_a,
            memo_program: crate::utils::MEMO_PROGRAM_ID,
            zk_elgamal_proof_program: solana_zk_elgamal_proof_interface::ID,
            equality_context: None,
            validity_context: None,
            range_context: None,
            zero_context: None,
        };

        // Leg A is public, so reclaim needs no confidential proofs.
        let session = refund_session(
            &f.config,
            RefundInstruction::Reclaim(accounts),
            None,
            &[],
            &[],
        )
        .unwrap();
        assert!(session.preparation.is_empty());
        crate::utils::execute(&mut context, &f.config, session, &[&f.dvp.user_a]);

        // Return all of leg A while keeping the swap open.
        assert_eq!(
            crate::utils::get_token_balance(&context, &f.asset_refund),
            crate::state_utils::AMOUNT_A
        );
        assert!(context.get_account(&f.dvp.accounts.swap_dvp).is_some());
    }
}

#[test]
fn client_partial_reclaim_then_full_in_both_formats() {
    for format in [TransactionFormat::V1, TransactionFormat::V0] {
        let mut context = TestContext::new();
        // Apply enough available funds for a maximum partial refund followed by the remainder.
        let f = ClientFixture::new(&mut context, format, MAX_TRANSFER_AMOUNT);
        crate::confidential_utils::fund_late_b(&mut context, &f.dvp, 1);
        let source = f.source(&context);
        let apply = apply_session(
            &f.config,
            &f.apply_accounts(f.dvp.user_b.pubkey()),
            f.apply_args(),
            TransferSource {
                state: &source,
                keys: &f.keys,
                history: &[],
            },
        )
        .unwrap();
        execute(&mut context, &f.config, apply, &[&f.dvp.user_b]);

        // Two maximum-sized credits push the pending high limb beyond u32;
        // one transfer cannot exceed MAX_TRANSFER_AMOUNT.
        crate::confidential_utils::fund_late_b(&mut context, &f.dvp, MAX_TRANSFER_AMOUNT);
        crate::confidential_utils::fund_late_b(&mut context, &f.dvp, MAX_TRANSFER_AMOUNT);

        // The fixture expires after 3600 seconds; advance one second past expiry.
        // Send Partial and Full refunds without applying late credits.
        context.advance_clock(3601);
        let partial = MAX_TRANSFER_AMOUNT;
        for amount in [RefundAmount::Partial(partial), RefundAmount::Full] {
            let source = f.source(&context);
            let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
            let recipient = context.get_account(&f.refund).unwrap();
            let session = refund_session(
                &f.config,
                RefundInstruction::Reclaim(reclaim_leg_b_accounts(&f)),
                Some(RefundRequest {
                    source: TransferSource {
                        state: &source,
                        keys: &f.keys,
                        history: &[],
                    },
                    recipient: crate::utils::recipient_account(f.refund, &recipient),
                    expected_recipient: None,
                    amount,
                    mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
                    expected_auditor: None,
                }),
                &[],
                &[],
            )
            .unwrap();
            execute(&mut context, &f.config, session, &[&f.dvp.user_b]);
            assert!(context.get_account(&f.dvp.accounts.swap_dvp).is_some());
        }

        // Verify both received refunds and the untouched pending escrow credits.
        let recipient = state(&context, &f.refund);
        let recipient_keys = EscrowKeys {
            elgamal: f.buyer_keys.elgamal.clone(),
            ae: f.buyer_keys.ae.clone(),
            opening_lo: f.keys.opening_lo.clone(),
            opening_hi: f.keys.opening_hi.clone(),
        };
        assert!(ciphertext_matches(
            &recipient.pending_balance_lo.try_into().unwrap(),
            &recipient_keys,
            1 << AMOUNT_LO_BITS
        ));
        assert!(ciphertext_matches(
            &recipient.pending_balance_hi.try_into().unwrap(),
            &recipient_keys,
            MAX_TRANSFER_AMOUNT >> AMOUNT_LO_BITS
        ));
        assert_eq!(u64::from(recipient.pending_balance_credit_counter), 2);
        assert!(context.get_account(&f.dvp.accounts.dvp_ata_b).is_some());
        let source = f.source(&context);
        assert_eq!(checked_available_balance(&source, &f.keys).unwrap(), 0);
        assert!(ciphertext_matches(
            &source.pending_balance_hi.try_into().unwrap(),
            &f.keys,
            2 * (MAX_TRANSFER_AMOUNT >> AMOUNT_LO_BITS)
        ));
        assert_eq!(u64::from(source.pending_balance_credit_counter), 2);
    }
}

#[test]
fn client_cancel_reject_and_recover_in_both_formats() {
    enum Scenario {
        PartialCancel,
        FullReject,
    }
    const LATE_CREDIT: u64 = 7;
    const PUBLIC_BALANCE: u64 = 11;

    for format in [TransactionFormat::V1, TransactionFormat::V0] {
        for scenario in [Scenario::PartialCancel, Scenario::FullReject] {
            let mut context = TestContext::new();
            let f = ClientFixture::new(&mut context, format, AMOUNT_B);
            let cancel_accounts = CancelConfidentialDvp {
                settlement_authority: f.dvp.authority.pubkey(),
                swap_dvp: f.dvp.accounts.swap_dvp,
                mint_a: f.dvp.accounts.mint_a,
                mint_b: f.dvp.accounts.mint_b,
                dvp_ata_a: f.dvp.accounts.dvp_ata_a,
                dvp_ata_b: f.dvp.accounts.dvp_ata_b,
                user_a_ata_a: f.asset_refund,
                user_b_ata_b: f.refund,
                token_program_a: f.dvp.accounts.token_program_a,
                token_program_b: TOKEN,
                memo_program: MEMO_PROGRAM_ID,
                zk_elgamal_proof_program: solana_zk_elgamal_proof_interface::ID,
                equality_context: None,
                validity_context: None,
                range_context: None,
                zero_context: None,
            };
            // Cancel returns half under the authority; Reject returns all under user A.
            let (instruction, signer, amount) = match scenario {
                Scenario::PartialCancel => (
                    RefundInstruction::Cancel(cancel_accounts),
                    &f.dvp.authority,
                    RefundAmount::Partial(AMOUNT_B / 2),
                ),
                Scenario::FullReject => (
                    RefundInstruction::Reject(RejectConfidentialDvp {
                        signer: f.dvp.user_a.pubkey(),
                        swap_dvp: cancel_accounts.swap_dvp,
                        mint_a: cancel_accounts.mint_a,
                        mint_b: cancel_accounts.mint_b,
                        dvp_ata_a: cancel_accounts.dvp_ata_a,
                        dvp_ata_b: cancel_accounts.dvp_ata_b,
                        user_a_ata_a: cancel_accounts.user_a_ata_a,
                        user_b_ata_b: cancel_accounts.user_b_ata_b,
                        token_program_a: cancel_accounts.token_program_a,
                        token_program_b: cancel_accounts.token_program_b,
                        memo_program: cancel_accounts.memo_program,
                        zk_elgamal_proof_program: cancel_accounts.zk_elgamal_proof_program,
                        equality_context: None,
                        validity_context: None,
                        range_context: None,
                        zero_context: None,
                    }),
                    &f.dvp.user_a,
                    RefundAmount::Full,
                ),
            };

            // Send all proof preparations before the late credit arrives.
            let source = f.source(&context);
            let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
            let recipient = context.get_account(&f.refund).unwrap();
            let session = refund_session(
                &f.config,
                instruction,
                Some(RefundRequest {
                    source: TransferSource {
                        state: &source,
                        keys: &f.keys,
                        history: &[],
                    },
                    recipient: crate::utils::recipient_account(f.refund, &recipient),
                    expected_recipient: None,
                    amount,
                    mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
                    expected_auditor: None,
                }),
                &[],
                &[],
            )
            .unwrap();
            for plan in &session.preparation {
                send_plan(&mut context, &f.config, plan, signer).unwrap();
            }

            // These proofs cover the old available balance; the late credit stays pending.
            crate::confidential_utils::fund_late_b(&mut context, &f.dvp, LATE_CREDIT);
            execute(
                &mut context,
                &f.config,
                TransactionSession {
                    preparation: vec![], // Already sent above; execute only final.
                    ..session
                },
                &[signer],
            );

            // The swap closes, but escrow retains the late pending credit.
            assert_eq!(
                pending(&context, &f.dvp.accounts.dvp_ata_b, &f.dvp.keys),
                LATE_CREDIT
            );
            assert_eq!(
                u64::from(f.source(&context).pending_balance_credit_counter),
                1
            );
            assert!(context.get_account(&f.dvp.accounts.swap_dvp).is_none());

            // Add public tokens separately, then apply the late credit after swap closure.
            // Public tokens will keep escrow alive after the confidential refund.
            crate::confidential_utils::mint_public(&mut context, &f.dvp, PUBLIC_BALANCE);
            let source = f.source(&context);
            let session = apply_session(
                &f.config,
                &f.apply_accounts(f.dvp.user_b.pubkey()),
                f.apply_args(),
                TransferSource {
                    state: &source,
                    keys: &f.keys,
                    history: &[],
                },
            )
            .unwrap();
            execute(&mut context, &f.config, session, &[&f.dvp.user_b]);

            // Recover Full returns the late credit plus any remainder from Partial Cancel.
            // The tombstone and original terms identify the already closed swap.
            let source = f.source(&context);
            let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
            let recipient = context.get_account(&f.refund).unwrap();
            let accounts = RecoverConfidentialDvp {
                signer: f.dvp.user_b.pubkey(),
                swap_dvp: f.dvp.accounts.swap_dvp,
                nonce_tombstone: f.dvp.accounts.nonce_tombstone,
                mint: f.dvp.accounts.mint_b,
                dvp_escrow_ata: f.dvp.accounts.dvp_ata_b,
                signer_dest_ata: dvp_ata(&f.dvp.user_b.pubkey(), &f.dvp.accounts.mint_b, &TOKEN),
                token_program: TOKEN,
                memo_program: MEMO_PROGRAM_ID,
                zk_elgamal_proof_program: solana_zk_elgamal_proof_interface::ID,
                equality_context: None,
                validity_context: None,
                range_context: None,
                zero_context: None,
            };
            let args = RecoverConfidentialDvpInstructionArgs {
                settlement_authority: f.dvp.authority.pubkey(),
                user_a: f.dvp.user_a.pubkey(),
                user_b: f.dvp.user_b.pubkey(),
                mint_a: f.dvp.accounts.mint_a,
                mint_b: f.dvp.accounts.mint_b,
                nonce: f.dvp.args.nonce,
                // refund_session replaces this using RefundRequest.amount.
                leg_b_refund: dvp_swap_program_client::types::LegBRefund::None,
            };
            let session = refund_session(
                &f.config,
                RefundInstruction::Recover(accounts, args),
                Some(RefundRequest {
                    source: TransferSource {
                        state: &source,
                        keys: &f.keys,
                        history: &[],
                    },
                    recipient: crate::utils::recipient_account(f.refund, &recipient),
                    expected_recipient: None,
                    amount: RefundAmount::Full,
                    mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
                    expected_auditor: None,
                }),
                &[],
                &[],
            )
            .unwrap();
            execute(&mut context, &f.config, session, &[&f.dvp.user_b]);

            // Full drained the confidential funds; None withdraws public tokens and closes escrow.
            let source = f.source(&context);
            let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
            let recipient = context.get_account(&f.refund).unwrap();
            let accounts = RecoverConfidentialDvp {
                signer: f.dvp.user_b.pubkey(),
                swap_dvp: f.dvp.accounts.swap_dvp,
                nonce_tombstone: f.dvp.accounts.nonce_tombstone,
                mint: f.dvp.accounts.mint_b,
                dvp_escrow_ata: f.dvp.accounts.dvp_ata_b,
                signer_dest_ata: f.refund,
                token_program: TOKEN,
                memo_program: MEMO_PROGRAM_ID,
                zk_elgamal_proof_program: solana_zk_elgamal_proof_interface::ID,
                equality_context: None,
                validity_context: None,
                range_context: None,
                zero_context: None,
            };
            let args = RecoverConfidentialDvpInstructionArgs {
                settlement_authority: f.dvp.authority.pubkey(),
                user_a: f.dvp.user_a.pubkey(),
                user_b: f.dvp.user_b.pubkey(),
                mint_a: f.dvp.accounts.mint_a,
                mint_b: f.dvp.accounts.mint_b,
                nonce: f.dvp.args.nonce,
                leg_b_refund: dvp_swap_program_client::types::LegBRefund::None,
            };
            let session = refund_session(
                &f.config,
                RefundInstruction::Recover(accounts, args),
                Some(RefundRequest {
                    source: TransferSource {
                        state: &source,
                        keys: &f.keys,
                        history: &[],
                    },
                    recipient: crate::utils::recipient_account(f.refund, &recipient),
                    expected_recipient: None,
                    amount: RefundAmount::None,
                    mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
                    expected_auditor: None,
                }),
                &[],
                &[],
            )
            .unwrap();
            execute(&mut context, &f.config, session, &[&f.dvp.user_b]);

            // Both paths return all original funding, the late credit, and the public tokens.
            assert!(context.get_account(&f.dvp.accounts.dvp_ata_b).is_none());
            assert_eq!(
                pending(&context, &f.refund, &f.buyer_keys),
                AMOUNT_B + LATE_CREDIT
            );
            assert_eq!(
                crate::utils::get_token_balance(&context, &f.refund),
                PUBLIC_BALANCE
            );
        }
    }
}

#[test]
fn refund_rejects_zero_partial_amount() {
    assert!(matches!(
        refund_from_snapshot(AMOUNT_B, RefundAmount::Partial(0)),
        Err(ConfidentialError::ZeroPartialRefund)
    ));
}

#[test]
fn refund_rejects_partial_above_available_balance() {
    assert!(matches!(
        refund_from_snapshot(AMOUNT_B, RefundAmount::Partial(AMOUNT_B + 1)),
        Err(ConfidentialError::InsufficientAvailable { available, required })
            if available == AMOUNT_B && required == AMOUNT_B + 1
    ));
}

#[test]
fn refund_rejects_none_with_nonzero_available_balance() {
    assert!(matches!(
        refund_from_snapshot(AMOUNT_B, RefundAmount::None),
        Err(ConfidentialError::BalanceMismatch)
    ));
}

#[test]
fn none_refund_does_not_need_a_readable_balance() {
    // The all-zero available ciphertext is all None checks. A false AE with
    // pending credits and no history must not block it.
    let keys = refund_keys();
    let source = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        decryptable_available_balance: keys.ae.encrypt(AMOUNT_B).into(),
        pending_balance_lo: keys.elgamal.pubkey().encrypt(AMOUNT_B).into(),
        pending_balance_credit_counter: 1.into(),
        ..Default::default()
    };
    refund_from_state(&source, RefundAmount::None).unwrap();
}

#[test]
fn refund_binds_the_recipient_snapshot() {
    let keys = refund_keys();
    let source = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        available_balance: keys.elgamal.pubkey().encrypt(AMOUNT_B).into(),
        decryptable_available_balance: keys.ae.encrypt(AMOUNT_B).into(),
        ..Default::default()
    };
    let recipient_keys = EscrowKeys::from_seed(&[22; 32]).unwrap();
    let other = EscrowKeys::from_seed(&[23; 32]).unwrap();
    let (mint, destination) = (Pubkey::new_unique(), Pubkey::new_unique());
    let ct = ConfidentialTransferAccount {
        elgamal_pubkey: (*recipient_keys.elgamal.pubkey()).into(),
        approved: true.into(),
        allow_confidential_credits: true.into(),
        maximum_pending_balance_credit_counter: 1.into(),
        ..Default::default()
    };
    let account = crate::utils::ct_token_account(mint, ct);
    let mint_account = crate::utils::ct_mint_account();
    let mut system_owned = account.clone();
    system_owned.owner = Pubkey::default();
    let other_mint = crate::utils::ct_token_account(Pubkey::new_unique(), ct);
    let config = SessionConfig::new(
        Pubkey::new_unique(),
        TransactionFormat::V1,
        Default::default(),
    );
    let refund = |recipient, expected_recipient| {
        refund_session(
            &config,
            RefundInstruction::Reclaim(ReclaimConfidentialDvp {
                signer: config.payer,
                swap_dvp: Pubkey::new_unique(),
                mint,
                dvp_source_ata: Pubkey::new_unique(),
                signer_dest_ata: destination,
                token_program: TOKEN,
                memo_program: MEMO_PROGRAM_ID,
                zk_elgamal_proof_program: solana_zk_elgamal_proof_interface::ID,
                equality_context: None,
                validity_context: None,
                range_context: None,
                zero_context: None,
            }),
            Some(RefundRequest {
                source: TransferSource {
                    state: &source,
                    keys: &keys,
                    history: &[],
                },
                recipient,
                expected_recipient,
                amount: RefundAmount::Partial(1),
                mint: crate::utils::mint_account(mint, &mint_account),
                expected_auditor: None,
            }),
            &[],
            &[],
        )
    };
    let bound = crate::utils::recipient_account(destination, &account);

    assert!(refund(bound, None).is_ok());
    assert!(refund(bound, Some(recipient_keys.elgamal.pubkey())).is_ok());
    for (recipient, expected, error) in [
        (
            crate::utils::recipient_account(Pubkey::new_unique(), &account),
            None,
            "recipient address or owner",
        ),
        (
            crate::utils::recipient_account(destination, &system_owned),
            None,
            "recipient address or owner",
        ),
        (
            crate::utils::recipient_account(destination, &other_mint),
            None,
            "recipient mint",
        ),
        (bound, Some(other.elgamal.pubkey()), "recipient ElGamal key"),
    ] {
        assert!(matches!(
            refund(recipient, expected),
            Err(ConfidentialError::Account(message)) if message == error
        ));
    }
}

#[test]
fn refund_rejects_full_above_transfer_limit() {
    assert!(matches!(
        refund_from_snapshot(MAX_TRANSFER_AMOUNT + 1, RefundAmount::Full),
        Err(ConfidentialError::TransferAmountTooLarge(amount))
            if amount == MAX_TRANSFER_AMOUNT + 1
    ));
}

pub(super) fn reclaim_leg_b_accounts(f: &ClientFixture) -> ReclaimConfidentialDvp {
    ReclaimConfidentialDvp {
        signer: f.dvp.user_b.pubkey(),
        swap_dvp: f.dvp.accounts.swap_dvp,
        mint: f.dvp.accounts.mint_b,
        dvp_source_ata: f.dvp.accounts.dvp_ata_b,
        signer_dest_ata: f.refund,
        token_program: TOKEN,
        memo_program: MEMO_PROGRAM_ID,
        zk_elgamal_proof_program: solana_zk_elgamal_proof_interface::ID,
        equality_context: None,
        validity_context: None,
        range_context: None,
        zero_context: None,
    }
}

fn refund_from_snapshot(
    available: u64,
    amount: RefundAmount,
) -> Result<TransactionSession, ConfidentialError> {
    // Consistent local ciphertexts reach refund validation without ledger setup or transfers.
    let keys = refund_keys();
    let source = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        available_balance: keys.elgamal.pubkey().encrypt(available).into(),
        decryptable_available_balance: keys.ae.encrypt(available).into(),
        ..Default::default()
    };
    refund_from_state(&source, amount)
}

fn refund_keys() -> EscrowKeys {
    EscrowKeys::from_seed(&[21; 32]).unwrap()
}

fn refund_from_state(
    source: &ConfidentialTransferAccount,
    amount: RefundAmount,
) -> Result<TransactionSession, ConfidentialError> {
    let keys = refund_keys();
    let config = SessionConfig::new(
        Pubkey::new_unique(),
        TransactionFormat::V1,
        Default::default(),
    );
    let recipient = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        approved: true.into(),
        allow_confidential_credits: true.into(),
        maximum_pending_balance_credit_counter: 1.into(),
        ..Default::default()
    };
    let (mint, destination) = (Pubkey::new_unique(), Pubkey::new_unique());
    let recipient = crate::utils::ct_token_account(mint, recipient);
    let mint_account = crate::utils::ct_mint_account();
    let accounts = ReclaimConfidentialDvp {
        signer: config.payer,
        swap_dvp: Pubkey::new_unique(),
        mint,
        dvp_source_ata: Pubkey::new_unique(),
        signer_dest_ata: destination,
        token_program: TOKEN,
        memo_program: MEMO_PROGRAM_ID,
        zk_elgamal_proof_program: solana_zk_elgamal_proof_interface::ID,
        equality_context: None,
        validity_context: None,
        range_context: None,
        zero_context: None,
    };
    refund_session(
        &config,
        RefundInstruction::Reclaim(accounts),
        Some(RefundRequest {
            source: TransferSource {
                state: source,
                keys: &keys,
                history: &[],
            },
            recipient: crate::utils::recipient_account(destination, &recipient),
            expected_recipient: None,
            amount,
            mint: crate::utils::mint_account(mint, &mint_account),
            expected_auditor: None,
        }),
        &[],
        &[],
    )
}
