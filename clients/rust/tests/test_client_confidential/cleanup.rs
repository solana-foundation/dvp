use crate::{
    confidential_utils::{ConfidentialDvpFixture, MAX_PENDING},
    utils::{cleanup, send_plan, ClientFixture, TestContext},
};
use dvp_swap_program_client::confidential::test_utils::{
    checked_available_balance, transfer_session, TransferAccounts, TransferRequest, AMOUNT_LO_BITS,
    RECORD_PROGRAM_ID,
};
use dvp_swap_program_client::{confidential::*, DvpSwapProgramError};
use solana_address::Address;
use solana_instruction::error::InstructionError;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::TransactionError;
use solana_zk_elgamal_proof_interface::instruction::ProofInstruction;
use spl_record::instruction::RecordInstruction;
use spl_token_2022_interface::extension::confidential_transfer::ConfidentialTransferAccount;
use std::collections::BTreeMap;

const SWAP_AMOUNT: u64 = (3 << AMOUNT_LO_BITS) + 42;

#[derive(Clone, Copy)]
enum PreparationStage {
    RecordCreated,
    RecordWritten,
    ProofVerified,
}

#[test]
fn client_cleans_up_interrupted_preparations_in_both_formats() {
    for format in [TransactionFormat::V0, TransactionFormat::V1] {
        let mut context = TestContext::new();
        let f = funded_fixture(&mut context, format);
        let available = checked_available_balance(&f.source(&context), &f.keys).unwrap();
        let stages: &[PreparationStage] = match format {
            TransactionFormat::V0 => &[
                PreparationStage::RecordCreated,
                PreparationStage::RecordWritten,
                PreparationStage::ProofVerified,
            ],
            TransactionFormat::V1 => &[PreparationStage::ProofVerified],
        };
        for stage in stages {
            let source = f.source(&context);
            let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
            let recipient = context.get_account(&f.recipient).unwrap();
            let refund = context.get_account(&f.refund).unwrap();
            let swap = f.swap(&context);
            let session = settle_session(
                &f.config,
                f.settle_accounts(),
                SettleRequest {
                    source: TransferSource {
                        state: &source,
                        keys: &f.keys,
                        history: &[],
                    },
                    swap: &swap,
                    expected_amount_b: SWAP_AMOUNT,
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
            .unwrap();

            // Send preparation only through the selected stage, leaving final unsent.
            let last = session
                .preparation
                .iter()
                .position(|plan| {
                    plan.instructions.iter().any(|ix| match stage {
                        PreparationStage::RecordCreated => {
                            ix.program_id == RECORD_PROGRAM_ID
                                && matches!(
                                    RecordInstruction::unpack(&ix.data),
                                    Ok(RecordInstruction::Initialize)
                                )
                        }
                        PreparationStage::RecordWritten => {
                            ix.program_id == RECORD_PROGRAM_ID
                                && matches!(
                                    RecordInstruction::unpack(&ix.data),
                                    Ok(RecordInstruction::Write { .. })
                                )
                        }
                        PreparationStage::ProofVerified => {
                            ix.program_id == solana_zk_elgamal_proof_interface::ID
                                && (format == TransactionFormat::V1
                                    || ix.data.len() == 1 + core::mem::size_of::<u32>())
                                && ix.data.first()
                                    != Some(&(ProofInstruction::CloseContextState as u8))
                        }
                    })
                })
                .unwrap();
            for plan in &session.preparation[..=last] {
                send_plan(&mut context, &f.config, plan, &f.dvp.authority).unwrap();
            }

            // Cleanup checks closure and rent recipients; escrow funds must stay intact.
            cleanup(&mut context, &f.config, &session, &f.dvp.authority);
            assert_eq!(
                checked_available_balance(&f.source(&context), &f.keys).unwrap(),
                available
            );
        }
    }
}

#[test]
fn client_cleanup_returns_rent_to_shared_payer_and_authority_in_both_formats() {
    for format in [TransactionFormat::V0, TransactionFormat::V1] {
        let mut context = TestContext::new();
        let authority = context.payer.insecure_clone();
        let (config, session) = prepared_cleanup_session(&mut context, format, &authority);

        // One wallet receives both Record and context rent and pays all cleanup fees.
        cleanup(&mut context, &config, &session, &authority);
    }
}

#[test]
fn client_cleanup_resumes_after_partial_cleanup_in_both_formats() {
    for format in [TransactionFormat::V0, TransactionFormat::V1] {
        let mut context = TestContext::new();
        let authority = Keypair::new();
        let (config, session) = prepared_cleanup_session(&mut context, format, &authority);
        let live: Vec<_> = session
            .cleanup
            .iter()
            .filter(|ix| context.get_account(&ix.accounts[0].pubkey).is_some())
            .collect();
        assert!(live.len() > 1);

        // Close one account, verify its rent return, then interrupt cleanup.
        let first = live[0];
        let address = first.accounts[0].pubkey;
        let rent = context.get_account(&address).unwrap().lamports;
        let destination = if first.program_id == RECORD_PROGRAM_ID {
            config.payer
        } else {
            authority.pubkey()
        };
        let before = context.get_account(&destination).map_or(0, |a| a.lamports);
        let meta = send_plan(
            &mut context,
            &config,
            &PlannedTransaction {
                instructions: vec![first.clone()],
                signers: vec![],
            },
            &authority,
        )
        .unwrap();
        assert!(context.get_account(&address).is_none());
        let fee = if destination == config.payer {
            meta.fee
        } else {
            0
        };
        assert_eq!(
            context.get_account(&destination).unwrap().lamports + fee,
            before + rent
        );
        assert!(live
            .iter()
            .skip(1)
            .all(|ix| context.get_account(&ix.accounts[0].pubkey).is_some()));

        // Resume from current account existence; the already closed account is skipped.
        cleanup(&mut context, &config, &session, &authority);
        let payer_after = context.get_account(&config.payer).unwrap();
        let authority_after = context.get_account(&authority.pubkey()).unwrap();

        // A further helper invocation has nothing to send and charges no fees.
        cleanup(&mut context, &config, &session, &authority);
        assert_eq!(context.get_account(&config.payer).unwrap(), payer_after);
        assert_eq!(
            context.get_account(&authority.pubkey()).unwrap(),
            authority_after
        );
    }
}

#[test]
fn client_rejects_wrong_high_limb_atomically_and_cleans_up_in_both_formats() {
    for format in [TransactionFormat::V0, TransactionFormat::V1] {
        let mut context = TestContext::new();
        let f = funded_fixture(&mut context, format);
        let source = f.source(&context);
        let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
        let available = checked_available_balance(&source, &f.keys).unwrap();
        let recipient = context.get_account(&f.recipient).unwrap();
        let refund = context.get_account(&f.refund).unwrap();

        // Build valid proofs for local terms whose high limb differs from the on-chain swap.
        let mut swap = f.swap(&context);
        let wrong_amount = SWAP_AMOUNT + (1 << AMOUNT_LO_BITS);
        swap.amount_b = f.keys.encrypt_amount(wrong_amount).unwrap();
        let session = settle_session(
            &f.config,
            f.settle_accounts(),
            SettleRequest {
                source: TransferSource {
                    state: &source,
                    keys: &f.keys,
                    history: &[],
                },
                swap: &swap,
                expected_amount_b: wrong_amount,
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
        .unwrap();
        for plan in &session.preparation {
            send_plan(&mut context, &f.config, plan, &f.dvp.authority).unwrap();
        }

        // Final must fail for the amount mismatch without changing accounts (except the fee payer).
        let before: BTreeMap<_, _> = session
            .final_transaction
            .instructions
            .iter()
            .flat_map(|ix| &ix.accounts)
            .filter(|a| a.pubkey != f.config.payer)
            .map(|a| (a.pubkey, context.get_account(&a.pubkey)))
            .collect();
        let error = send_plan(
            &mut context,
            &f.config,
            &session.final_transaction,
            &f.dvp.authority,
        )
        .unwrap_err();
        let instructions = session
            .final_transaction
            .message(&f.config, context.svm.latest_blockhash())
            .unwrap();
        let index = instructions
            .instructions()
            .iter()
            .position(|ix| {
                instructions.static_account_keys()[ix.program_id_index as usize]
                    == dvp_swap_program_client::DVP_SWAP_PROGRAM_ID
            })
            .unwrap() as u8;
        assert_eq!(
            error.err,
            TransactionError::InstructionError(
                index,
                InstructionError::Custom(DvpSwapProgramError::ConfidentialAmountBMismatch as u32),
            )
        );
        for (address, account) in before {
            assert_eq!(context.get_account(&address), account);
        }

        // Proof accounts survive the rejected final and must still be reclaimable.
        cleanup(&mut context, &f.config, &session, &f.dvp.authority);
        assert_eq!(
            checked_available_balance(&f.source(&context), &f.keys).unwrap(),
            available
        );
    }
}

#[test]
fn client_cleanup_rejects_wrong_authority_in_both_formats() {
    for format in [TransactionFormat::V0, TransactionFormat::V1] {
        let mut context = TestContext::new();
        let authority = Keypair::new();
        let (config, session) = prepared_cleanup_session(&mut context, format, &authority);
        let wrong_authority = Keypair::new();
        context
            .svm
            .airdrop(&wrong_authority.pubkey(), config.rent.minimum_balance(0))
            .unwrap();

        // Keep the client's close instruction and rent destination; replace only its signer.
        for close in session
            .cleanup
            .iter()
            .filter(|ix| context.get_account(&ix.accounts[0].pubkey).is_some())
            .cloned()
            .collect::<Vec<_>>()
        {
            let address = close.accounts[0].pubkey;
            let before = context.get_account(&address).unwrap();
            let payer_before = context.get_account(&config.payer).unwrap().lamports;
            let authority_before = context.get_account(&authority.pubkey());
            let expected_error = if close.program_id == RECORD_PROGRAM_ID {
                InstructionError::Custom(spl_record::error::RecordError::IncorrectAuthority as u32)
            } else {
                assert_eq!(close.program_id, solana_zk_elgamal_proof_interface::ID);
                InstructionError::InvalidAccountOwner
            };
            let mut invalid = close;
            invalid
                .accounts
                .iter_mut()
                .find(|meta| meta.is_signer)
                .unwrap()
                .pubkey = wrong_authority.pubkey();
            let plan = PlannedTransaction {
                instructions: vec![invalid],
                signers: vec![],
            };
            let message = plan
                .message(&config, context.svm.latest_blockhash())
                .unwrap();
            // The close follows any format-specific compute budget instructions.
            let index = u8::try_from(message.instructions().len() - 1).unwrap();
            let error = send_plan(&mut context, &config, &plan, &wrong_authority).unwrap_err();
            assert_eq!(
                error.err,
                TransactionError::InstructionError(index, expected_error)
            );
            assert_eq!(context.get_account(&address).unwrap(), before);
            assert_eq!(context.get_account(&authority.pubkey()), authority_before);
            assert_eq!(
                context.get_account(&config.payer).unwrap().lamports + error.meta.fee,
                payer_before
            );
        }

        // Failed unauthorized closes must not prevent the legitimate owner from reclaiming rent.
        cleanup(&mut context, &config, &session, &authority);
    }
}

fn funded_fixture(context: &mut TestContext, format: TransactionFormat) -> ClientFixture {
    let fixture = ConfidentialDvpFixture::new(context, true, false);
    ClientFixture::with_fixture(
        context,
        format,
        SWAP_AMOUNT + 65_537,
        fixture,
        SWAP_AMOUNT,
        false,
        MAX_PENDING,
    )
}

fn prepared_cleanup_session(
    context: &mut TestContext,
    format: TransactionFormat,
    authority: &Keypair,
) -> (SessionConfig, TransactionSession) {
    let config = SessionConfig::new(context.payer.pubkey(), format, context.svm.get_sysvar());
    if authority.pubkey() != config.payer {
        context
            .svm
            .airdrop(&authority.pubkey(), config.rent.minimum_balance(0))
            .unwrap();
    }
    // Local transfer snapshots avoid DvP funding; all temporary accounts are created by real preparations.
    let keys = EscrowKeys::from_seed(&[21; 32]).unwrap();
    let source = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        available_balance: keys.elgamal.pubkey().encrypt(SWAP_AMOUNT).into(),
        decryptable_available_balance: keys.ae.encrypt(SWAP_AMOUNT).into(),
        ..Default::default()
    };
    let recipient = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        approved: true.into(),
        allow_confidential_credits: true.into(),
        maximum_pending_balance_credit_counter: 1.into(),
        ..Default::default()
    };
    let session = transfer_session(
        &config,
        TransferAccounts {
            authority: authority.pubkey(),
            source: Address::new_unique(),
            mint: Address::new_unique(),
            destination: Address::new_unique(),
        },
        TransferRequest {
            source: TransferSource {
                state: &source,
                keys: &keys,
                history: &[],
            },
            recipient: &recipient,
            amount: SWAP_AMOUNT,
            auditor: None,
        },
        &[],
    )
    .unwrap();

    // v0 stops with both a live Record and verified contexts; v1 prepares its inline proofs.
    let last = match format {
        TransactionFormat::V0 => session
            .preparation
            .iter()
            .position(|plan| {
                plan.instructions.iter().any(|ix| {
                    ix.program_id == RECORD_PROGRAM_ID
                        && matches!(
                            RecordInstruction::unpack(&ix.data),
                            Ok(RecordInstruction::Initialize)
                        )
                })
            })
            .unwrap(),
        TransactionFormat::V1 => session.preparation.len() - 1,
    };
    for plan in &session.preparation[..=last] {
        send_plan(context, &config, plan, authority).unwrap();
    }
    let live: Vec<_> = session
        .cleanup
        .iter()
        .filter(|ix| context.get_account(&ix.accounts[0].pubkey).is_some())
        .collect();
    assert!(live
        .iter()
        .any(|ix| ix.program_id == solana_zk_elgamal_proof_interface::ID));
    assert_eq!(
        live.iter().any(|ix| ix.program_id == RECORD_PROGRAM_ID),
        format == TransactionFormat::V0
    );
    (config, session)
}
