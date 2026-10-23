//! Real confidential settlement proofs, deliveries, surplus and post-close balances.
use crate::{
    confidential_utils::{
        assert_contexts_closed, context_account, create_wallet_account, fund_b, mint_auditor,
        pending, prepare_funding_transfer, prepare_refund, reclaim, send_v1, set_mint_auditor,
        state, transfer_contexts, ConfidentialDvpFixture, Keys, DECIMALS, LO_BITS, ZK,
    },
    state_utils::AMOUNT_A,
    utils::{
        create_ata, fund_wallet_ata, get_token_balance, hook_extras_for_mint,
        set_hook_extra_account_metas, TestContext, MEMO_PROGRAM_ID, TOKEN_2022_PROGRAM_ID as TOKEN,
        TOKEN_PROGRAM_ID,
    },
};
use dvp_swap_program_client::{
    instructions::{SettleConfidentialDvp, SettleConfidentialDvpInstructionArgs},
    types::CtTransferData,
    DvpSwapProgramError as Error, DVP_SWAP_PROGRAM_ID,
};
use solana_instruction::{error::InstructionError, AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_zk_elgamal_proof_interface::{instruction::ProofInstruction, proof_data::ZkProofData};
use solana_zk_sdk::{
    encryption::elgamal::ElGamalCiphertext,
    zk_elgamal_proof_program::{
        build_ciphertext_ciphertext_equality_proof_data, build_zero_ciphertext_proof_data,
    },
};
use spl_token_2022_interface::{
    extension::{
        confidential_transfer::ConfidentialTransferAccount, BaseStateWithExtensionsMut,
        ExtensionType, StateWithExtensionsMut,
    },
    instruction as token_ix,
    state::Account as TokenAccount,
};
use spl_token_confidential_transfer_proof_generation::{
    transfer::transfer_split_proof_data, try_combine_lo_hi_ciphertexts, try_split_u64,
};

// Exercise both ciphertext limbs instead of only the low 16 bits.
const AMOUNT_B: u64 = (3 << LO_BITS) + 42;
const PAYMENT_CONTEXTS_START: usize = 14;
const ZERO_CONTEXT: usize = 19;
const SURPLUS_CONTEXTS_START: usize = 20;
const FIXED_ACCOUNTS_LEN: usize = 23;

fn setup(
    context: &mut TestContext,
    surplus_a: u64,
    surplus_b: u64,
    hook: bool,
    custom_destinations: bool,
) -> (ConfidentialDvpFixture, SettleConfidentialDvp, Keys, Keys) {
    let f = ConfidentialDvpFixture::new(context, true, hook);
    setup_fixture(context, f, surplus_a, surplus_b, hook, custom_destinations)
}

fn setup_fixture(
    context: &mut TestContext,
    mut f: ConfidentialDvpFixture,
    surplus_a: u64,
    surplus_b: u64,
    hook: bool,
    custom_destinations: bool,
) -> (ConfidentialDvpFixture, SettleConfidentialDvp, Keys, Keys) {
    let destination_a = Keypair::new();
    let destination_b = Keypair::new();
    if custom_destinations {
        f.args.user_a_settlement_destination = Some(destination_a.pubkey());
        f.args.user_b_settlement_destination = Some(destination_b.pubkey());
    }
    f.args.earliest_settlement_timestamp = Some(context.now());
    let (lo, hi) = try_split_u64(AMOUNT_B, LO_BITS).unwrap();
    f.args.amount_b_ciphertext_lo = f
        .keys
        .elgamal
        .pubkey()
        .encrypt_with(lo, &f.amount_b_openings[0])
        .to_bytes();
    f.args.amount_b_ciphertext_hi = f
        .keys
        .elgamal
        .pubkey()
        .encrypt_with(hi, &f.amount_b_openings[1])
        .to_bytes();
    f.create(context);
    let user_a_ata_a = if f.accounts.mint_a == crate::utils::NATIVE_MINT {
        let refund = create_ata(
            context,
            &f.user_a.pubkey(),
            &f.accounts.mint_a,
            &TOKEN_PROGRAM_ID,
        );
        // Raw SOL funding leaves the token amount unsynced until Settle.
        let funding = solana_system_interface::instruction::transfer(
            &context.payer.pubkey(),
            &f.accounts.dvp_ata_a,
            AMOUNT_A + surplus_a,
        );
        send_v1(context, &[funding], &[]).unwrap();
        refund
    } else {
        let user_a_ata_a = fund_wallet_ata(
            context,
            &f.user_a,
            &f.accounts.mint_a,
            AMOUNT_A + surplus_a,
            &TOKEN_PROGRAM_ID,
        );
        context
            .send(
                spl_token_interface::instruction::transfer_checked(
                    &TOKEN_PROGRAM_ID,
                    &user_a_ata_a,
                    &f.accounts.mint_a,
                    &f.accounts.dvp_ata_a,
                    &f.user_a.pubkey(),
                    &[],
                    AMOUNT_A + surplus_a,
                    DECIMALS,
                )
                .unwrap(),
                &[&f.user_a],
            )
            .unwrap();
        user_a_ata_a
    };
    let destination_keys = Keys::new();
    let refund_keys = Keys::new();
    let user_a_destination_ata_b = create_wallet_account(
        context,
        if custom_destinations {
            &destination_a
        } else {
            &f.user_a
        },
        &f.accounts.mint_b,
        &destination_keys,
    );
    let user_b_destination_ata_a = create_ata(
        context,
        &f.args
            .user_b_settlement_destination
            .unwrap_or(f.user_b.pubkey()),
        &f.accounts.mint_a,
        &TOKEN_PROGRAM_ID,
    );
    let user_b_ata_b = create_wallet_account(context, &f.user_b, &f.accounts.mint_b, &refund_keys);
    let extras = if hook {
        set_hook_extra_account_metas(context, &f.accounts.mint_b, &[]);
        hook_extras_for_mint(&f.accounts.mint_b)
    } else {
        vec![]
    };
    let funding = prepare_funding_transfer(
        context,
        &f.accounts.mint_b,
        &f.user_b,
        &user_b_ata_b,
        &refund_keys,
        &f.accounts.dvp_ata_b,
        &f.keys,
        AMOUNT_B + surplus_b,
        &extras,
    );
    send_v1(context, &funding, &[&f.user_b]).unwrap();
    send_v1(
        context,
        &[f.apply(f.user_b.pubkey(), AMOUNT_B + surplus_b)
            .instruction()],
        &[&f.user_b],
    )
    .unwrap();
    let accounts = SettleConfidentialDvp {
        settlement_authority: f.authority.pubkey(),
        swap_dvp: f.accounts.swap_dvp,
        mint_a: f.accounts.mint_a,
        mint_b: f.accounts.mint_b,
        dvp_ata_a: f.accounts.dvp_ata_a,
        dvp_ata_b: f.accounts.dvp_ata_b,
        user_a_destination_ata_b,
        user_b_destination_ata_a,
        user_a_ata_a,
        user_b_ata_b,
        token_program_a: TOKEN_PROGRAM_ID,
        token_program_b: TOKEN,
        memo_program: MEMO_PROGRAM_ID,
        zk_elgamal_proof_program: ZK,
        payment_equality_context: Pubkey::default(),
        payment_validity_context: Pubkey::default(),
        payment_range_context: Pubkey::default(),
        eq_lo_context: Pubkey::default(),
        eq_hi_context: Pubkey::default(),
        zero_context: Pubkey::default(),
        surplus_equality_context: None,
        surplus_validity_context: None,
        surplus_range_context: None,
    };
    (f, accounts, destination_keys, refund_keys)
}

struct PreparedTransfer {
    data: CtTransferData,
    contexts: [Pubkey; 3],
    remaining: ElGamalCiphertext,
    limbs: [ElGamalCiphertext; 2],
}

fn prepare_transfer(
    context: &mut TestContext,
    f: &ConfidentialDvpFixture,
    available: &ElGamalCiphertext,
    balance: u64,
    amount: u64,
    destination: &Keys,
) -> PreparedTransfer {
    let proof = transfer_split_proof_data(
        available,
        &f.keys.balance(balance).try_into().unwrap(),
        amount,
        &f.keys.elgamal,
        &f.keys.ae,
        destination.elgamal.pubkey(),
        mint_auditor(context, &f.accounts.mint_b).as_ref(),
    )
    .unwrap();
    let contexts = transfer_contexts(context, &f.authority.pubkey(), &proof);
    let ciphertext = &proof.ciphertext_validity_proof_data_with_ciphertext;
    let validity = ciphertext.proof_data.context_data();
    let lo = validity
        .grouped_ciphertext_lo
        .try_extract_ciphertext(0)
        .unwrap()
        .try_into()
        .unwrap();
    let hi = validity
        .grouped_ciphertext_hi
        .try_extract_ciphertext(0)
        .unwrap()
        .try_into()
        .unwrap();
    PreparedTransfer {
        data: CtTransferData {
            new_source_decryptable_available_balance: f.keys.balance(balance - amount).0,
            auditor_ciphertext_lo: ciphertext.ciphertext_lo.0,
            auditor_ciphertext_hi: ciphertext.ciphertext_hi.0,
        },
        contexts,
        remaining: available - try_combine_lo_hi_ciphertexts(&lo, &hi, LO_BITS).unwrap(),
        limbs: [lo, hi],
    }
}

fn prepare_settle(
    context: &mut TestContext,
    f: &ConfidentialDvpFixture,
    accounts: &mut SettleConfidentialDvp,
    destination_keys: &Keys,
    refund_keys: &Keys,
    surplus: Option<u64>,
) -> Instruction {
    let available = state(context, &f.accounts.dvp_ata_b)
        .available_balance
        .try_into()
        .unwrap();
    let payment = prepare_transfer(
        context,
        f,
        &available,
        AMOUNT_B + surplus.unwrap_or(0),
        AMOUNT_B,
        destination_keys,
    );
    [
        accounts.payment_equality_context,
        accounts.payment_validity_context,
        accounts.payment_range_context,
    ] = payment.contexts;
    let (lo, hi) = try_split_u64(AMOUNT_B, LO_BITS).unwrap();
    for (index, (amount, stored)) in [
        (lo, f.args.amount_b_ciphertext_lo),
        (hi, f.args.amount_b_ciphertext_hi),
    ]
    .into_iter()
    .enumerate()
    {
        let proof = build_ciphertext_ciphertext_equality_proof_data(
            &f.keys.elgamal,
            f.keys.elgamal.pubkey(),
            &payment.limbs[index],
            &ElGamalCiphertext::from_bytes(&stored).unwrap(),
            &f.amount_b_openings[index],
            amount,
        )
        .unwrap();
        let address = context_account(
            context,
            &f.authority.pubkey(),
            ProofInstruction::VerifyCiphertextCiphertextEquality,
            &proof,
        );
        if index == 0 {
            accounts.eq_lo_context = address;
        } else {
            accounts.eq_hi_context = address;
        }
    }
    let mut remaining = payment.remaining;
    let surplus_data = surplus.map(|amount| {
        let transfer = prepare_transfer(context, f, &remaining, amount, amount, refund_keys);
        accounts.surplus_equality_context = Some(transfer.contexts[0]);
        accounts.surplus_validity_context = Some(transfer.contexts[1]);
        accounts.surplus_range_context = Some(transfer.contexts[2]);
        remaining = transfer.remaining;
        transfer.data
    });
    let zero = build_zero_ciphertext_proof_data(&f.keys.elgamal, &remaining).unwrap();
    accounts.zero_context = context_account(
        context,
        &f.authority.pubkey(),
        ProofInstruction::VerifyZeroCiphertext,
        &zero,
    );
    accounts.instruction(SettleConfidentialDvpInstructionArgs {
        leg_a_extras_count: 0,
        payment: payment.data,
        surplus_b: surplus_data,
    })
}

fn assert_failure(
    context: &mut TestContext,
    f: &ConfidentialDvpFixture,
    ix: Instruction,
    expected: InstructionError,
    before_cpi: bool,
) {
    let before: Vec<_> = ix
        .accounts
        .iter()
        .filter(|a| a.is_writable)
        .map(|a| (a.pubkey, context.get_account(&a.pubkey)))
        .collect();
    let signers = if ix.accounts[0].is_signer {
        vec![&f.authority]
    } else {
        vec![]
    };
    let failure = send_v1(context, &[ix], &signers).unwrap_err();
    assert_eq!(
        format!("{:?}", failure.err),
        format!("InstructionError(0, {expected:?})"),
        "{:?}",
        failure.meta.logs
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
    for (address, account) in before {
        assert_eq!(context.get_account(&address), account, "{address}");
    }
}

#[test]
fn settles_exact_amounts_and_surplus_to_depositors_and_returns_all_rent() {
    for surplus in [None, Some(0), Some(9)] {
        let mut context = TestContext::new();
        let surplus_a = 17;
        let (f, mut accounts, destination_keys, refund_keys) =
            setup(&mut context, surplus_a, surplus.unwrap_or(0), false, true);
        let ix = prepare_settle(
            &mut context,
            &f,
            &mut accounts,
            &destination_keys,
            &refund_keys,
            surplus,
        );
        let closed: Vec<_> = [accounts.swap_dvp, accounts.dvp_ata_a, accounts.dvp_ata_b]
            .into_iter()
            .chain(
                ix.accounts[PAYMENT_CONTEXTS_START..FIXED_ACCOUNTS_LEN]
                    .iter()
                    .map(|a| a.pubkey)
                    .filter(|a| *a != DVP_SWAP_PROGRAM_ID),
            )
            .collect();
        let rent: u64 = closed
            .iter()
            .map(|a| context.get_account(a).unwrap().lamports)
            .sum();
        let before = context
            .get_account(&f.authority.pubkey())
            .map_or(0, |a| a.lamports);
        let result = send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
        assert!(result.compute_units_consumed < 200_000);
        println!(
            "Settle surplus={surplus:?}: {} CU",
            result.compute_units_consumed
        );
        assert_eq!(
            pending(
                &context,
                &accounts.user_a_destination_ata_b,
                &destination_keys
            ),
            AMOUNT_B
        );
        assert_eq!(
            pending(&context, &accounts.user_b_ata_b, &refund_keys),
            surplus.unwrap_or(0)
        );
        assert_eq!(
            get_token_balance(&context, &accounts.user_b_destination_ata_a),
            AMOUNT_A
        );
        assert_eq!(
            get_token_balance(&context, &accounts.user_a_ata_a),
            surplus_a
        );
        for address in closed {
            assert!(
                context
                    .get_account(&address)
                    .is_none_or(|a| a.lamports == 0),
                "{address}"
            );
        }
        assert_eq!(
            context.get_account(&f.authority.pubkey()).unwrap().lamports,
            before + rent
        );
        assert!(context
            .get_account(&f.accounts.nonce_tombstone)
            .unwrap()
            .data
            .is_empty());
    }
}

#[test]
fn pending_and_mint_to_leave_escrow_open_and_allow_apply_after_settle() {
    for (late_credit, public_amount) in [(7, 0), (0, 11), (7, 11)] {
        let mut context = TestContext::new();
        let (f, mut accounts, destination_keys, refund_keys) =
            setup(&mut context, 0, 0, false, false);
        let ix = prepare_settle(
            &mut context,
            &f,
            &mut accounts,
            &destination_keys,
            &refund_keys,
            None,
        );
        if late_credit != 0 {
            let donor = Keypair::new();
            let donor_keys = Keys::new();
            let source = create_wallet_account(&mut context, &donor, &accounts.mint_b, &donor_keys);
            let funding = prepare_funding_transfer(
                &mut context,
                &accounts.mint_b,
                &donor,
                &source,
                &donor_keys,
                &accounts.dvp_ata_b,
                &f.keys,
                late_credit,
                &[],
            );
            send_v1(&mut context, &funding, &[&donor]).unwrap();
        }
        if public_amount != 0 {
            let mint_to = token_ix::mint_to(
                &TOKEN,
                &accounts.mint_b,
                &accounts.dvp_ata_b,
                &context.payer.pubkey(),
                &[],
                public_amount,
            )
            .unwrap();
            send_v1(&mut context, &[mint_to], &[]).unwrap();
        }
        let before = state(&context, &accounts.dvp_ata_b);
        send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
        assert!(context
            .get_account(&accounts.swap_dvp)
            .is_none_or(|a| a.lamports == 0));
        assert!(context
            .get_account(&accounts.dvp_ata_a)
            .is_none_or(|a| a.lamports == 0));
        assert_eq!(
            get_token_balance(&context, &accounts.dvp_ata_b),
            public_amount
        );
        let after = state(&context, &accounts.dvp_ata_b);
        assert_eq!(after.pending_balance_lo, before.pending_balance_lo);
        assert_eq!(after.pending_balance_hi, before.pending_balance_hi);
        assert_eq!(
            after.pending_balance_credit_counter,
            before.pending_balance_credit_counter
        );
        let available: ElGamalCiphertext = after.available_balance.try_into().unwrap();
        assert_eq!(available.decrypt_u32(f.keys.elgamal.secret()), Some(0));
        assert_eq!(
            after.available_balance == Default::default(),
            late_credit == 0
        );
        if late_credit != 0 {
            send_v1(
                &mut context,
                &[f.apply(f.user_b.pubkey(), late_credit).instruction()],
                &[&f.user_b],
            )
            .unwrap();
            let after_apply: ElGamalCiphertext = state(&context, &accounts.dvp_ata_b)
                .available_balance
                .try_into()
                .unwrap();
            assert_eq!(
                after_apply.decrypt_u32(f.keys.elgamal.secret()),
                Some(late_credit)
            );
            assert_eq!(pending(&context, &accounts.dvp_ata_b, &f.keys), 0);
        }
    }
}

#[test]
fn validates_every_context_and_its_writability_before_cpi() {
    let mut context = TestContext::new();
    let (f, mut accounts, destination_keys, refund_keys) = setup(&mut context, 0, 9, false, false);
    let ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        Some(9),
    );
    for index in PAYMENT_CONTEXTS_START..FIXED_ACCOUNTS_LEN {
        if index >= SURPLUS_CONTEXTS_START {
            let mut missing = ix.clone();
            missing.accounts[index] = AccountMeta::new_readonly(DVP_SWAP_PROGRAM_ID, false);
            assert_failure(
                &mut context,
                &f,
                missing,
                InstructionError::InvalidInstructionData,
                true,
            );
        }
        let mut readonly = ix.clone();
        readonly.accounts[index].is_writable = false;
        assert_failure(
            &mut context,
            &f,
            readonly,
            InstructionError::InvalidAccountData,
            true,
        );
        let address = ix.accounts[index].pubkey;
        let original = context.get_account(&address).unwrap();
        let mut wrong_owner = original.clone();
        wrong_owner.owner = TOKEN;
        context.svm.set_account(address, wrong_owner).unwrap();
        assert_failure(
            &mut context,
            &f,
            ix.clone(),
            InstructionError::Custom(Error::InvalidProofContext as u32),
            true,
        );
        let mut wrong_authority = original.clone();
        wrong_authority.data[..core::mem::size_of::<Pubkey>()]
            .copy_from_slice(f.user_b.pubkey().as_ref());
        context.svm.set_account(address, wrong_authority).unwrap();
        assert_failure(
            &mut context,
            &f,
            ix.clone(),
            InstructionError::Custom(Error::ProofContextAuthorityMismatch as u32),
            true,
        );
        context.svm.set_account(address, original).unwrap();
    }
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
}

#[test]
fn binds_stored_amount_and_rolls_back_a_wrong_zero_or_missing_surplus() {
    let mut context = TestContext::new();
    let (f, mut accounts, destination_keys, refund_keys) = setup(&mut context, 17, 9, false, false);
    let ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        Some(9),
    );
    let other = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        Some(9),
    );
    let original = context.get_account(&accounts.swap_dvp).unwrap();
    let base_len = dvp_swap_program_client::verify::SWAP_DVP_ACCOUNT_LEN;
    let ciphertext_len =
        core::mem::size_of::<solana_zk_sdk_pod::encryption::elgamal::PodElGamalCiphertext>();
    for offset in [base_len, base_len + ciphertext_len] {
        let mut wrong_amount = original.clone();
        wrong_amount.data[offset] ^= 1;
        context
            .svm
            .set_account(accounts.swap_dvp, wrong_amount)
            .unwrap();
        assert_failure(
            &mut context,
            &f,
            ix.clone(),
            InstructionError::Custom(Error::ConfidentialAmountBMismatch as u32),
            true,
        );
    }
    context
        .svm
        .set_account(accounts.swap_dvp, original)
        .unwrap();
    let mut wrong_payment = ix.clone();
    wrong_payment.accounts[15] = other.accounts[15].clone(); // payment validity
    assert_failure(
        &mut context,
        &f,
        wrong_payment,
        InstructionError::Custom(Error::ConfidentialAmountBMismatch as u32),
        true,
    );
    let mut wrong_zero = ix.clone();
    wrong_zero.accounts[ZERO_CONTEXT] = other.accounts[ZERO_CONTEXT].clone();
    assert_failure(
        &mut context,
        &f,
        wrong_zero,
        InstructionError::Custom(Error::EscrowBalanceNotZero as u32),
        false,
    );
    let mut no_surplus = ix.clone();
    // Keep the prepared payment, but omit the transfer that would drain its remainder.
    let mut args: SettleConfidentialDvpInstructionArgs = borsh::from_slice(&ix.data[1..]).unwrap();
    args.surplus_b = None;
    no_surplus.data.truncate(1);
    no_surplus.data.extend(borsh::to_vec(&args).unwrap());
    for account in &mut no_surplus.accounts[SURPLUS_CONTEXTS_START..FIXED_ACCOUNTS_LEN] {
        *account = AccountMeta::new_readonly(DVP_SWAP_PROGRAM_ID, false);
    }
    assert_failure(
        &mut context,
        &f,
        no_surplus,
        InstructionError::Custom(Error::EscrowBalanceNotZero as u32),
        false,
    );
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
}

