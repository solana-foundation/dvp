use crate::{
    confidential_utils::{
        assert_contexts_closed, assert_failure, available, context_account, fund_b, mutate_ct,
        pending, prepare_refund, recover, send_v1, set_mint_auditor, setup_large_refund,
        setup_refund, terminal, Refund, LO_BITS,
    },
    state_utils::AMOUNT_A,
    utils::{
        dvp_ata, get_token_balance, TestContext, TOKEN_2022_PROGRAM_ID as TOKEN, TOKEN_PROGRAM_ID,
    },
};
use dvp_swap_program_client::{DvpSwapProgramError as Error, DVP_SWAP_PROGRAM_ID};
use solana_instruction::{error::InstructionError, AccountMeta};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_zk_elgamal_proof_interface::instruction::ProofInstruction;
use solana_zk_sdk::zk_elgamal_proof_program::build_zero_ciphertext_proof_data;

const AMOUNT_B: u64 = (3 << LO_BITS) + 42;
const PARTIAL_B: u64 = 19;
const TERMINAL_CONTEXTS: usize = 12;

#[test]
fn refunds_both_legs_after_expiry_and_pays_rent_to_each_authorized_signer() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A + 17, AMOUNT_B, false);
    let signer = &f.authority;
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        signer.pubkey(),
        AMOUNT_B,
        AMOUNT_B,
        true,
    );
    context.advance_clock(3601);
    let ix = terminal(&f, signer.pubkey(), &refund, true, &[]);
    let closed: Vec<_> = [
        f.accounts.swap_dvp,
        f.accounts.dvp_ata_a,
        f.accounts.dvp_ata_b,
    ]
    .into_iter()
    .chain(refund.contexts.iter().flatten().copied())
    .collect();
    let rent: u64 = closed
        .iter()
        .map(|a| context.get_account(a).unwrap().lamports)
        .sum();
    let before = context
        .get_account(&signer.pubkey())
        .map_or(0, |a| a.lamports);
    let result = send_v1(&mut context, &[ix], &[signer]).unwrap();
    assert!(result.compute_units_consumed < 200_000);
    for key in closed {
        assert!(context.get_account(&key).is_none());
    }
    assert_eq!(
        context.get_account(&signer.pubkey()).unwrap().lamports,
        before + rent
    );
    assert_eq!(
        get_token_balance(
            &context,
            &dvp_ata(&f.user_a.pubkey(), &f.accounts.mint_a, &TOKEN_PROGRAM_ID)
        ),
        AMOUNT_A + 17
    );
    assert_eq!(
        pending(
            &context,
            &dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN),
            &keys
        ),
        AMOUNT_B
    );
    assert!(context.get_account(&f.accounts.nonce_tombstone).is_some());
}

#[test]
fn partial_terminal_refund_leaves_remainder_for_real_recover() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, false);
    let signer = &f.authority;
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        signer.pubkey(),
        AMOUNT_B,
        PARTIAL_B,
        false,
    );
    send_v1(
        &mut context,
        &[terminal(&f, signer.pubkey(), &refund, true, &[])],
        &[signer],
    )
    .unwrap();
    assert_contexts_closed(&context, &refund);
    assert!(context.get_account(&f.accounts.swap_dvp).is_none());
    assert!(context.get_account(&f.accounts.dvp_ata_a).is_none());
    assert_eq!(available(&context, &f), AMOUNT_B - PARTIAL_B);
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_b.pubkey(),
        AMOUNT_B - PARTIAL_B,
        AMOUNT_B - PARTIAL_B,
        true,
    );
    send_v1(&mut context, &[recover(&f, &refund, &[])], &[&f.user_b]).unwrap();
    assert_contexts_closed(&context, &refund);
    assert!(context.get_account(&f.accounts.dvp_ata_b).is_none());
    assert_eq!(
        pending(
            &context,
            &dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN),
            &keys
        ),
        AMOUNT_B
    );
}

