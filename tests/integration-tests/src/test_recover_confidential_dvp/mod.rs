use crate::{
    confidential_utils::{
        assert_contexts_closed, assert_failure, available, fund_late_b, mint_public, mutate_ct,
        pending, prepare_refund, reclaim, recover, send_v1, setup_large_refund, setup_refund,
        state, terminal, Refund, LO_BITS,
    },
    state_utils::AMOUNT_A,
    utils::{
        create_ata, dvp_ata, get_token_balance, hook_extras_for_mint, TestContext,
        TOKEN_2022_PROGRAM_ID as TOKEN,
    },
};
use dvp_swap_program_client::DvpSwapProgramError as Error;
use solana_instruction::{error::InstructionError, AccountMeta};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token_2022_interface::instruction as token_ix;

const AMOUNT_B: u64 = (3 << LO_BITS) + 42;
const PARTIAL_B: u64 = 19;
const LATE_B: u64 = 7;
const PUBLIC_B: u64 = 11;

#[test]
fn full_terminal_refund_preserves_late_pending_then_apply_and_recover_drain_it() {
    for (public, late_recover) in [(0, 0), (PUBLIC_B, 0), (0, LATE_B), (PUBLIC_B, LATE_B)] {
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
        fund_late_b(&mut context, &f, LATE_B);
        if public > 0 {
            mint_public(&mut context, &f, public);
        }
        send_v1(
            &mut context,
            &[terminal(&f, f.authority.pubkey(), &refund, true, &[])],
            &[&f.authority],
        )
        .unwrap();
        assert_eq!(available(&context, &f), 0);
        assert_ne!(
            state(&context, &f.accounts.dvp_ata_b).available_balance,
            Default::default()
        );
        assert_eq!(pending(&context, &f.accounts.dvp_ata_b, &f.keys), LATE_B);
        assert_failure(
            &mut context,
            recover(&f, &Refund::none(), &[]),
            &f.user_b,
            InstructionError::Custom(Error::LegBRefundRequired as u32),
            true,
        );
        // Even an encryption of zero needs Full(0) when pending prevented reset.
        let zero = prepare_refund(&mut context, &f, &keys, f.user_b.pubkey(), 0, 0, true);
        send_v1(&mut context, &[recover(&f, &zero, &[])], &[&f.user_b]).unwrap();
        assert_contexts_closed(&context, &zero);
        assert_eq!(pending(&context, &f.accounts.dvp_ata_b, &f.keys), LATE_B);
        send_v1(
            &mut context,
            &[f.apply(f.user_b.pubkey(), LATE_B).instruction()],
            &[&f.user_b],
        )
        .unwrap();
        let refund = prepare_refund(
            &mut context,
            &f,
            &keys,
            f.user_b.pubkey(),
            LATE_B,
            LATE_B,
            true,
        );
        if late_recover > 0 {
            fund_late_b(&mut context, &f, late_recover);
        }
        let before = state(&context, &f.accounts.dvp_ata_b);
        send_v1(&mut context, &[recover(&f, &refund, &[])], &[&f.user_b]).unwrap();
        assert_contexts_closed(&context, &refund);
        if late_recover > 0 {
            assert_eq!(available(&context, &f), 0);
            let after = state(&context, &f.accounts.dvp_ata_b);
            assert_eq!(after.pending_balance_lo, before.pending_balance_lo);
            assert_eq!(after.pending_balance_hi, before.pending_balance_hi);
            assert_eq!(u64::from(after.pending_balance_credit_counter), 1);
            assert_eq!(
                pending(&context, &f.accounts.dvp_ata_b, &f.keys),
                late_recover
            );
            send_v1(
                &mut context,
                &[f.apply(f.user_b.pubkey(), late_recover).instruction()],
                &[&f.user_b],
            )
            .unwrap();
            let final_refund = prepare_refund(
                &mut context,
                &f,
                &keys,
                f.user_b.pubkey(),
                late_recover,
                late_recover,
                true,
            );
            send_v1(
                &mut context,
                &[recover(&f, &final_refund, &[])],
                &[&f.user_b],
            )
            .unwrap();
            assert_contexts_closed(&context, &final_refund);
        }
        if public > 0 {
            assert_eq!(get_token_balance(&context, &f.accounts.dvp_ata_b), public);
            send_v1(
                &mut context,
                &[recover(&f, &Refund::none(), &[])],
                &[&f.user_b],
            )
            .unwrap();
        }
        assert!(context.get_account(&f.accounts.dvp_ata_b).is_none());
        assert!(context.get_account(&f.accounts.nonce_tombstone).is_some());
        assert_eq!(
            pending(
                &context,
                &dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN),
                &keys
            ),
            AMOUNT_B + LATE_B + late_recover
        );
    }
}