#[test]
fn surplus_recipient_readiness_is_checked_before_payment() {
    let mut context = TestContext::new();
    let (f, mut accounts, destination_keys, refund_keys) = setup(&mut context, 0, 9, false, false);
    let ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        Some(9),
    );
    for address in [accounts.user_a_destination_ata_b, accounts.user_b_ata_b] {
        let original = context.get_account(&address).unwrap();
        for (change, expected) in [
            (0, Error::RecipientNotApproved),
            (1, Error::RecipientConfidentialCreditsDisabled),
            (2, Error::RecipientPendingCounterFull),
        ] {
            let mut account = original.clone();
            let mut token =
                StateWithExtensionsMut::<TokenAccount>::unpack(&mut account.data).unwrap();
            let ct = token
                .get_extension_mut::<ConfidentialTransferAccount>()
                .unwrap();
            match change {
                0 => ct.approved = false.into(),
                1 => ct.allow_confidential_credits = false.into(),
                _ => ct.pending_balance_credit_counter = ct.maximum_pending_balance_credit_counter,
            }
            context.svm.set_account(address, account).unwrap();
            assert_failure(
                &mut context,
                &f,
                ix.clone(),
                InstructionError::Custom(expected as u32),
                true,
            );
        }
        context.svm.set_account(address, original).unwrap();
    }
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
}

