//! CreateConfidentialDvp account setup, terms and validation.
use crate::{
    confidential_utils::{
        assert_error, create_wallet_account, pending, prepare_funding_transfer, send_v1, state,
        ConfidentialDvpFixture, Keys, MAX_PENDING,
    },
    state_utils::{AMOUNT_A, AMOUNT_B},
    utils::{
        create_ata, set_mint, TestContext, SWAP_PROGRAM_ID, TOKEN_2022_PROGRAM_ID as TOKEN,
        TOKEN_PROGRAM_ID,
    },
};
use dvp_swap_program_client::{
    accounts::SwapDvp, instructions::CreateConfidentialDvpInstructionArgs,
    verify::SWAP_DVP_ACCOUNT_LEN, DvpSwapProgramError as Error,
};
use solana_account::Account;
use solana_instruction::{error::InstructionError, Instruction};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token_2022_interface::{
    error::TokenError,
    extension::confidential_transfer::instruction as ct_ix,
    extension::{transfer_hook::TransferHookAccount, BaseStateWithExtensions, StateWithExtensions},
    instruction as token_ix,
    state::Account as TokenAccount,
};

// Public base followed by the two fixed-width ElGamal ciphertexts.
const CONFIDENTIAL_SWAP_LEN: usize = SWAP_DVP_ACCOUNT_LEN + 2 * 64;

#[test]
fn create_checks_instructions_sysvar_before_cpi() {
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, true, false);
    let mut instructions = f.create_instructions();
    instructions[1].accounts[14].pubkey = solana_sdk_ids::sysvar::clock::ID;
    let failure = send_v1(&mut context, &instructions, &[]).unwrap_err();
    assert_eq!(
        failure.err,
        solana_transaction::TransactionError::InstructionError(
            1,
            InstructionError::UnsupportedSysvar,
        ),
        "{:?}",
        failure.meta.logs
    );
    assert!(!failure.meta.logs.iter().any(|s| s.contains("invoke [2]")));
    for address in [
        f.accounts.swap_dvp,
        f.accounts.nonce_tombstone,
        f.accounts.dvp_ata_a,
        f.accounts.dvp_ata_b,
    ] {
        assert!(context.get_account(&address).is_none());
    }
    f.create(&mut context);
}

#[test]
fn create_configures_escrow_and_preserves_terms() {
    for hook in [false, true] {
        let mut context = TestContext::new();
        let mut f = ConfidentialDvpFixture::new(&mut context, true, hook);
        let destination_a = Pubkey::new_unique();
        let destination_b = Pubkey::new_unique();
        let earliest = context.now() + 60;
        f.args.ref_string = Some("CT-TRADE".into());
        f.args.user_a_settlement_destination = Some(destination_a);
        f.args.user_b_settlement_destination = Some(destination_b);
        f.args.earliest_settlement_timestamp = Some(earliest);
        f.create(&mut context);
        let swap = context.get_account(&f.accounts.swap_dvp).unwrap();
        assert_eq!(swap.data.len(), CONFIDENTIAL_SWAP_LEN);
        assert_eq!(
            swap.lamports,
            context
                .svm
                .minimum_balance_for_rent_exemption(CONFIDENTIAL_SWAP_LEN)
        );
        let base = SwapDvp::from_bytes(&swap.data[..SWAP_DVP_ACCOUNT_LEN]).unwrap();
        assert_eq!(base.amount_a, AMOUNT_A);
        assert_eq!(base.amount_b, u64::MAX);
        assert_eq!(base.user_a, f.user_a.pubkey());
        assert_eq!(base.user_b, f.user_b.pubkey());
        assert_eq!(base.settlement_authority, f.authority.pubkey());
        assert_eq!(base.user_a_settlement_destination, destination_a);
        assert_eq!(base.user_b_settlement_destination, destination_b);
        assert_eq!(base.earliest_settlement_timestamp, Some(earliest));
        assert_eq!(&base.ref_string[..8], b"CT-TRADE");
        assert_eq!(
            &swap.data[SWAP_DVP_ACCOUNT_LEN..],
            [f.args.amount_b_ciphertext_lo, f.args.amount_b_ciphertext_hi].concat()
        );
        let tombstone = context.get_account(&f.accounts.nonce_tombstone).unwrap();
        assert_eq!(tombstone.owner, SWAP_PROGRAM_ID);
        assert!(tombstone.data.is_empty());
        let escrow = context.get_account(&f.accounts.dvp_ata_b).unwrap();
        assert_eq!(
            escrow.lamports,
            context
                .svm
                .minimum_balance_for_rent_exemption(escrow.data.len())
        );
        let token = StateWithExtensions::<TokenAccount>::unpack(&escrow.data).unwrap();
        assert_eq!(token.base.owner, f.accounts.swap_dvp);
        assert_eq!(token.base.mint, f.accounts.mint_b);
        assert_eq!(token.base.amount, 0);
        assert_eq!(token.get_extension::<TransferHookAccount>().is_ok(), hook);
        let ct = state(&context, &f.accounts.dvp_ata_b);
        assert!(bool::from(ct.approved));
        assert!(bool::from(ct.allow_confidential_credits));
        assert!(!bool::from(ct.allow_non_confidential_credits));
        assert_eq!(ct.elgamal_pubkey, (*f.keys.elgamal.pubkey()).into());
        assert_eq!(
            u64::from(ct.maximum_pending_balance_credit_counter),
            MAX_PENDING
        );
        assert_eq!(ct.available_balance.0, [0; 64]);
        assert_eq!(pending(&context, &f.accounts.dvp_ata_b, &f.keys), 0);
        assert_eq!(
            f.keys
                .ae
                .decrypt(&ct.decryptable_available_balance.try_into().unwrap()),
            Some(0)
        );
    }
}

