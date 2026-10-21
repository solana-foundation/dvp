use crate::{
    confidential_utils::{
        assert_contexts_closed, assert_failure, available, fund_late_b, mint_public, mutate_ct,
        pending, prepare_refund, reclaim, send_v1, setup_large_refund, setup_refund, state, Refund,
        LO_BITS,
    },
    state_utils::AMOUNT_A,
    utils::{
        dvp_ata, get_token_balance, TestContext, TOKEN_2022_PROGRAM_ID as TOKEN, TOKEN_PROGRAM_ID,
    },
};
use dvp_swap_program_client::DvpSwapProgramError as Error;
use solana_instruction::{error::InstructionError, AccountMeta};
use solana_pubkey::Pubkey;
use solana_signer::Signer;

const AMOUNT_B: u64 = (3 << LO_BITS) + 42;
const PARTIAL_B: u64 = 19;
const LATE_B: u64 = 7;
const PUBLIC_B: u64 = 11;

#[test]
fn full_reclaim_preserves_credit_arriving_after_proof_preparation() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, 0, AMOUNT_B, false);
    let swap = context.get_account(&f.accounts.swap_dvp);
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_b.pubkey(),
        AMOUNT_B,
        AMOUNT_B,
        true,
    );
    fund_late_b(&mut context, &f, LATE_B);
    let before = state(&context, &f.accounts.dvp_ata_b);
    send_v1(
        &mut context,
        &[reclaim(&f, false, &refund, &[])],
        &[&f.user_b],
    )
    .unwrap();
    assert_contexts_closed(&context, &refund);
    assert_eq!(context.get_account(&f.accounts.swap_dvp), swap);
    assert_eq!(available(&context, &f), 0);
    let after = state(&context, &f.accounts.dvp_ata_b);
    assert_eq!(after.pending_balance_lo, before.pending_balance_lo);
    assert_eq!(after.pending_balance_hi, before.pending_balance_hi);
    assert_eq!(u64::from(after.pending_balance_credit_counter), 1);
    assert_eq!(pending(&context, &f.accounts.dvp_ata_b, &f.keys), LATE_B);
    assert_eq!(
        pending(
            &context,
            &dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN),
            &keys
        ),
        AMOUNT_B
    );
    send_v1(
        &mut context,
        &[f.apply(f.user_b.pubkey(), LATE_B).instruction()],
        &[&f.user_b],
    )
    .unwrap();
    assert_eq!(available(&context, &f), LATE_B);
}

#[test]
fn leg_a_reclaims_independently_of_unavailable_leg_b_and_keeps_trade_open() {
    let mut context = TestContext::new();
    let (f, _) = setup_refund(&mut context, AMOUNT_A + 17, AMOUNT_B, false);
    let destination_b = dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN);
    mutate_ct(&mut context, destination_b, |ct| {
        ct.allow_confidential_credits = false.into()
    });
    context.advance_clock(3601);
    let before = context.get_account(&f.accounts.swap_dvp).unwrap();
    let escrow_b = context.get_account(&f.accounts.dvp_ata_b).unwrap();
    send_v1(
        &mut context,
        &[reclaim(&f, true, &Refund::none(), &[])],
        &[&f.user_a],
    )
    .unwrap();
    assert_eq!(get_token_balance(&context, &f.accounts.dvp_ata_a), 0);
    assert_eq!(
        get_token_balance(
            &context,
            &dvp_ata(&f.user_a.pubkey(), &f.accounts.mint_a, &TOKEN_PROGRAM_ID)
        ),
        AMOUNT_A + 17
    );
    assert_eq!(context.get_account(&f.accounts.swap_dvp).unwrap(), before);
    assert_eq!(
        context.get_account(&f.accounts.dvp_ata_b).unwrap(),
        escrow_b
    );
    send_v1(
        &mut context,
        &[reclaim(&f, true, &Refund::none(), &[])],
        &[&f.user_a],
    )
    .unwrap();
}

#[test]
fn partial_then_full_refunds_close_contexts_and_allow_funding_again() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, 0, AMOUNT_B, false);
    let swap = context.get_account(&f.accounts.swap_dvp).unwrap();
    for (balance, amount, full) in [
        (AMOUNT_B, PARTIAL_B, false),
        (AMOUNT_B - PARTIAL_B, AMOUNT_B - PARTIAL_B, true),
    ] {
        let refund = prepare_refund(
            &mut context,
            &f,
            &keys,
            f.user_b.pubkey(),
            balance,
            amount,
            full,
        );
        let rent: u64 = refund
            .contexts
            .iter()
            .flatten()
            .map(|a| context.get_account(a).unwrap().lamports)
            .sum();
        let before = context
            .get_account(&f.user_b.pubkey())
            .map_or(0, |a| a.lamports);
        send_v1(
            &mut context,
            &[reclaim(&f, false, &refund, &[])],
            &[&f.user_b],
        )
        .unwrap();
        assert_contexts_closed(&context, &refund);
        assert_eq!(
            context.get_account(&f.user_b.pubkey()).unwrap().lamports,
            before + rent
        );
        assert_eq!(available(&context, &f), balance - amount);
    }
    assert_eq!(
        state(&context, &f.accounts.dvp_ata_b).available_balance,
        Default::default()
    );
    assert_eq!(context.get_account(&f.accounts.swap_dvp).unwrap(), swap);
    // Use a separate donor: the refund recipient now has its refund in pending.
    fund_late_b(&mut context, &f, LATE_B);
    send_v1(
        &mut context,
        &[f.apply(f.user_b.pubkey(), LATE_B).instruction()],
        &[&f.user_b],
    )
    .unwrap();
    assert_eq!(available(&context, &f), LATE_B);
}