#[test]
fn hooked_payment_and_surplus_each_receive_a_preceding_memo() {
    for surplus in [None, Some(9)] {
        let mut context = TestContext::new();
        let (f, mut accounts, destination_keys, refund_keys) =
            setup(&mut context, 0, surplus.unwrap_or(0), true, false);
        for (address, owner) in [
            (accounts.user_a_destination_ata_b, &f.user_a),
            (accounts.user_b_ata_b, &f.user_b),
        ] {
            let instructions = [
                token_ix::reallocate(&TOKEN, &address, &context.payer.pubkey(), &owner.pubkey(), &[], &[ExtensionType::MemoTransfer]).unwrap(),
                spl_token_2022_interface::extension::memo_transfer::instruction::enable_required_transfer_memos(&TOKEN, &address, &owner.pubkey(), &[]).unwrap(),
            ];
            send_v1(&mut context, &instructions, &[owner]).unwrap();
        }
        let mut ix = prepare_settle(
            &mut context,
            &f,
            &mut accounts,
            &destination_keys,
            &refund_keys,
            surplus,
        );
        // Missing hook extras fail after entering Token-2022; all DvP state rolls back.
        let before = context.get_account(&accounts.dvp_ata_b);
        let failure = send_v1(&mut context, &[ix.clone()], &[&f.authority]).unwrap_err();
        assert_eq!(
            failure.err,
            solana_transaction::TransactionError::InstructionError(
                0,
                InstructionError::MissingAccount
            )
        );
        assert!(failure
            .meta
            .logs
            .contains(&format!("Program {TOKEN} invoke [2]")));
        assert_eq!(context.get_account(&accounts.dvp_ata_b), before);
        ix.accounts.extend(hook_extras_for_mint(&accounts.mint_b));
        let result = send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
        let transfers = 1 + usize::from(surplus.is_some());
        assert_eq!(
            result
                .logs
                .iter()
                .filter(|line| line.contains(&format!("hook amount: {}", u64::MAX)))
                .count(),
            transfers
        );
        let memo_end = format!("Program {MEMO_PROGRAM_ID} success");
        assert_eq!(
            result.logs.iter().filter(|line| *line == &memo_end).count(),
            transfers
        );
        for (i, line) in result.logs.iter().enumerate() {
            if line == &memo_end {
                assert_eq!(result.logs[i + 1], format!("Program {TOKEN} invoke [2]"));
            }
        }
        assert!(result.compute_units_consumed < 250_000);
        println!(
            "Hooked Settle surplus={surplus:?}: {} CU",
            result.compute_units_consumed
        );
    }
}

