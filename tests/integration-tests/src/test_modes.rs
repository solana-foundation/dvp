//! Public/confidential mode guards for every lifecycle entry point.
use crate::{
    state_utils::{
        assert_create_dvp, assert_reject_dvp, setup_dvp, setup_dvp_with_programs, DvpFixture,
    },
    utils::{
        assert_instruction_error, assert_program_error, create_ata, TestContext, MEMO_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    },
};
use dvp_swap_program_client::{
    instructions::{
        ApplyConfidentialDvpBuilder, CancelConfidentialDvpBuilder, CancelDvpBuilder,
        ReclaimConfidentialDvpBuilder, ReclaimDvpBuilder, RecoverConfidentialDvpBuilder,
        RecoverDvpBuilder, RejectConfidentialDvpBuilder, RejectDvpBuilder,
        SettleConfidentialDvpBuilder, SettleDvpBuilder,
    },
    types::{CtTransferData, LegBRefund},
    DvpSwapProgramError,
};
use solana_instruction::{error::InstructionError, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use spl_token_2022_interface::{
    extension::{
        confidential_transfer::ConfidentialTransferAccount, BaseStateWithExtensionsMut,
        ExtensionType, PodStateWithExtensionsMut,
    },
    pod::{PodAccount, PodCOption},
    state::{Account as TokenAccount, AccountState},
};

fn assert_modes(
    context: &mut TestContext,
    cases: &[(&str, Instruction, &Keypair)],
    expected: InstructionError,
) {
    for (name, ix, signer) in cases {
        // Check every supplied account except the fee payer, including both escrows.
        let before: Vec<_> = ix
            .accounts
            .iter()
            .filter(|meta| meta.pubkey != context.payer.pubkey())
            .map(|meta| (meta.pubkey, context.get_account(&meta.pubkey)))
            .collect();
        let result = context.send(ix.clone(), &[*signer]);
        assert_instruction_error(
            result.map_err(|error| format!("{name}: {error}")),
            &format!("{expected:?}"),
        );
        for (address, account) in before {
            assert_eq!(context.get_account(&address), account, "{name}: {address}");
        }
    }
}

#[test]
fn public_instructions_reject_confidential_swap() {
    let mut context = TestContext::new();
    let f = setup_dvp(&mut context, 0);
    assert_create_dvp(&mut context, &f);
    let mut swap = context.get_account(&f.swap_dvp).unwrap();
    // The confidential layout adds two 64-byte ciphertexts to the public base.
    swap.data.extend_from_slice(&[0; 64 * 2]);
    swap.lamports = context
        .svm
        .minimum_balance_for_rent_exemption(swap.data.len());
    context.svm.set_account(f.swap_dvp, swap).unwrap();
    let cases = [
        (
            "Reclaim",
            ReclaimDvpBuilder::new()
                .signer(f.user_a.pubkey())
                .swap_dvp(f.swap_dvp)
                .mint(f.mint_a)
                .dvp_source_ata(f.dvp_ata_a)
                .signer_dest_ata(f.user_a_ata_a)
                .token_program(f.token_program_a)
                .memo_program(MEMO_PROGRAM_ID)
                .instruction(),
            &f.user_a,
        ),
        (
            "Settle",
            SettleDvpBuilder::new()
                .settlement_authority(f.settlement_authority.pubkey())
                .swap_dvp(f.swap_dvp)
                .mint_a(f.mint_a)
                .mint_b(f.mint_b)
                .dvp_ata_a(f.dvp_ata_a)
                .dvp_ata_b(f.dvp_ata_b)
                .user_a_destination_ata_b(f.user_a_ata_b)
                .user_b_destination_ata_a(f.user_b_ata_a)
                .user_a_ata_a(f.user_a_ata_a)
                .user_b_ata_b(f.user_b_ata_b)
                .token_program_a(f.token_program_a)
                .token_program_b(f.token_program_b)
                .memo_program(MEMO_PROGRAM_ID)
                .leg_a_extras_count(0)
                .instruction(),
            &f.settlement_authority,
        ),
        (
            "Cancel",
            CancelDvpBuilder::new()
                .settlement_authority(f.settlement_authority.pubkey())
                .swap_dvp(f.swap_dvp)
                .mint_a(f.mint_a)
                .mint_b(f.mint_b)
                .dvp_ata_a(f.dvp_ata_a)
                .dvp_ata_b(f.dvp_ata_b)
                .user_a_ata_a(f.user_a_ata_a)
                .user_b_ata_b(f.user_b_ata_b)
                .token_program_a(f.token_program_a)
                .token_program_b(f.token_program_b)
                .memo_program(MEMO_PROGRAM_ID)
                .leg_a_extras_count(0)
                .instruction(),
            &f.settlement_authority,
        ),
        (
            "Reject",
            RejectDvpBuilder::new()
                .signer(f.user_a.pubkey())
                .swap_dvp(f.swap_dvp)
                .mint_a(f.mint_a)
                .mint_b(f.mint_b)
                .dvp_ata_a(f.dvp_ata_a)
                .dvp_ata_b(f.dvp_ata_b)
                .user_a_ata_a(f.user_a_ata_a)
                .user_b_ata_b(f.user_b_ata_b)
                .token_program_a(f.token_program_a)
                .token_program_b(f.token_program_b)
                .memo_program(MEMO_PROGRAM_ID)
                .leg_a_extras_count(0)
                .instruction(),
            &f.user_a,
        ),
    ];
    assert_modes(
        &mut context,
        &cases,
        InstructionError::Custom(DvpSwapProgramError::SwapModeMismatch as u32),
    );
}

fn apply_instruction(f: &DvpFixture) -> Instruction {
    ApplyConfidentialDvpBuilder::new()
        .signer(f.user_b.pubkey())
        .swap_dvp(f.swap_dvp)
        .nonce_tombstone(f.nonce_tombstone)
        .dvp_ata_b(f.dvp_ata_b)
        .token_program(f.token_program_b)
        .expected_pending_balance_credit_counter(0)
        .new_decryptable_available_balance([0; 36])
        .settlement_authority(f.settlement_authority.pubkey())
        .user_a(f.user_a.pubkey())
        .user_b(f.user_b.pubkey())
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .nonce(f.nonce)
        .instruction()
}

#[test]
fn confidential_instructions_check_swap_mode_before_optional_contexts() {
    let mut context = TestContext::new();
    let f = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    assert_create_dvp(&mut context, &f);
    let swap = context.get_account(&f.swap_dvp).unwrap();
    let zk_program = solana_sdk_ids::zk_elgamal_proof_program::ID;
    let transfer = CtTransferData {
        new_source_decryptable_available_balance: [0; 36],
        auditor_ciphertext_lo: [0; 64],
        auditor_ciphertext_hi: [0; 64],
    };
    let mut reclaim = ReclaimConfidentialDvpBuilder::new();
    reclaim
        .signer(f.user_b.pubkey())
        .swap_dvp(f.swap_dvp)
        .mint(f.mint_b)
        .dvp_source_ata(f.dvp_ata_b)
        .signer_dest_ata(f.user_b_ata_b)
        .token_program(f.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .zk_elgamal_proof_program(zk_program);
    let mut cancel = CancelConfidentialDvpBuilder::new();
    cancel
        .settlement_authority(f.settlement_authority.pubkey())
        .swap_dvp(f.swap_dvp)
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .dvp_ata_a(f.dvp_ata_a)
        .dvp_ata_b(f.dvp_ata_b)
        .user_a_ata_a(f.user_a_ata_a)
        .user_b_ata_b(f.user_b_ata_b)
        .token_program_a(f.token_program_a)
        .token_program_b(f.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .zk_elgamal_proof_program(zk_program)
        .leg_a_extras_count(0);
    let mut reject = RejectConfidentialDvpBuilder::new();
    reject
        .signer(f.user_a.pubkey())
        .swap_dvp(f.swap_dvp)
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .dvp_ata_a(f.dvp_ata_a)
        .dvp_ata_b(f.dvp_ata_b)
        .user_a_ata_a(f.user_a_ata_a)
        .user_b_ata_b(f.user_b_ata_b)
        .token_program_a(f.token_program_a)
        .token_program_b(f.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .zk_elgamal_proof_program(zk_program)
        .leg_a_extras_count(0);
    let mut settle = SettleConfidentialDvpBuilder::new();
    settle
        .settlement_authority(f.settlement_authority.pubkey())
        .swap_dvp(f.swap_dvp)
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .dvp_ata_a(f.dvp_ata_a)
        .dvp_ata_b(f.dvp_ata_b)
        .user_a_destination_ata_b(f.user_a_ata_b)
        .user_b_destination_ata_a(f.user_b_ata_a)
        .user_a_ata_a(f.user_a_ata_a)
        .user_b_ata_b(f.user_b_ata_b)
        .token_program_a(f.token_program_a)
        .token_program_b(f.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .zk_elgamal_proof_program(zk_program)
        .leg_a_extras_count(0)
        .payment(transfer.clone())
        .payment_equality_context(f.user_a.pubkey())
        .payment_validity_context(f.user_a.pubkey())
        .payment_range_context(f.user_a.pubkey())
        .eq_lo_context(f.user_a.pubkey())
        .eq_hi_context(f.user_a.pubkey())
        .zero_context(f.user_a.pubkey());

    // Optional contexts deliberately stay as placeholders: mode/owner errors win
    // even when Full/Partial or Some(surplus) would require actual proof accounts.
    let mut cases = Vec::new();
    for refund in [
        LegBRefund::None,
        LegBRefund::Full(transfer.clone()),
        LegBRefund::Partial(transfer.clone()),
    ] {
        cases.extend([
            (
                "Reclaim",
                reclaim.leg_b_refund(refund.clone()).instruction(),
                &f.user_b,
            ),
            (
                "Cancel",
                cancel.leg_b_refund(refund.clone()).instruction(),
                &f.settlement_authority,
            ),
            (
                "Reject",
                reject.leg_b_refund(refund).instruction(),
                &f.user_a,
            ),
        ]);
    }
    cases.push((
        "Settle without surplus",
        settle.instruction(),
        &f.settlement_authority,
    ));
    cases.push((
        "Settle with surplus",
        settle.surplus_b(transfer).instruction(),
        &f.settlement_authority,
    ));
    assert_modes(
        &mut context,
        &cases,
        InstructionError::Custom(DvpSwapProgramError::SwapModeMismatch as u32),
    );
    assert_modes(
        &mut context,
        &[("Apply", apply_instruction(&f), &f.user_b)],
        InstructionError::Custom(DvpSwapProgramError::SwapModeMismatch as u32),
    );

    let mut closed = swap;
    closed.owner = solana_sdk_ids::system_program::ID;
    closed.data.clear();
    context.svm.set_account(f.swap_dvp, closed).unwrap();
    assert_modes(&mut context, &cases, InstructionError::InvalidAccountOwner);
}

#[test]
fn confidential_recovery_and_apply_reject_public_escrow() {
    let mut context = TestContext::new();
    let f = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    assert_create_dvp(&mut context, &f);
    assert_reject_dvp(&mut context, &f, &f.user_a);
    create_ata(&mut context, &f.swap_dvp, &f.mint_b, &f.token_program_b);
    let mut recover = RecoverConfidentialDvpBuilder::new();
    recover
        .signer(f.user_b.pubkey())
        .swap_dvp(f.swap_dvp)
        .nonce_tombstone(f.nonce_tombstone)
        .mint(f.mint_b)
        .dvp_escrow_ata(f.dvp_ata_b)
        .signer_dest_ata(f.user_b_ata_b)
        .token_program(f.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .zk_elgamal_proof_program(solana_sdk_ids::zk_elgamal_proof_program::ID)
        .settlement_authority(f.settlement_authority.pubkey())
        .user_a(f.user_a.pubkey())
        .user_b(f.user_b.pubkey())
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .nonce(f.nonce);
    let transfer = CtTransferData {
        new_source_decryptable_available_balance: [0; 36],
        auditor_ciphertext_lo: [0; 64],
        auditor_ciphertext_hi: [0; 64],
    };
    // Keep placeholders for every mode: the escrow mode error takes priority
    // over refund-specific proof-context checks.
    let mut cases = vec![("Apply", apply_instruction(&f), &f.user_b)];
    for (name, refund) in [
        ("Recover None", LegBRefund::None),
        ("Recover Full", LegBRefund::Full(transfer.clone())),
        ("Recover Partial", LegBRefund::Partial(transfer)),
    ] {
        cases.push((name, recover.leg_b_refund(refund).instruction(), &f.user_b));
    }
    assert_modes(
        &mut context,
        &cases,
        InstructionError::Custom(DvpSwapProgramError::EscrowNotConfidential as u32),
    );
}

fn set_ct_escrow(context: &mut TestContext, f: &DvpFixture) {
    let len = ExtensionType::try_calculate_account_len::<TokenAccount>(&[
        ExtensionType::ConfidentialTransferAccount,
    ])
    .unwrap();
    let mut data = vec![0; len];
    let mut state =
        PodStateWithExtensionsMut::<PodAccount>::unpack_uninitialized(&mut data).unwrap();
    state
        .init_extension::<ConfidentialTransferAccount>(true)
        .unwrap();
    *state.base = PodAccount {
        mint: f.mint_a,
        owner: f.swap_dvp,
        amount: 0.into(),
        delegate: PodCOption::none(),
        state: AccountState::Initialized as u8,
        is_native: PodCOption::none(),
        delegated_amount: 0.into(),
        close_authority: PodCOption::none(),
    };
    state.init_account_type().unwrap();
    context
        .svm
        .set_account(
            f.dvp_ata_a,
            solana_account::Account {
                lamports: 20_000_000,
                data,
                owner: TOKEN_2022_PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

#[test]
fn public_recover_rejects_confidential_escrow() {
    let mut context = TestContext::new();
    let f = setup_dvp_with_programs(
        &mut context,
        0,
        TOKEN_2022_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
    );
    assert_create_dvp(&mut context, &f);
    assert_reject_dvp(&mut context, &f, &f.user_a);
    set_ct_escrow(&mut context, &f);
    let before = context.get_account(&f.dvp_ata_a);
    let ix = RecoverDvpBuilder::new()
        .signer(f.user_a.pubkey())
        .swap_dvp(f.swap_dvp)
        .nonce_tombstone(f.nonce_tombstone)
        .mint(f.mint_a)
        .dvp_escrow_ata(f.dvp_ata_a)
        .signer_dest_ata(f.user_a_ata_a)
        .token_program(f.token_program_a)
        .memo_program(MEMO_PROGRAM_ID)
        .settlement_authority(f.settlement_authority.pubkey())
        .user_a(f.user_a.pubkey())
        .user_b(f.user_b.pubkey())
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .nonce(f.nonce)
        .instruction();
    assert_program_error(
        context.send(ix, &[&f.user_a]),
        DvpSwapProgramError::SwapModeMismatch as u32,
    );
    assert_eq!(context.get_account(&f.dvp_ata_a), before);
}