#[test]
fn partial_that_drains_available_still_needs_full_zero_to_reset_and_close() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, 0, AMOUNT_B, false);
    let balance = AMOUNT_B;
    let max_transfer = AMOUNT_B - PARTIAL_B;
    // Partial may itself empty the available balance, but has no zero/reset proof.
    let first = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_a.pubkey(),
        balance,
        max_transfer,
        false,
    );
    send_v1(
        &mut context,
        &[terminal(&f, f.user_a.pubkey(), &first, false, &[])],
        &[&f.user_a],
    )
    .unwrap();
    assert_eq!(available(&context, &f), PARTIAL_B);
    let second = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_b.pubkey(),
        PARTIAL_B,
        PARTIAL_B,
        false,
    );
    send_v1(&mut context, &[recover(&f, &second, &[])], &[&f.user_b]).unwrap();
    assert_eq!(available(&context, &f), 0);
    assert_ne!(
        state(&context, &f.accounts.dvp_ata_b).available_balance,
        Default::default()
    );
    let last = prepare_refund(&mut context, &f, &keys, f.user_b.pubkey(), 0, 0, true);
    let before = context.get_account(&f.user_b.pubkey()).unwrap().lamports;
    let rent: u64 = last
        .contexts
        .iter()
        .flatten()
        .chain(std::iter::once(&f.accounts.dvp_ata_b))
        .map(|a| context.get_account(a).unwrap().lamports)
        .sum();
    send_v1(&mut context, &[recover(&f, &last, &[])], &[&f.user_b]).unwrap();
    assert_eq!(
        context.get_account(&f.user_b.pubkey()).unwrap().lamports,
        before + rent
    );
    assert!(context.get_account(&f.accounts.dvp_ata_b).is_none());
    for refund in [first, second, last] {
        assert_contexts_closed(&context, &refund);
    }
}

#[test]
fn recover_authenticates_closed_swap_seed_parties_tombstone_and_ct_escrow() {
    let mut context = TestContext::new();
    let (f, _) = setup_refund(&mut context, 0, 0, false);
    let ix = recover(&f, &Refund::none(), &[]);
    assert_failure(
        &mut context,
        ix.clone(),
        &f.user_b,
        InstructionError::Custom(Error::DvpStillOpen as u32),
        true,
    );
    mint_public(&mut context, &f, PUBLIC_B);
    send_v1(
        &mut context,
        &[terminal(
            &f,
            f.authority.pubkey(),
            &Refund::none(),
            true,
            &[],
        )],
        &[&f.authority],
    )
    .unwrap();
    let mut bad = ix.clone();
    bad.accounts[0] = AccountMeta::new(f.user_a.pubkey(), true);
    assert_failure(
        &mut context,
        bad,
        &f.user_a,
        InstructionError::Custom(Error::SignerNotParty as u32),
        true,
    );
    let mut bad = ix.clone();
    bad.accounts[0].is_signer = false;
    assert_failure(
        &mut context,
        bad,
        &f.user_b,
        InstructionError::MissingRequiredSignature,
        true,
    );
    for (position, expected) in [
        (1, InstructionError::InvalidSeeds),
        (2, InstructionError::InvalidAccountData),
        (3, InstructionError::InvalidAccountData),
        (4, InstructionError::InvalidSeeds),
        (5, InstructionError::InvalidSeeds),
        (6, InstructionError::IncorrectProgramId),
        (8, InstructionError::IncorrectProgramId),
    ] {
        let mut bad = ix.clone();
        bad.accounts[position].pubkey = Pubkey::new_unique();
        assert_failure(&mut context, bad, &f.user_b, expected, true);
    }
    // Alter each seed independently; a real tombstone must authenticate all of them.
    for offset in [1, 1 + 32, 1 + 64, 1 + 96, 1 + 128, 1 + 160] {
        let mut bad = ix.clone();
        bad.data[offset] ^= 1;
        let expected = if offset == 1 + 64 {
            InstructionError::Custom(Error::SignerNotParty as u32)
        } else if offset == 1 + 128 {
            InstructionError::InvalidAccountData
        } else {
            InstructionError::InvalidSeeds
        };
        assert_failure(&mut context, bad, &f.user_b, expected, true);
    }
    let tombstone = context.get_account(&f.accounts.nonce_tombstone).unwrap();
    context
        .svm
        .set_account(
            f.accounts.nonce_tombstone,
            solana_account::Account::default(),
        )
        .unwrap();
    assert_failure(
        &mut context,
        ix.clone(),
        &f.user_b,
        InstructionError::Custom(Error::DvpNeverCreated as u32),
        true,
    );
    context
        .svm
        .set_account(f.accounts.nonce_tombstone, tombstone)
        .unwrap();
    let escrow = context.get_account(&f.accounts.dvp_ata_b).unwrap();
    // An otherwise valid ordinary Token-2022 ATA cannot enter the CT recovery path.
    let ordinary = create_ata(
        &mut context,
        &Pubkey::new_unique(),
        &f.accounts.mint_b,
        &TOKEN,
    );
    context
        .svm
        .set_account(
            f.accounts.dvp_ata_b,
            context.get_account(&ordinary).unwrap(),
        )
        .unwrap();
    assert_failure(
        &mut context,
        ix.clone(),
        &f.user_b,
        InstructionError::Custom(Error::EscrowNotConfidential as u32),
        true,
    );
    context
        .svm
        .set_account(f.accounts.dvp_ata_b, escrow)
        .unwrap();
    send_v1(&mut context, &[ix], &[&f.user_b]).unwrap();
}