#[test]
fn checks_public_settlement_guards_and_optional_account_layout() {
    let mut context = TestContext::new();
    let (f, mut accounts, destination_keys, refund_keys) = setup(&mut context, 0, 0, false, false);
    let ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        None,
    );
    let mut missing_signer = ix.clone();
    missing_signer.accounts[0].is_signer = false;
    assert_failure(
        &mut context,
        &f,
        missing_signer,
        InstructionError::MissingRequiredSignature,
        true,
    );
    let mut wrong_authority = ix.clone();
    wrong_authority.accounts[0].pubkey = f.user_a.pubkey();
    crate::confidential_utils::assert_error(
        &mut context,
        &[wrong_authority],
        &[&f.user_a],
        0,
        InstructionError::Custom(Error::SettlementAuthorityMismatch as u32),
    );
    for (index, replacement, expected) in [
        (2, accounts.mint_b, InstructionError::InvalidAccountData),
        (3, accounts.mint_a, InstructionError::InvalidAccountData),
        (4, accounts.dvp_ata_b, InstructionError::InvalidSeeds),
        (5, accounts.dvp_ata_a, InstructionError::InvalidSeeds),
        (6, accounts.user_b_ata_b, InstructionError::InvalidSeeds),
        (7, accounts.user_a_ata_a, InstructionError::InvalidSeeds),
        (
            8,
            accounts.user_b_destination_ata_a,
            InstructionError::InvalidSeeds,
        ),
        (
            9,
            accounts.user_a_destination_ata_b,
            InstructionError::InvalidSeeds,
        ),
        (10, TOKEN, InstructionError::IncorrectProgramId),
        (11, TOKEN_PROGRAM_ID, InstructionError::IncorrectProgramId),
        (13, TOKEN, InstructionError::IncorrectProgramId),
        (
            SURPLUS_CONTEXTS_START,
            accounts.payment_equality_context,
            InstructionError::InvalidInstructionData,
        ),
    ] {
        let mut invalid = ix.clone();
        invalid.accounts[index].pubkey = replacement;
        assert_failure(&mut context, &f, invalid, expected, true);
    }
    let mut bad_split = ix.clone();
    bad_split.data[1] = 1; // One leg A extra declared, no remaining accounts supplied.
    assert_failure(
        &mut context,
        &f,
        bad_split,
        InstructionError::InvalidInstructionData,
        true,
    );
    let mut too_many_extras = ix.clone();
    too_many_extras
        .accounts
        .extend(vec![AccountMeta::new_readonly(TOKEN, false); 33]);
    assert_failure(
        &mut context,
        &f,
        too_many_extras,
        InstructionError::InvalidInstructionData,
        true,
    );
    let clock = context.svm.get_sysvar::<solana_clock::Clock>();
    context.svm.set_sysvar(&solana_clock::Clock {
        unix_timestamp: f.args.earliest_settlement_timestamp.unwrap() - 1,
        ..clock
    });
    assert_failure(
        &mut context,
        &f,
        ix.clone(),
        InstructionError::Custom(Error::SettlementTooEarly as u32),
        true,
    );
    context.svm.set_sysvar(&solana_clock::Clock {
        unix_timestamp: f.args.expiry_timestamp + 1,
        ..clock
    });
    assert_failure(
        &mut context,
        &f,
        ix.clone(),
        InstructionError::Custom(Error::DvpExpired as u32),
        true,
    );
    context.svm.set_sysvar(&clock);

    let swap = context.get_account(&accounts.swap_dvp).unwrap();
    let mut public = swap.clone();
    public
        .data
        .truncate(dvp_swap_program_client::verify::SWAP_DVP_ACCOUNT_LEN);
    context.svm.set_account(accounts.swap_dvp, public).unwrap();
    assert_failure(
        &mut context,
        &f,
        ix.clone(),
        InstructionError::Custom(Error::SwapModeMismatch as u32),
        true,
    );
    context.svm.set_account(accounts.swap_dvp, swap).unwrap();
    for (address, owner, balance) in [
        (accounts.dvp_ata_a, accounts.swap_dvp, AMOUNT_A - 1),
        (accounts.user_b_destination_ata_a, Pubkey::new_unique(), 0),
        (accounts.user_a_ata_a, Pubkey::new_unique(), 0),
    ] {
        let original = context.get_account(&address).unwrap();
        crate::utils::set_token_balance(
            &mut context,
            &address,
            &accounts.mint_a,
            &owner,
            balance,
            &TOKEN_PROGRAM_ID,
        );
        let error = if address == accounts.dvp_ata_a {
            Error::LegNotFunded
        } else {
            Error::RecipientAtaMismatch
        };
        assert_failure(
            &mut context,
            &f,
            ix.clone(),
            InstructionError::Custom(error as u32),
            true,
        );
        context.svm.set_account(address, original).unwrap();
    }
    // No surplus: the refund accounts may be absent and need no CT readiness.
    context
        .svm
        .set_account(accounts.user_a_ata_a, solana_account::Account::default())
        .unwrap();
    context
        .svm
        .set_account(accounts.user_b_ata_b, solana_account::Account::default())
        .unwrap();
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
}

