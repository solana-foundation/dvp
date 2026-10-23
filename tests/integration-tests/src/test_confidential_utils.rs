use dvp_swap_program_client::DvpSwapProgramError as Error;
use solana_account::Account;
use solana_instruction::AccountMeta;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_zk_elgamal_proof_interface::state::ProofContextStateMeta;
use solana_zk_sdk_pod::encryption::{
    auth_encryption::PodAeCiphertext, elgamal::PodElGamalCiphertext,
};
use spl_token_2022_interface::{
    extension::{
        confidential_transfer::ConfidentialTransferAccount, BaseStateWithExtensionsMut,
        StateWithExtensionsMut,
    },
    state::Account as TokenAccount,
};

use crate::{
    confidential_utils::*,
    utils::{MEMO_PROGRAM_ID, TOKEN_2022_PROGRAM_ID},
};

// Nonzero high and low limbs catch accidental swaps or truncation.
const AMOUNT: u64 = (3 << LO_BITS) + 42;
const PUBKEY_LEN: usize = core::mem::size_of::<Pubkey>();
const CONTEXT_HEADER_LEN: usize = core::mem::size_of::<ProofContextStateMeta>();
const AE_CIPHERTEXT_LEN: usize = core::mem::size_of::<PodAeCiphertext>();
const ELGAMAL_CIPHERTEXT_LEN: usize = core::mem::size_of::<PodElGamalCiphertext>();

fn snapshot(
    ct_context: &ConfidentialTransferTestContext,
    payment: &Payment,
) -> Vec<(Pubkey, Option<Account>)> {
    [
        ct_context.escrow,
        ct_context.destination,
        ct_context.proof_authority.pubkey(),
    ]
    .into_iter()
    .chain(payment.contexts)
    .map(|address| (address, ct_context.context.get_account(&address)))
    .collect()
}

fn assert_unchanged(
    ct_context: &ConfidentialTransferTestContext,
    before: &[(Pubkey, Option<Account>)],
) {
    for (address, account) in before {
        assert_eq!(
            &ct_context.context.get_account(address),
            account,
            "{address}"
        );
    }
}

fn assert_error(
    ct_context: &mut ConfidentialTransferTestContext,
    payment: &Payment,
    expected: Error,
    before_cpi: bool,
) {
    let before = snapshot(ct_context, payment);
    let failure = ct_context.pay(payment).unwrap_err();
    assert!(
        format!("{:?}", failure.err).contains(&format!("Custom({})", expected as u32)),
        "{failure:?}"
    );
    if before_cpi {
        assert!(
            !failure
                .meta
                .logs
                .iter()
                .any(|line| line.contains("invoke [2]")),
            "{:?}",
            failure.meta.logs
        );
    }
    assert_unchanged(ct_context, &before);
}

fn assert_contexts_closed(
    ct_context: &ConfidentialTransferTestContext,
    payment: &Payment,
    rent_before: u64,
) {
    for address in payment.contexts {
        assert!(ct_context
            .context
            .get_account(&address)
            .is_none_or(|a| a.lamports == 0));
    }
    assert_eq!(
        ct_context
            .context
            .get_account(&ct_context.proof_authority.pubkey())
            .unwrap()
            .lamports,
        rent_before
    );
}

fn context_rent(ct_context: &ConfidentialTransferTestContext, payment: &Payment) -> u64 {
    payment
        .contexts
        .iter()
        .map(|a| ct_context.context.get_account(a).unwrap().lamports)
        .sum::<u64>()
        + ct_context
            .context
            .get_account(&ct_context.proof_authority.pubkey())
            .map_or(0, |a| a.lamports)
}

#[test]
fn test_confidential_pda_transfer_reset_and_context_rent() {
    let mut ct_context = ConfidentialTransferTestContext::new(AMOUNT, false);
    let configured = state(&ct_context.context, &ct_context.escrow);
    assert!(bool::from(configured.approved));
    assert!(!bool::from(configured.allow_non_confidential_credits));
    assert_eq!(
        u64::from(configured.maximum_pending_balance_credit_counter),
        MAX_PENDING
    );
    assert_eq!(
        pending(
            &ct_context.context,
            &ct_context.escrow,
            &ct_context.escrow_keys
        ),
        0
    );
    let payment = ct_context.prepare(AMOUNT);
    let rent = context_rent(&ct_context, &payment);
    let result = ct_context.pay(&payment).unwrap();
    assert!(!result
        .logs
        .iter()
        .any(|line| line.contains(&format!("{MEMO_PROGRAM_ID} invoke"))));
    assert_eq!(
        pending(
            &ct_context.context,
            &ct_context.destination,
            &ct_context.recipient_keys
        ),
        AMOUNT
    );
    assert_eq!(
        state(&ct_context.context, &ct_context.escrow).available_balance,
        Default::default()
    );
    assert_contexts_closed(&ct_context, &payment, rent);
}