#[test]
fn manual_approval_blocks_funding_until_approved() {
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, false, false);
    f.create(&mut context);
    assert!(!bool::from(state(&context, &f.accounts.dvp_ata_b).approved));
    let source_keys = Keys::new();
    let source = create_wallet_account(&mut context, &f.user_b, &f.accounts.mint_b, &source_keys);
    let approve = |account| {
        ct_ix::approve_account(
            &TOKEN,
            &account,
            &f.accounts.mint_b,
            &context.payer.pubkey(),
            &[],
        )
        .unwrap()
    };
    let approve_source = approve(source);
    let approve_escrow = approve(f.accounts.dvp_ata_b);
    send_v1(&mut context, &[approve_source], &[]).unwrap();
    let funding = prepare_funding_transfer(
        &mut context,
        &f.accounts.mint_b,
        &f.user_b,
        &source,
        &source_keys,
        &f.accounts.dvp_ata_b,
        &f.keys,
        AMOUNT_B,
        &[],
    );
    let before = context.get_account(&f.accounts.dvp_ata_b);
    assert_error(
        &mut context,
        &funding,
        &[&f.user_b],
        0,
        InstructionError::Custom(TokenError::ConfidentialTransferAccountNotApproved as u32),
    );
    assert_eq!(context.get_account(&f.accounts.dvp_ata_b), before);
    send_v1(&mut context, &[approve_escrow], &[]).unwrap();
    send_v1(&mut context, &funding, &[&f.user_b]).unwrap();
    send_v1(
        &mut context,
        &[f.apply(f.user_b.pubkey(), AMOUNT_B).instruction()],
        &[&f.user_b],
    )
    .unwrap();
    assert_eq!(pending(&context, &f.accounts.dvp_ata_b, &f.keys), 0);
}

#[test]
fn create_rejects_public_balance_and_lamport_preloads_atomically() {
    enum Preload {
        PublicTokens,
        EscrowA,
        EscrowB,
        Swap,
    }
    for preload in [
        Preload::PublicTokens,
        Preload::EscrowB,
        Preload::EscrowA,
        Preload::Swap,
    ] {
        let mut context = TestContext::new();
        let f = ConfidentialDvpFixture::new(&mut context, true, false);
        let (address, expected) = match preload {
            Preload::PublicTokens => (f.accounts.dvp_ata_b, Error::EscrowPublicBalanceNotEmpty),
            Preload::EscrowA => (f.accounts.dvp_ata_a, Error::EscrowPreloadedWithLamports),
            Preload::EscrowB => (f.accounts.dvp_ata_b, Error::EscrowPreloadedWithLamports),
            Preload::Swap => (f.accounts.swap_dvp, Error::SwapDvpPreloadedWithLamports),
        };
        if matches!(preload, Preload::Swap) {
            context
                .svm
                .set_account(
                    address,
                    Account {
                        lamports: context
                            .svm
                            .minimum_balance_for_rent_exemption(CONFIDENTIAL_SWAP_LEN)
                            + 1,
                        ..Account::default()
                    },
                )
                .unwrap();
        } else {
            let (mint, token) = if matches!(preload, Preload::EscrowA) {
                (f.accounts.mint_a, TOKEN_PROGRAM_ID)
            } else {
                (f.accounts.mint_b, TOKEN)
            };
            create_ata(&mut context, &f.accounts.swap_dvp, &mint, &token);
            if matches!(preload, Preload::PublicTokens) {
                let mint =
                    token_ix::mint_to(&TOKEN, &mint, &address, &context.payer.pubkey(), &[], 1)
                        .unwrap();
                send_v1(&mut context, &[mint], &[]).unwrap();
            } else {
                let mut account = context.get_account(&address).unwrap();
                account.lamports += 1;
                context.svm.set_account(address, account).unwrap();
            }
        }
        let before = context.get_account(&address);
        assert_error(
            &mut context,
            &f.create_instructions(),
            &[],
            1,
            InstructionError::Custom(expected as u32),
        );
        assert_eq!(context.get_account(&address), before);
        assert!(context.get_account(&f.accounts.nonce_tombstone).is_none());
        if !matches!(preload, Preload::Swap) {
            assert!(context.get_account(&f.accounts.swap_dvp).is_none());
        }
    }
}

