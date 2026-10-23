use crate::{
    confidential_utils::{
        assert_contexts_closed, assert_failure, available, fund_b, fund_late_b, mint_public,
        pending, prepare_refund, recover, send_v1, setup_large_refund, setup_refund, state,
        terminal, ConfidentialDvpFixture, Refund, DECIMALS, LO_BITS,
    },
    state_utils::AMOUNT_A,
    utils::{
        dvp_ata, get_token_balance, nonce_tombstone_pda, swap_dvp_pda, TestContext,
        TOKEN_2022_PROGRAM_ID as TOKEN, TOKEN_PROGRAM_ID,
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
fn full_reject_preserves_credit_arriving_after_proof_preparation() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, false);
    let refund = prepare_refund(
        &mut context,
        &f,
        &keys,
        f.user_a.pubkey(),
        AMOUNT_B,
        AMOUNT_B,
        true,
    );
    fund_late_b(&mut context, &f, LATE_B);
    let before = state(&context, &f.accounts.dvp_ata_b);
    let rent: u64 = [f.accounts.swap_dvp, f.accounts.dvp_ata_a]
        .iter()
        .chain(refund.contexts.iter().flatten())
        .map(|a| context.get_account(a).unwrap().lamports)
        .sum();
    let signer_lamports = context.get_account(&f.user_a.pubkey()).unwrap().lamports;
    send_v1(
        &mut context,
        &[terminal(&f, f.user_a.pubkey(), &refund, false, &[])],
        &[&f.user_a],
    )
    .unwrap();
    assert_contexts_closed(&context, &refund);
    assert!(context.get_account(&f.accounts.swap_dvp).is_none());
    assert!(context.get_account(&f.accounts.dvp_ata_a).is_none());
    assert!(context.get_account(&f.accounts.nonce_tombstone).is_some());
    assert_eq!(
        context.get_account(&f.user_a.pubkey()).unwrap().lamports,
        signer_lamports + rent
    );
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
    assert_eq!(
        get_token_balance(
            &context,
            &dvp_ata(&f.user_a.pubkey(), &f.accounts.mint_a, &TOKEN_PROGRAM_ID)
        ),
        AMOUNT_A
    );
}