#[test]
fn revalidates_both_mints_before_any_transfer() {
    let mut context = TestContext::new();
    let (f, mut accounts, destination_keys, refund_keys) = setup(&mut context, 0, 0, false, false);
    let ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        None,
    );
    for address in [accounts.mint_a, accounts.mint_b] {
        let original = context.get_account(&address).unwrap();
        let mut wrong_owner = original.clone();
        wrong_owner.owner = solana_sdk_ids::system_program::ID;
        context.svm.set_account(address, wrong_owner).unwrap();
        assert_failure(
            &mut context,
            &f,
            ix.clone(),
            InstructionError::InvalidAccountOwner,
            true,
        );
        context.svm.set_account(address, original.clone()).unwrap();
        let mut changed_authority = original.clone();
        let mut mint = StateWithExtensionsMut::<spl_token_2022_interface::state::Mint>::unpack(
            &mut changed_authority.data,
        )
        .unwrap();
        mint.base.mint_authority = solana_program_option::COption::Some(Pubkey::new_unique());
        mint.pack_base();
        context.svm.set_account(address, changed_authority).unwrap();
        assert_failure(
            &mut context,
            &f,
            ix.clone(),
            InstructionError::Custom(Error::MintAuthorityChanged as u32),
            true,
        );
        context.svm.set_account(address, original).unwrap();
    }
    let original = context.get_account(&accounts.mint_b).unwrap();
    // Deliberate layout substitution exercises the guard; removing a live
    // mint's confidential extension is not a Token-2022 operation.
    crate::utils::set_mint(&mut context, &accounts.mint_b, &TOKEN);
    assert_failure(
        &mut context,
        &f,
        ix.clone(),
        InstructionError::Custom(Error::MintNotConfidential as u32),
        true,
    );
    context
        .svm
        .set_account(accounts.mint_b, original.clone())
        .unwrap();
    crate::utils::set_mint_2022_with_transfer_fee(
        &mut context,
        &accounts.mint_b,
        &f.authority.pubkey(),
        100,
        1_000,
    );
    assert_failure(
        &mut context,
        &f,
        ix.clone(),
        InstructionError::Custom(Error::BlockedMintExtension as u32),
        true,
    );
    context.svm.set_account(accounts.mint_b, original).unwrap();
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
}