#[test]
fn rejects_invalid_contexts_and_recipient_before_refunding_leg_a() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, false);
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.authority.pubkey(),
        AMOUNT_B,
        AMOUNT_B,
        true,
    );
    let ix = terminal(&f, f.authority.pubkey(), &refund, true, &[]);
    for position in TERMINAL_CONTEXTS..TERMINAL_CONTEXTS + 4 {
        let mut readonly = ix.clone();
        readonly.accounts[position].is_writable = false;
        assert_failure(
            &mut context,
            readonly,
            &f.authority,
            InstructionError::InvalidAccountData,
            true,
        );
        let key = ix.accounts[position].pubkey;
        let original = context.get_account(&key).unwrap();
        for expected in [
            Error::InvalidProofContext,
            Error::ProofContextAuthorityMismatch,
        ] {
            let mut bad = original.clone();
            if expected == Error::InvalidProofContext {
                bad.owner = DVP_SWAP_PROGRAM_ID;
            } else {
                bad.data[..32].copy_from_slice(Pubkey::new_unique().as_ref());
            }
            context.svm.set_account(key, bad).unwrap();
            assert_failure(
                &mut context,
                ix.clone(),
                &f.authority,
                InstructionError::Custom(expected as u32),
                true,
            );
        }
        context.svm.set_account(key, original).unwrap();
        let mut missing = ix.clone();
        missing.accounts[position] = AccountMeta::new_readonly(DVP_SWAP_PROGRAM_ID, false);
        assert_failure(
            &mut context,
            missing,
            &f.authority,
            InstructionError::InvalidInstructionData,
            true,
        );
    }
    let destination = ix.accounts[7].pubkey;
    let original = context.get_account(&destination).unwrap();
    for (case, expected) in [
        (0, Error::RecipientNotApproved),
        (1, Error::RecipientConfidentialCreditsDisabled),
        (2, Error::RecipientPendingCounterFull),
    ] {
        mutate_ct(&mut context, destination, |ct| match case {
            0 => ct.approved = false.into(),
            1 => ct.allow_confidential_credits = false.into(),
            _ => ct.pending_balance_credit_counter = ct.maximum_pending_balance_credit_counter,
        });
        assert_failure(
            &mut context,
            ix.clone(),
            &f.authority,
            InstructionError::Custom(expected as u32),
            true,
        );
        context
            .svm
            .set_account(destination, original.clone())
            .unwrap();
    }
    assert_failure(
        &mut context,
        terminal(&f, f.authority.pubkey(), &Refund::none(), true, &[]),
        &f.authority,
        InstructionError::Custom(Error::LegBRefundRequired as u32),
        true,
    );
    // A correctly verified zero ciphertext is not enough: it must be the actual
    // post-refund ciphertext. Failure after both transfers must roll everything back.
    let zero =
        build_zero_ciphertext_proof_data(&f.keys.elgamal, &f.keys.elgamal.pubkey().encrypt(0u64))
            .unwrap();
    let wrong_zero = context_account(
        &mut context,
        &f.authority.pubkey(),
        ProofInstruction::VerifyZeroCiphertext,
        &zero,
    );
    let mut bad = ix.clone();
    bad.accounts[TERMINAL_CONTEXTS + 3].pubkey = wrong_zero;
    assert_failure(
        &mut context,
        bad,
        &f.authority,
        InstructionError::Custom(Error::EscrowBalanceNotZero as u32),
        false,
    );
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
}