#[test]
fn refunds_both_legs_after_expiry_and_pays_rent_to_each_authorized_signer() {
    for reject_by_user_a in [true, false] {
        let mut context = TestContext::new();
        let (f, keys) = setup_refund(&mut context, AMOUNT_A + 17, AMOUNT_B, false);
        let signer = if reject_by_user_a {
            &f.user_a
        } else {
            &f.user_b
        };
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
        let ix = terminal(&f, signer.pubkey(), &refund, false, &[]);
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
}

#[test]
fn partial_terminal_refund_leaves_remainder_for_real_recover() {
    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, false);
    let signer = &f.user_a;
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
        &[terminal(&f, signer.pubkey(), &refund, false, &[])],
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
fn none_closes_empty_escrow_b_but_preserves_pending_and_public_tokens() {
    for (late, public) in [(0, 0), (LATE_B, 0), (0, PUBLIC_B), (LATE_B, PUBLIC_B)] {
        let mut context = TestContext::new();
        let (f, keys) = setup_refund(&mut context, 0, 0, false);
        if late > 0 {
            fund_b(&mut context, &f, &keys, late, false);
        }
        if public > 0 {
            mint_public(&mut context, &f, public);
        }
        // Unfunded leg A needs no destination account.
        let destination_a = dvp_ata(&f.user_a.pubkey(), &f.accounts.mint_a, &TOKEN_PROGRAM_ID);
        context
            .svm
            .set_account(destination_a, solana_account::Account::default())
            .unwrap();
        send_v1(
            &mut context,
            &[terminal(&f, f.user_a.pubkey(), &Refund::none(), false, &[])],
            &[&f.user_a],
        )
        .unwrap();
        assert!(context.get_account(&f.accounts.swap_dvp).is_none());
        if late == 0 && public == 0 {
            assert!(context.get_account(&f.accounts.dvp_ata_b).is_none());
        } else {
            assert_eq!(pending(&context, &f.accounts.dvp_ata_b, &f.keys), late);
            assert_eq!(get_token_balance(&context, &f.accounts.dvp_ata_b), public);
        }
    }
}

#[test]
fn authorization_account_binding_and_hook_limits_match_terminal_contract() {
    let mut context = TestContext::new();
    let (f, _) = setup_refund(&mut context, AMOUNT_A, 0, false);
    let signer = &f.user_a;
    let ix = terminal(&f, signer.pubkey(), &Refund::none(), false, &[]);
    let mut unsigned = ix.clone();
    unsigned.accounts[0].is_signer = false;
    assert_failure(
        &mut context,
        unsigned,
        signer,
        InstructionError::MissingRequiredSignature,
        true,
    );
    let wrong = &f.authority;
    assert_failure(
        &mut context,
        terminal(&f, wrong.pubkey(), &Refund::none(), false, &[]),
        wrong,
        InstructionError::Custom(Error::SignerNotParty as u32),
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
    let signer = &f.user_a;
    let partial = prepare_refund(
        &mut context,
        &f,
        &keys,
        signer.pubkey(),
        balance,
        balance - 1,
        false,
    );
    let ix = terminal(&f, signer.pubkey(), &partial, false, &[]);
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
fn none_unwinds_empty_escrow_after_real_mint_close_and_recreation() {
    use solana_keypair::Keypair;
    use solana_program_pack::Pack;
    use spl_token_2022_interface::{
        extension::ExtensionType, instruction as token_ix, state::Mint,
    };

    let mut context = TestContext::new();
    let mut f = ConfidentialDvpFixture::new(&mut context, true, false);
    let mint = Keypair::new();
    let payer = context.payer.pubkey();
    let space =
        ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::MintCloseAuthority])
            .unwrap();
    let rent = context.svm.minimum_balance_for_rent_exemption(space);
    send_v1(
        &mut context,
        &[
            solana_system_interface::instruction::create_account(
                &payer,
                &mint.pubkey(),
                rent,
                space as u64,
                &TOKEN,
            ),
            token_ix::initialize_mint_close_authority(&TOKEN, &mint.pubkey(), Some(&payer))
                .unwrap(),
            token_ix::initialize_mint2(&TOKEN, &mint.pubkey(), &payer, None, DECIMALS).unwrap(),
        ],
        &[&mint],
    )
    .unwrap();
    f.accounts.mint_a = mint.pubkey();
    f.accounts.token_program_a = TOKEN;
    let swap = swap_dvp_pda(
        &f.authority.pubkey(),
        &f.user_a.pubkey(),
        &f.user_b.pubkey(),
        &mint.pubkey(),
        &f.accounts.mint_b,
        f.args.nonce,
    )
    .0;
    f.accounts.swap_dvp = swap;
    f.accounts.nonce_tombstone = nonce_tombstone_pda(&swap).0;
    f.accounts.dvp_ata_a = dvp_ata(&swap, &mint.pubkey(), &TOKEN);
    f.accounts.dvp_ata_b = dvp_ata(&swap, &f.accounts.mint_b, &TOKEN);
    f.create(&mut context);
    send_v1(
        &mut context,
        &[token_ix::close_account(&TOKEN, &mint.pubkey(), &payer, &payer, &[]).unwrap()],
        &[],
    )
    .unwrap();
    assert!(context.get_account(&mint.pubkey()).is_none());
    let rent = context.svm.minimum_balance_for_rent_exemption(Mint::LEN);
    send_v1(
        &mut context,
        &[
            solana_system_interface::instruction::create_account(
                &payer,
                &mint.pubkey(),
                rent,
                Mint::LEN as u64,
                &TOKEN_PROGRAM_ID,
            ),
            spl_token_interface::instruction::initialize_mint2(
                &TOKEN_PROGRAM_ID,
                &mint.pubkey(),
                &payer,
                None,
                DECIMALS,
            )
            .unwrap(),
        ],
        &[&mint],
    )
    .unwrap();
    assert_eq!(
        context.get_account(&mint.pubkey()).unwrap().owner,
        TOKEN_PROGRAM_ID
    );
    // The old empty ATA is still owned by Token-2022 and can be closed without its mint.
    assert_eq!(
        context.get_account(&f.accounts.dvp_ata_a).unwrap().owner,
        TOKEN
    );
    send_v1(
        &mut context,
        &[terminal(&f, f.user_a.pubkey(), &Refund::none(), false, &[])],
        &[&f.user_a],
    )
    .unwrap();
    for address in [swap, f.accounts.dvp_ata_a, f.accounts.dvp_ata_b] {
        assert!(context.get_account(&address).is_none());
    }
    assert!(context.get_account(&f.accounts.nonce_tombstone).is_some());
}

#[test]
fn hook_changed_after_funding_rejects_atomically() {
    use crate::confidential_utils::{send_and_assert_hook_rejection, switch_to_rejecting_hook};

    let mut context = TestContext::new();
    let (f, keys) = setup_refund(&mut context, AMOUNT_A, AMOUNT_B, true);
    let signer = &f.user_a;
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
    let ix = terminal(&f, signer.pubkey(), &refund, false, &extras);
    send_and_assert_hook_rejection(&mut context, ix, signer);
}