#[test]
fn applying_a_late_credit_invalidates_old_proofs_and_a_fresh_settle_succeeds() {
    let mut context = TestContext::new();
    let (f, mut accounts, destination_keys, refund_keys) = setup(&mut context, 0, 0, false, false);
    let ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        None,
    );
    let extra = 7;
    let funding = prepare_funding_transfer(
        &mut context,
        &accounts.mint_b,
        &f.user_b,
        &accounts.user_b_ata_b,
        &refund_keys,
        &accounts.dvp_ata_b,
        &f.keys,
        extra,
        &[],
    );
    send_v1(&mut context, &funding, &[&f.user_b]).unwrap();
    send_v1(
        &mut context,
        &[f.apply(f.user_b.pubkey(), AMOUNT_B + extra).instruction()],
        &[&f.user_b],
    )
    .unwrap();
    assert_failure(
        &mut context,
        &f,
        ix,
        InstructionError::Custom(
            spl_token_2022_interface::error::TokenError::ConfidentialTransferBalanceMismatch as u32,
        ),
        false,
    );
    let fresh = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        Some(extra),
    );
    send_v1(&mut context, &[fresh], &[&f.authority]).unwrap();
    assert_eq!(
        pending(
            &context,
            &accounts.user_a_destination_ata_b,
            &destination_keys
        ),
        AMOUNT_B
    );
    assert_eq!(
        pending(&context, &accounts.user_b_ata_b, &refund_keys),
        extra
    );
}