#[test]
fn create_accepts_rent_preloads_and_precreated_empty_escrow() {
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, true, false);
    context
        .svm
        .set_account(
            f.accounts.swap_dvp,
            Account {
                lamports: context
                    .svm
                    .minimum_balance_for_rent_exemption(CONFIDENTIAL_SWAP_LEN),
                ..Account::default()
            },
        )
        .unwrap();
    create_ata(
        &mut context,
        &f.accounts.swap_dvp,
        &f.accounts.mint_b,
        &TOKEN,
    );
    let old_size = context
        .get_account(&f.accounts.dvp_ata_b)
        .unwrap()
        .data
        .len();
    f.create(&mut context);
    assert!(
        context
            .get_account(&f.accounts.dvp_ata_b)
            .unwrap()
            .data
            .len()
            > old_size
    );
    f.close_fixture(&mut context);
    assert_error(
        &mut context,
        &f.create_instructions(),
        &[],
        1,
        InstructionError::Custom(Error::NonceAlreadyUsed as u32),
    );
}

#[test]
fn create_requires_confidential_mint_and_valid_proof() {
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, true, false);
    let original = context.get_account(&f.accounts.mint_b).unwrap();
    for owner in [TOKEN_PROGRAM_ID, TOKEN] {
        set_mint(&mut context, &f.accounts.mint_b, &owner);
        assert_error(
            &mut context,
            &f.create_instructions(),
            &[],
            1,
            InstructionError::Custom(Error::MintNotConfidential as u32),
        );
    }
    context
        .svm
        .set_account(f.accounts.mint_b, original)
        .unwrap();
    // Offset -1 must point to a verified PubkeyValidity instruction, not a memo.
    let mut instructions = f.create_instructions();
    instructions[0] = Instruction {
        program_id: crate::utils::MEMO_PROGRAM_ID,
        accounts: vec![],
        data: b"not a proof".to_vec(),
    };
    assert_error(
        &mut context,
        &instructions,
        &[],
        1,
        InstructionError::InvalidInstructionData,
    );
    for address in [
        f.accounts.swap_dvp,
        f.accounts.nonce_tombstone,
        f.accounts.dvp_ata_a,
        f.accounts.dvp_ata_b,
    ] {
        assert!(context.get_account(&address).is_none());
    }
    f.create(&mut context);
}

#[test]
fn create_reuses_public_argument_and_account_checks() {
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, true, false);
    let base_args = f.args.clone();
    for (args, expected) in [
        (
            CreateConfidentialDvpInstructionArgs {
                amount_a: 0,
                ..base_args.clone()
            },
            Error::ZeroAmount,
        ),
        (
            CreateConfidentialDvpInstructionArgs {
                expiry_timestamp: context.now(),
                ..base_args.clone()
            },
            Error::ExpiryNotInFuture,
        ),
        (
            CreateConfidentialDvpInstructionArgs {
                earliest_settlement_timestamp: Some(base_args.expiry_timestamp + 1),
                ..base_args.clone()
            },
            Error::EarliestAfterExpiry,
        ),
        (
            CreateConfidentialDvpInstructionArgs {
                user_a_settlement_destination: Some(f.accounts.swap_dvp),
                ..base_args.clone()
            },
            Error::SettlementDestinationIsSwapDvp,
        ),
    ] {
        let mut instructions = f.create_instructions();
        instructions[1] = f.accounts.instruction(args);
        assert_error(
            &mut context,
            &instructions,
            &[],
            1,
            InstructionError::Custom(expected as u32),
        );
    }
    // Account positions are the shared public Create prefix.
    for (index, address, expected) in [
        (1, Pubkey::new_unique(), InstructionError::InvalidSeeds), // swap PDA
        (
            2,
            Pubkey::new_unique(),
            InstructionError::InvalidAccountData,
        ), // tombstone
        (
            4,
            f.user_b.pubkey(),
            InstructionError::Custom(Error::SelfDvp as u32),
        ),
        (
            4,
            f.accounts.mint_b,
            InstructionError::Custom(Error::PartyNotSignerCapable as u32),
        ),
        (8, f.accounts.dvp_ata_b, InstructionError::InvalidSeeds), // leg A ATA
        (9, f.accounts.dvp_ata_a, InstructionError::InvalidSeeds), // leg B ATA
        (
            12,
            TOKEN_PROGRAM_ID,
            InstructionError::Custom(Error::MintNotConfidential as u32),
        ),
    ] {
        let mut instructions = f.create_instructions();
        instructions[1].accounts[index].pubkey = address;
        assert_error(&mut context, &instructions, &[], 1, expected);
    }
    let mut instructions = f.create_instructions();
    // Use a different payer so the transaction fee payer does not restore its signer flag.
    instructions[1].accounts[0] = solana_instruction::AccountMeta::new(Pubkey::new_unique(), false);
    assert_error(
        &mut context,
        &instructions,
        &[],
        1,
        InstructionError::MissingRequiredSignature,
    );
    for address in [
        f.accounts.swap_dvp,
        f.accounts.nonce_tombstone,
        f.accounts.dvp_ata_a,
        f.accounts.dvp_ata_b,
    ] {
        assert!(context.get_account(&address).is_none());
    }
}