#[test]
fn test_confidential_pending_credit_does_not_invalidate_prepared_payment() {
    let mut ct_context = ConfidentialTransferTestContext::new(AMOUNT, false);
    let payment = ct_context.prepare(AMOUNT);
    ct_context.fund(7);
    let before = state(&ct_context.context, &ct_context.escrow);
    let rent = context_rent(&ct_context, &payment);
    ct_context.pay(&payment).unwrap();
    let after = state(&ct_context.context, &ct_context.escrow);
    assert_eq!(after.pending_balance_lo, before.pending_balance_lo);
    assert_eq!(after.pending_balance_hi, before.pending_balance_hi);
    assert_eq!(
        after.pending_balance_credit_counter,
        before.pending_balance_credit_counter
    );
    assert_eq!(
        pending(
            &ct_context.context,
            &ct_context.escrow,
            &ct_context.escrow_keys
        ),
        7
    );
    // EmptyAccount was skipped: the encrypted zero is not the all-zero sentinel.
    assert_ne!(after.available_balance, Default::default());
    let available: solana_zk_sdk::encryption::elgamal::ElGamalCiphertext =
        after.available_balance.try_into().unwrap();
    assert_eq!(
        available.decrypt_u32(ct_context.escrow_keys.elgamal.secret()),
        Some(0)
    );
    assert_eq!(
        pending(
            &ct_context.context,
            &ct_context.destination,
            &ct_context.recipient_keys
        ),
        AMOUNT
    );
    assert_contexts_closed(&ct_context, &payment, rent);
}

#[test]
fn test_confidential_rejects_invalid_contexts_before_any_cpi() {
    let mut ct_context = ConfidentialTransferTestContext::new(AMOUNT, false);
    let payment = ct_context.prepare(AMOUNT);
    // Exercise every expected context type, including the post-transfer zero proof.
    for address in payment.contexts {
        let original = ct_context.context.get_account(&address).unwrap();
        let mut variants = Vec::new();
        let mut wrong_owner = original.clone();
        wrong_owner.owner = solana_sdk_ids::system_program::ID;
        variants.push((wrong_owner, Error::InvalidProofContext));
        let mut short = original.clone();
        short.data.pop();
        variants.push((short, Error::InvalidProofContext));
        let mut long = original.clone();
        long.data.push(0);
        variants.push((long, Error::InvalidProofContext));
        let mut wrong_type = original.clone();
        wrong_type.data[PUBKEY_LEN] = u8::MAX;
        variants.push((wrong_type, Error::InvalidProofContext));
        let mut wrong_authority = original.clone();
        wrong_authority.data[..PUBKEY_LEN].copy_from_slice(Pubkey::new_unique().as_ref());
        variants.push((wrong_authority, Error::ProofContextAuthorityMismatch));
        for (account, expected) in variants {
            ct_context
                .context
                .svm
                .set_account(address, account)
                .unwrap();
            assert_error(&mut ct_context, &payment, expected, true);
        }
        ct_context
            .context
            .svm
            .set_account(address, original)
            .unwrap();
    }
    ct_context.pay(&payment).unwrap();
}