#[test]
fn validates_proofs_before_syncing_and_settling_native_leg_a() {
    let mut context = TestContext::new();
    crate::utils::set_native_mint(&mut context);
    let mut f = ConfidentialDvpFixture::new(&mut context, true, false);
    f.accounts.mint_a = crate::utils::NATIVE_MINT;
    f.accounts.swap_dvp = crate::utils::swap_dvp_pda(
        &f.authority.pubkey(),
        &f.user_a.pubkey(),
        &f.user_b.pubkey(),
        &f.accounts.mint_a,
        &f.accounts.mint_b,
        f.args.nonce,
    )
    .0;
    f.accounts.nonce_tombstone = crate::utils::nonce_tombstone_pda(&f.accounts.swap_dvp).0;
    f.accounts.dvp_ata_a =
        crate::utils::dvp_ata(&f.accounts.swap_dvp, &f.accounts.mint_a, &TOKEN_PROGRAM_ID);
    f.accounts.dvp_ata_b = crate::utils::dvp_ata(&f.accounts.swap_dvp, &f.accounts.mint_b, &TOKEN);
    let (f, mut accounts, destination_keys, refund_keys) =
        setup_fixture(&mut context, f, 17, 0, false, false);
    assert_eq!(get_token_balance(&context, &accounts.dvp_ata_a), 0);
    let ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        None,
    );
    let zero = context.get_account(&accounts.zero_context).unwrap();
    let mut invalid = zero.clone();
    invalid.owner = TOKEN;
    context
        .svm
        .set_account(accounts.zero_context, invalid)
        .unwrap();
    assert_failure(
        &mut context,
        &f,
        ix.clone(),
        InstructionError::Custom(Error::InvalidProofContext as u32),
        true,
    );
    context
        .svm
        .set_account(accounts.zero_context, zero)
        .unwrap();
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
    assert_eq!(
        get_token_balance(&context, &accounts.user_b_destination_ata_a),
        AMOUNT_A
    );
    assert_eq!(get_token_balance(&context, &accounts.user_a_ata_a), 17);
    assert!(context
        .get_account(&accounts.dvp_ata_a)
        .is_none_or(|a| a.lamports == 0));
}