#[test]
fn public_withdrawal_requires_reset_available_but_not_empty_pending() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, 0, AMOUNT_B, false);
    mint_public(&mut context, &f, PUBLIC_B);
    assert_failure(
        &mut context,
        reclaim(&f, false, &Refund::none(), &[]),
        &f.user_b,
        InstructionError::Custom(Error::LegBRefundRequired as u32),
        true,
    );
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_b.pubkey(),
        AMOUNT_B,
        AMOUNT_B,
        true,
    );
    send_v1(
        &mut context,
        &[reclaim(&f, false, &refund, &[])],
        &[&f.user_b],
    )
    .unwrap();
    assert_eq!(get_token_balance(&context, &f.accounts.dvp_ata_b), PUBLIC_B);
    fund_late_b(&mut context, &f, LATE_B);
    send_v1(
        &mut context,
        &[reclaim(&f, false, &Refund::none(), &[])],
        &[&f.user_b],
    )
    .unwrap();
    assert_eq!(get_token_balance(&context, &f.accounts.dvp_ata_b), 0);
    assert_eq!(
        get_token_balance(
            &context,
            &dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN)
        ),
        PUBLIC_B
    );
    assert_eq!(pending(&context, &f.accounts.dvp_ata_b, &f.keys), LATE_B);
}

#[test]
fn validates_reclaim_party_leg_mode_and_contexts() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, false);
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_b.pubkey(),
        AMOUNT_B,
        PARTIAL_B,
        false,
    );
    assert_failure(
        &mut context,
        reclaim(&f, true, &refund, &[]),
        &f.user_a,
        InstructionError::InvalidInstructionData,
        true,
    );
    let ix = reclaim(&f, false, &refund, &[]);
    let mut bad = ix.clone();
    bad.accounts[0] = AccountMeta::new(f.authority.pubkey(), true);
    assert_failure(
        &mut context,
        bad,
        &f.authority,
        InstructionError::Custom(Error::SignerNotParty as u32),
        true,
    );
    for (position, expected) in [
        (2, InstructionError::InvalidAccountData),
        (3, InstructionError::InvalidSeeds),
        (4, InstructionError::InvalidSeeds),
        (5, InstructionError::IncorrectProgramId),
        (7, InstructionError::IncorrectProgramId),
    ] {
        let mut bad = ix.clone();
        bad.accounts[position].pubkey = Pubkey::new_unique();
        assert_failure(&mut context, bad, &f.user_b, expected, true);
    }
    let mut bad = ix.clone();
    bad.accounts[11] = bad.accounts[8].clone();
    assert_failure(
        &mut context,
        bad,
        &f.user_b,
        InstructionError::InvalidInstructionData,
        true,
    );
    let mut bad = ix.clone();
    bad.accounts[8].is_writable = false;
    assert_failure(
        &mut context,
        bad,
        &f.user_b,
        InstructionError::InvalidAccountData,
        true,
    );
    let mut bad = ix.clone();
    bad.accounts
        .extend(vec![AccountMeta::new_readonly(TOKEN, false); 33]);
    assert_failure(
        &mut context,
        bad,
        &f.user_b,
        InstructionError::InvalidArgument,
        true,
    );
    send_v1(&mut context, &[ix], &[&f.user_b]).unwrap();
}

#[test]
fn handles_two_credits_of_two_to_the_47() {
    let mut context = TestContext::new();
    let (f, keys, balance) = setup_large_refund(&mut context, false);
    let signer = &f.user_b;
    let partial = prepare_refund(
        &mut context,
        &f,
        &keys,
        signer.pubkey(),
        balance,
        balance - 1,
        false,
    );
    let ix = reclaim(&f, false, &partial, &[]);
    send_v1(&mut context, &[ix], &[signer]).unwrap();
    assert_contexts_closed(&context, &partial);
    assert_eq!(available(&context, &f), 1);
    let full = prepare_refund(&mut context, &f, &keys, f.user_b.pubkey(), 1, 1, true);
    let ix = reclaim(&f, false, &full, &[]);
    send_v1(&mut context, &[ix], &[&f.user_b]).unwrap();
    assert_contexts_closed(&context, &full);
    assert_eq!(
        state(&context, &f.accounts.dvp_ata_b).available_balance,
        Default::default()
    );
    assert!(context.get_account(&f.accounts.swap_dvp).is_some());
}

#[test]
fn hook_changed_after_funding_rejects_atomically() {
    use crate::confidential_utils::{send_and_assert_hook_rejection, switch_to_rejecting_hook};

    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, true);
    let signer = &f.user_b;
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
    let ix = reclaim(&f, false, &refund, &extras);
    send_and_assert_hook_rejection(&mut context, ix, signer);
    // Leg A remains independently reclaimable, even before expiry.
    let swap = context.get_account(&f.accounts.swap_dvp);
    let escrow_b = context.get_account(&f.accounts.dvp_ata_b);
    send_v1(
        &mut context,
        &[reclaim(&f, true, &Refund::none(), &[])],
        &[&f.user_a],
    )
    .unwrap();
    assert_eq!(context.get_account(&f.accounts.swap_dvp), swap);
    assert_eq!(context.get_account(&f.accounts.dvp_ata_b), escrow_b);
    assert_eq!(get_token_balance(&context, &f.accounts.dvp_ata_a), 0);
    assert_eq!(
        get_token_balance(
            &context,
            &dvp_ata(&f.user_a.pubkey(), &f.accounts.mint_a, &TOKEN_PROGRAM_ID)
        ),
        AMOUNT_A
    );
}