#[test]
fn test_confidential_amount_binding_and_zero_match() {
    let mut ct_context = ConfidentialTransferTestContext::new(AMOUNT, false);
    let mut payment = ct_context.prepare(AMOUNT);
    let original_data = payment.instruction.data.clone();
    // Fixture tag, AE balance, auditor lo + hi precede the stored ciphertexts.
    let stored_start = 1 + AE_CIPHERTEXT_LEN + 2 * ELGAMAL_CIPHERTEXT_LEN;
    for offset in [stored_start, stored_start + ELGAMAL_CIPHERTEXT_LEN] {
        payment.instruction.data[offset] ^= 1;
        assert_error(
            &mut ct_context,
            &payment,
            Error::ConfidentialAmountBMismatch,
            true,
        );
        payment.instruction.data.clone_from(&original_data);
    }
    // A second set contains valid proofs under the same key and for the same
    // amount, but randomized ciphertexts. It must not bind to the first set.
    let other = ct_context.prepare(AMOUNT);
    let validity_meta = payment.instruction.accounts[8].clone();
    payment.instruction.accounts[8].pubkey = other.contexts[3];
    assert_error(
        &mut ct_context,
        &payment,
        Error::ConfidentialAmountBMismatch,
        true,
    );
    payment.instruction.accounts[8] = validity_meta;

    let validity = payment.contexts[3];
    let original = ct_context.context.get_account(&validity).unwrap();
    let mut wrong_source_key = original.clone();
    wrong_source_key.data[CONTEXT_HEADER_LEN] ^= 1;
    ct_context
        .context
        .svm
        .set_account(validity, wrong_source_key)
        .unwrap();
    assert_error(
        &mut ct_context,
        &payment,
        Error::ConfidentialAmountBMismatch,
        true,
    );
    ct_context
        .context
        .svm
        .set_account(validity, original)
        .unwrap();

    // Corrupt each public key of the equality contexts; these are validation
    // tests, not a substitute for real verification in the successful path.
    for address in payment.contexts[..2].iter().copied() {
        let original = ct_context.context.get_account(&address).unwrap();
        for offset in [CONTEXT_HEADER_LEN, CONTEXT_HEADER_LEN + PUBKEY_LEN] {
            let mut account = original.clone();
            account.data[offset] ^= 1;
            ct_context
                .context
                .svm
                .set_account(address, account)
                .unwrap();
            assert_error(
                &mut ct_context,
                &payment,
                Error::ConfidentialAmountBMismatch,
                true,
            );
        }
        ct_context
            .context
            .svm
            .set_account(address, original)
            .unwrap();
    }
    // Another genuinely verified zero proof cannot reset this transfer's
    // different encrypted zero. Failure after Transfer must roll it back.
    let zero_meta = payment.instruction.accounts[10].clone();
    payment.instruction.accounts[10].pubkey = other.contexts[5];
    assert_error(
        &mut ct_context,
        &payment,
        Error::EscrowBalanceNotZero,
        false,
    );
    let foreign_keys = Keys::new();
    let zero = solana_zk_sdk::zk_elgamal_proof_program::build_zero_ciphertext_proof_data(
        &foreign_keys.elgamal,
        &foreign_keys.elgamal.pubkey().encrypt(0u64),
    )
    .unwrap();
    payment.instruction.accounts[10].pubkey = context_account(
        &mut ct_context.context,
        &ct_context.proof_authority.pubkey(),
        solana_zk_elgamal_proof_interface::instruction::ProofInstruction::VerifyZeroCiphertext,
        &zero,
    );
    assert_error(
        &mut ct_context,
        &payment,
        Error::EscrowBalanceNotZero,
        false,
    );
    payment.instruction.accounts[10] = zero_meta;
    ct_context.pay(&payment).unwrap();
}

#[test]
fn test_confidential_recipient_readiness_before_cpi() {
    let mut ct_context = ConfidentialTransferTestContext::new(AMOUNT, false);
    let payment = ct_context.prepare(AMOUNT);
    let original = ct_context
        .context
        .get_account(&ct_context.destination)
        .unwrap();
    for (change, expected) in [
        (0, Error::RecipientNotApproved),
        (1, Error::RecipientConfidentialCreditsDisabled),
        (2, Error::RecipientPendingCounterFull),
    ] {
        let mut account = original.clone();
        let mut state = StateWithExtensionsMut::<TokenAccount>::unpack(&mut account.data).unwrap();
        let ct = state
            .get_extension_mut::<ConfidentialTransferAccount>()
            .unwrap();
        match change {
            0 => ct.approved = false.into(),
            1 => ct.allow_confidential_credits = false.into(),
            _ => ct.pending_balance_credit_counter = ct.maximum_pending_balance_credit_counter,
        }
        ct_context
            .context
            .svm
            .set_account(ct_context.destination, account)
            .unwrap();
        assert_error(&mut ct_context, &payment, expected, true);
    }
    // A real base token account with no CT extension.
    let mut account = original.clone();
    account
        .data
        .truncate(spl_token_2022_interface::state::Account::LEN);
    ct_context
        .context
        .svm
        .set_account(ct_context.destination, account)
        .unwrap();
    assert_error(
        &mut ct_context,
        &payment,
        Error::RecipientNotConfidential,
        true,
    );
    ct_context
        .context
        .svm
        .set_account(ct_context.destination, original)
        .unwrap();
    ct_context.pay(&payment).unwrap();
}