#[test]
fn memo_and_hooks_work_for_ct_refunds_and_public_recovery() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, true);
    let destination = dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN);
    let enable = spl_token_2022_interface::extension::memo_transfer::instruction::enable_required_transfer_memos(
        &TOKEN, &destination, &f.user_b.pubkey(), &[]).unwrap();
    let resize = token_ix::reallocate(
        &TOKEN,
        &destination,
        &context.payer.pubkey(),
        &f.user_b.pubkey(),
        &[],
        &[spl_token_2022_interface::extension::ExtensionType::MemoTransfer],
    )
    .unwrap();
    send_v1(&mut context, &[resize, enable], &[&f.user_b]).unwrap();
    let extras = hook_extras_for_mint(&f.accounts.mint_b);
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.authority.pubkey(),
        AMOUNT_B,
        AMOUNT_B,
        true,
    );
    mint_public(&mut context, &f, PUBLIC_B);
    let ix = terminal(&f, f.authority.pubkey(), &refund, true, &extras);
    let result = send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
    assert!(result.logs.iter().any(|s| s.contains("memo required")));
    assert!(result.compute_units_consumed < 250_000);
    let ix = recover(&f, &Refund::none(), &extras);
    mutate_ct(&mut context, destination, |ct| {
        ct.allow_non_confidential_credits = false.into()
    });
    assert_failure(
        &mut context,
        ix.clone(),
        &f.user_b,
        InstructionError::Custom(
            spl_token_2022_interface::error::TokenError::NonConfidentialTransfersDisabled as u32,
        ),
        false,
    );
    mutate_ct(&mut context, destination, |ct| {
        ct.allow_non_confidential_credits = true.into()
    });
    let result = send_v1(&mut context, &[ix], &[&f.user_b]).unwrap();
    assert!(result.logs.iter().any(|s| s.contains("memo required")));
    assert!(context.get_account(&f.accounts.dvp_ata_b).is_none());
    assert_eq!(get_token_balance(&context, &destination), PUBLIC_B);
}

#[test]
fn reclaim_reject_and_recover_forward_hook_extras_for_ct_refunds() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, 0, AMOUNT_B, true);
    let extras = hook_extras_for_mint(&f.accounts.mint_b);
    let partial = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_b.pubkey(),
        AMOUNT_B,
        PARTIAL_B,
        false,
    );
    let missing = reclaim(&f, false, &partial, &[]);
    let before = context.get_account(&f.accounts.dvp_ata_b);
    let failure = send_v1(&mut context, &[missing], &[&f.user_b]).unwrap_err();
    assert_eq!(
        failure.err,
        solana_transaction::TransactionError::InstructionError(0, InstructionError::MissingAccount)
    );
    assert!(failure
        .meta
        .logs
        .contains(&format!("Program {TOKEN} invoke [2]")));
    assert_eq!(context.get_account(&f.accounts.dvp_ata_b), before);
    send_v1(
        &mut context,
        &[reclaim(&f, false, &partial, &extras)],
        &[&f.user_b],
    )
    .unwrap();
    let partial = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_a.pubkey(),
        AMOUNT_B - PARTIAL_B,
        PARTIAL_B,
        false,
    );
    send_v1(
        &mut context,
        &[terminal(&f, f.user_a.pubkey(), &partial, false, &extras)],
        &[&f.user_a],
    )
    .unwrap();
    let remaining = AMOUNT_B - PARTIAL_B * 2;
    let full = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_b.pubkey(),
        remaining,
        remaining,
        true,
    );
    send_v1(&mut context, &[recover(&f, &full, &extras)], &[&f.user_b]).unwrap();
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
fn handles_two_credits_of_two_to_the_47() {
    let mut context = TestContext::new();
    let (f, keys, balance) = setup_large_refund(&mut context, true);
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
    let ix = recover(&f, &partial, &[]);
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
fn hook_changed_after_funding_rejects_atomically() {
    use crate::confidential_utils::{send_and_assert_hook_rejection, switch_to_rejecting_hook};

    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, true);
    let partial = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.authority.pubkey(),
        AMOUNT_B,
        PARTIAL_B,
        false,
    );
    let ix = terminal(
        &f,
        f.authority.pubkey(),
        &partial,
        true,
        &hook_extras_for_mint(&f.accounts.mint_b),
    );
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
    assert_contexts_closed(&context, &partial);
    assert!(context.get_account(&f.accounts.swap_dvp).is_none());
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
    let extras = switch_to_rejecting_hook(&mut context, &f.accounts.mint_b);
    send_and_assert_hook_rejection(&mut context, recover(&f, &refund, &extras), &f.user_b);
}