#[test]
fn authorization_account_binding_and_hook_limits_match_terminal_contract() {
    let mut context = TestContext::new();
    let (f, _) = setup_refund(&mut context, AMOUNT_A, 0, false);
    let signer = &f.authority;
    let ix = terminal(&f, signer.pubkey(), &Refund::none(), true, &[]);
    let mut unsigned = ix.clone();
    unsigned.accounts[0].is_signer = false;
    assert_failure(
        &mut context,
        unsigned,
        signer,
        InstructionError::MissingRequiredSignature,
        true,
    );
    let wrong = &f.user_a;
    assert_failure(
        &mut context,
        terminal(&f, wrong.pubkey(), &Refund::none(), true, &[]),
        wrong,
        InstructionError::Custom(Error::SettlementAuthorityMismatch as u32),
        true,
    );
    for (position, expected) in [
        (2, InstructionError::InvalidAccountData),
        (3, InstructionError::InvalidAccountData),
        (4, InstructionError::InvalidSeeds),
        (5, InstructionError::InvalidSeeds),
        (6, InstructionError::InvalidSeeds),
        (7, InstructionError::InvalidSeeds),
        (8, InstructionError::IncorrectProgramId),
        (9, InstructionError::IncorrectProgramId),
        (11, InstructionError::IncorrectProgramId),
    ] {
        let mut bad = ix.clone();
        bad.accounts[position].pubkey = Pubkey::new_unique();
        assert_failure(&mut context, bad, signer, expected, true);
    }
    let mut bad = ix.clone();
    bad.data[1] = 1;
    assert_failure(
        &mut context,
        bad,
        signer,
        InstructionError::InvalidInstructionData,
        true,
    );
    let mut bad = ix.clone();
    bad.accounts
        .extend(vec![AccountMeta::new_readonly(TOKEN, false); 33]);
    assert_failure(
        &mut context,
        bad,
        signer,
        InstructionError::InvalidInstructionData,
        true,
    );
    send_v1(&mut context, &[ix], &[signer]).unwrap();
}

#[test]
fn handles_two_credits_of_two_to_the_47() {
    let mut context = TestContext::new();
    let (f, keys, balance) = setup_large_refund(&mut context, false);
    let signer = &f.authority;
    let partial = prepare_refund(
        &mut context,
        &f,
        &keys,
        signer.pubkey(),
        balance,
        balance - 1,
        false,
    );
    let ix = terminal(&f, signer.pubkey(), &partial, true, &[]);
    send_v1(&mut context, &[ix], &[signer]).unwrap();
    assert_contexts_closed(&context, &partial);
    assert_eq!(available(&context, &f), 1);
    let full = prepare_refund(&mut context, &f, &keys, f.user_b.pubkey(), 1, 1, true);
    let ix = recover(&f, &full, &[]);
    send_v1(&mut context, &[ix], &[&f.user_b]).unwrap();
    assert_contexts_closed(&context, &full);
    assert!(context.get_account(&f.accounts.dvp_ata_b).is_none());
}

#[test]
fn full_refund_with_mint_auditor_closes_escrows_and_proofs() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, 0, 0, false);
    let auditor = solana_zk_sdk::encryption::elgamal::ElGamalKeypair::new_rand();
    set_mint_auditor(&mut context, &f.accounts.mint_b, auditor.pubkey());
    fund_b(&mut context, &f, &keys, AMOUNT_B, false);
    send_v1(
        &mut context,
        &[f.apply(f.user_b.pubkey(), AMOUNT_B).instruction()],
        &[&f.user_b],
    )
    .unwrap();
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.authority.pubkey(),
        AMOUNT_B,
        AMOUNT_B,
        true,
    );
    send_v1(
        &mut context,
        &[terminal(&f, f.authority.pubkey(), &refund, true, &[])],
        &[&f.authority],
    )
    .unwrap();
    assert_contexts_closed(&context, &refund);
    assert_eq!(
        pending(
            &context,
            &dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN),
            &keys
        ),
        AMOUNT_B
    );
    for address in [
        f.accounts.swap_dvp,
        f.accounts.dvp_ata_a,
        f.accounts.dvp_ata_b,
    ] {
        assert!(context.get_account(&address).is_none());
    }
    assert!(context.get_account(&f.accounts.nonce_tombstone).is_some());
}

#[test]
fn hook_changed_after_funding_rejects_atomically() {
    use crate::confidential_utils::{send_and_assert_hook_rejection, switch_to_rejecting_hook};

    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, true);
    let signer = &f.authority;
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        signer.pubkey(),
        AMOUNT_B,
        AMOUNT_B,
        true,
    );
    let extras = switch_to_rejecting_hook(&mut context, &f.accounts.mint_b);
    let ix = terminal(&f, signer.pubkey(), &refund, true, &extras);
    send_and_assert_hook_rejection(&mut context, ix, signer);
}