#[test]
fn partial_reclaim_reduces_large_surplus_before_settlement() {
    const SURPLUS: u64 = 1 << 48;
    let mut context = TestContext::new();
    let (f, mut accounts, destination_keys, refund_keys) = setup(&mut context, 0, 0, false, false);
    // Each incoming transfer fits Token-2022's 48-bit limit; their sum does not.
    for _ in 0..2 {
        fund_b(&mut context, &f, &refund_keys, SURPLUS / 2, false);
    }
    send_v1(
        &mut context,
        &[f.apply(f.user_b.pubkey(), AMOUNT_B + SURPLUS)
            .expected_pending_balance_credit_counter(2)
            .instruction()],
        &[&f.user_b],
    )
    .unwrap();
    let swap_before = context.get_account(&accounts.swap_dvp);
    let refund = prepare_refund(
        &mut context,
        &f,
        &refund_keys,
        f.user_b.pubkey(),
        AMOUNT_B + SURPLUS,
        1,
        false,
    );
    send_v1(
        &mut context,
        &[reclaim(&f, false, &refund, &[])],
        &[&f.user_b],
    )
    .unwrap();
    assert_contexts_closed(&context, &refund);
    assert_eq!(context.get_account(&accounts.swap_dvp), swap_before);
    assert_eq!(pending(&context, &accounts.user_b_ata_b, &refund_keys), 1);
    // Prepare new proofs against the post-reclaim balance and the unchanged payment.
    let ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        Some(SURPLUS - 1),
    );
    let proofs: Vec<_> = ix.accounts[PAYMENT_CONTEXTS_START..FIXED_ACCOUNTS_LEN]
        .iter()
        .map(|a| a.pubkey)
        .collect();
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
    assert_eq!(
        pending(
            &context,
            &accounts.user_a_destination_ata_b,
            &destination_keys
        ),
        AMOUNT_B
    );
    assert_eq!(
        pending(&context, &accounts.user_b_ata_b, &refund_keys),
        SURPLUS
    );
    assert_eq!(
        get_token_balance(&context, &accounts.user_b_destination_ata_a),
        AMOUNT_A
    );
    for address in [accounts.swap_dvp, accounts.dvp_ata_a, accounts.dvp_ata_b]
        .into_iter()
        .chain(proofs)
    {
        assert!(context.get_account(&address).is_none());
    }
    assert!(context.get_account(&f.accounts.nonce_tombstone).is_some());
}

#[test]
fn auditor_payment_and_surplus_reject_mismatched_ciphertext_atomically() {
    use borsh::BorshDeserialize;
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, true, false);
    let auditor = solana_zk_sdk::encryption::elgamal::ElGamalKeypair::new_rand();
    set_mint_auditor(&mut context, &f.accounts.mint_b, auditor.pubkey());
    let (f, mut accounts, destination_keys, refund_keys) =
        setup_fixture(&mut context, f, 0, 9, false, false);
    let ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        Some(9),
    );
    let args = SettleConfidentialDvpInstructionArgs::try_from_slice(&ix.data[1..]).unwrap();
    for (data, amount) in [
        (&args.payment, AMOUNT_B),
        (args.surplus_b.as_ref().unwrap(), 9),
    ] {
        let lo = ElGamalCiphertext::from_bytes(&data.auditor_ciphertext_lo).unwrap();
        let hi = ElGamalCiphertext::from_bytes(&data.auditor_ciphertext_hi).unwrap();
        assert_eq!(
            lo.decrypt_u32(auditor.secret()).unwrap()
                + (hi.decrypt_u32(auditor.secret()).unwrap() << LO_BITS),
            amount
        );
    }
    // Corrupt each transfer separately; a surplus failure must also undo payment.
    for surplus in [false, true] {
        let mut bad = args.clone();
        let transfer = if surplus {
            bad.surplus_b.as_mut().unwrap()
        } else {
            &mut bad.payment
        };
        transfer.auditor_ciphertext_lo = auditor.pubkey().encrypt(1u64).to_bytes();
        assert_failure(
            &mut context,
            &f,
            accounts.instruction(bad),
            InstructionError::Custom(
                spl_token_2022_interface::error::TokenError::ConfidentialTransferBalanceMismatch
                    as u32,
            ),
            false,
        );
    }
    send_v1(&mut context, &[ix], &[&f.authority]).unwrap();
    assert_eq!(
        pending(
            &context,
            &accounts.user_a_destination_ata_b,
            &destination_keys
        ),
        AMOUNT_B
    );
    assert_eq!(pending(&context, &accounts.user_b_ata_b, &refund_keys), 9);
    for address in [accounts.swap_dvp, accounts.dvp_ata_a, accounts.dvp_ata_b] {
        assert!(context.get_account(&address).is_none());
    }
}

#[test]
fn hook_changed_after_funding_rejects_atomically() {
    use crate::confidential_utils::{send_and_assert_hook_rejection, switch_to_rejecting_hook};

    let mut context = TestContext::new();
    let (f, mut accounts, destination_keys, refund_keys) = setup(&mut context, 17, 9, true, false);
    let mut ix = prepare_settle(
        &mut context,
        &f,
        &mut accounts,
        &destination_keys,
        &refund_keys,
        Some(9),
    );
    ix.accounts
        .extend(switch_to_rejecting_hook(&mut context, &f.accounts.mint_b));
    send_and_assert_hook_rejection(&mut context, ix, &f.authority);
}