#[test]
fn test_confidential_transfer_memo_and_hook() {
    let mut ct_context = ConfidentialTransferTestContext::new(AMOUNT, true);
    let instructions = [
        spl_token_2022_interface::instruction::reallocate(&TOKEN_2022_PROGRAM_ID, &ct_context.destination, &ct_context.context.payer.pubkey(), &ct_context.recipient.pubkey(), &[], &[spl_token_2022_interface::extension::ExtensionType::MemoTransfer]).unwrap(),
        spl_token_2022_interface::extension::memo_transfer::instruction::enable_required_transfer_memos(&TOKEN_2022_PROGRAM_ID, &ct_context.destination, &ct_context.recipient.pubkey(), &[]).unwrap(),
    ];
    send_v1(
        &mut ct_context.context,
        &instructions,
        &[&ct_context.recipient],
    )
    .unwrap();
    let extra = ct_context.proof_authority.pubkey();
    ct_context.context.svm.airdrop(&extra, 1_000_000).unwrap();
    ct_context.set_hook_extras(&[AccountMeta::new(extra, false)]);
    let mut payment = ct_context.prepare(AMOUNT);
    // The limit is per CT CPI, before the memo as well as the transfer.
    let extras_start = 14;
    let original = payment.instruction.accounts.clone();
    payment.instruction.accounts.resize(
        extras_start + 33,
        AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
    );
    let before = snapshot(&ct_context, &payment);
    let failure = ct_context.pay(&payment).unwrap_err();
    assert!(
        format!("{:?}", failure.err).contains("InvalidArgument"),
        "{failure:?}"
    );
    assert!(!failure
        .meta
        .logs
        .iter()
        .any(|line| line.contains("invoke [2]")));
    assert_unchanged(&ct_context, &before);
    payment.instruction.accounts = original;
    let result = ct_context.pay(&payment).unwrap();
    let memo_end = result
        .logs
        .iter()
        .position(|line| line == &format!("Program {MEMO_PROGRAM_ID} success"))
        .unwrap();
    assert_eq!(
        result.logs[memo_end + 1],
        format!("Program {TOKEN_2022_PROGRAM_ID} invoke [2]")
    );
    assert!(result
        .logs
        .iter()
        .any(|line| line.contains("hook accounts: 6")));
    assert!(result
        .logs
        .iter()
        .any(|line| line.contains(&format!("hook amount: {}", u64::MAX))));
    assert!(result
        .logs
        .iter()
        .any(|line| line.contains("hook first extra writable: 1, signer: 0")));
    assert_eq!(
        pending(
            &ct_context.context,
            &ct_context.destination,
            &ct_context.recipient_keys
        ),
        AMOUNT
    );
}

#[test]
fn test_confidential_hook_extras_cannot_receive_outer_signer() {
    let mut ct_context = ConfidentialTransferTestContext::new(AMOUNT, true);
    let victim = ct_context.proof_authority.pubkey();
    let attacker = Pubkey::new_unique();
    ct_context
        .context
        .svm
        .airdrop(&victim, 500_000_000)
        .unwrap();
    ct_context.set_hook_extras(&[
        AccountMeta::new(victim, true),
        AccountMeta::new(attacker, false),
        AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
    ]);
    let payment = ct_context.prepare(AMOUNT);
    let before = snapshot(&ct_context, &payment);
    let failure = ct_context.pay(&payment).unwrap_err();
    assert!(
        format!("{failure:?}").contains("signer privilege escalated"),
        "{failure:?}"
    );
    assert_unchanged(&ct_context, &before);
    assert!(ct_context
        .context
        .get_account(&attacker)
        .is_none_or(|a| a.lamports == 0));
}

#[test]
fn send_v1_rejects_oversized_transactions_without_execution() {
    let mut context = crate::utils::TestContext::new();
    let payer = context.payer.pubkey();
    let before = context.get_account(&payer);
    let ix = solana_instruction::Instruction {
        program_id: MEMO_PROGRAM_ID,
        accounts: vec![],
        data: vec![b'a'; solana_message::v1::MAX_TRANSACTION_SIZE],
    };
    let failure = send_v1(&mut context, &[ix], &[]).unwrap_err();
    assert_eq!(
        failure.err,
        solana_transaction::TransactionError::SanitizeFailure
    );
    assert!(failure.meta.logs[0].contains(&format!(
        "exceeds v1 limit {}",
        solana_message::v1::MAX_TRANSACTION_SIZE
    )));
    assert_eq!(failure.meta.compute_units_consumed, 0);
    assert_eq!(context.get_account(&payer), before);
}
