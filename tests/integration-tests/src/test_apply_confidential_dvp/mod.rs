//! ApplyConfidentialDvp while open and after a fixture-modeled close.
use crate::{
    confidential_utils::{
        assert_error, create_wallet_account, pending, prepare_funding_transfer, send_v1, state,
        ConfidentialDvpFixture, Keys,
    },
    state_utils::AMOUNT_B,
    utils::{
        create_ata, get_token_balance, TestContext, TOKEN_2022_PROGRAM_ID as TOKEN,
        TOKEN_PROGRAM_ID,
    },
};
use dvp_swap_program_client::{verify::SWAP_DVP_ACCOUNT_LEN, DvpSwapProgramError as Error};
use solana_account::Account;
use solana_instruction::error::InstructionError;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

#[test]
fn funding_and_apply_by_each_party_open_and_after_close() {
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, true, false);
    f.create(&mut context);
    let source_keys = Keys::new();
    let source = create_wallet_account(&mut context, &f.user_b, &f.accounts.mint_b, &source_keys);
    let mut available = 0;
    for closed in [false, true] {
        if closed {
            f.close_fixture(&mut context);
        }
        for signer in [&f.user_a, &f.user_b, &f.authority] {
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
            send_v1(&mut context, &funding, &[&f.user_b]).unwrap();
            assert_eq!(pending(&context, &f.accounts.dvp_ata_b, &f.keys), AMOUNT_B);
            available += AMOUNT_B;
            let ix = f.apply(signer.pubkey(), available).instruction();
            send_v1(&mut context, &[ix], &[signer]).unwrap();
            let ct = state(&context, &f.accounts.dvp_ata_b);
            assert_eq!(pending(&context, &f.accounts.dvp_ata_b, &f.keys), 0);
            assert_eq!(u64::from(ct.pending_balance_credit_counter), 0);
            assert_eq!(
                f.keys
                    .ae
                    .decrypt(&ct.decryptable_available_balance.try_into().unwrap()),
                Some(available)
            );
            let ciphertext: solana_zk_sdk::encryption::elgamal::ElGamalCiphertext =
                ct.available_balance.try_into().unwrap();
            assert_eq!(
                ciphertext.decrypt_u32(f.keys.elgamal.secret()),
                Some(available)
            );
        }
    }
}

#[test]
fn apply_checks_signer_seeds_ata_program_and_tombstone() {
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, true, false);
    f.create(&mut context);
    let stranger = Keypair::new();
    let before = context.get_account(&f.accounts.dvp_ata_b);
    for closed in [false, true] {
        if closed {
            f.close_fixture(&mut context);
        }
        for (index, address, expected) in [
            (1, Pubkey::new_unique(), InstructionError::InvalidSeeds),
            (
                2,
                Pubkey::new_unique(),
                InstructionError::InvalidAccountData,
            ),
            (3, f.accounts.dvp_ata_a, InstructionError::InvalidSeeds),
            (4, TOKEN_PROGRAM_ID, InstructionError::IncorrectProgramId),
        ] {
            let mut ix = f.apply(f.user_a.pubkey(), 0).instruction();
            ix.accounts[index].pubkey = address;
            assert_error(&mut context, &[ix], &[&f.user_a], 0, expected);
        }
        let mut missing_signer = f.apply(stranger.pubkey(), 0).instruction();
        missing_signer.accounts[0].is_signer = false;
        assert_error(
            &mut context,
            &[missing_signer],
            &[],
            0,
            InstructionError::MissingRequiredSignature,
        );
        assert_error(
            &mut context,
            &[f.apply(stranger.pubkey(), 0).instruction()],
            &[&stranger],
            0,
            InstructionError::Custom(Error::SignerNotParty as u32),
        );
        let mut wrong_seeds = f.apply(f.user_a.pubkey(), 0);
        wrong_seeds.nonce(43);
        assert_error(
            &mut context,
            &[wrong_seeds.instruction()],
            &[&f.user_a],
            0,
            InstructionError::InvalidSeeds,
        );
        assert_eq!(context.get_account(&f.accounts.dvp_ata_b), before);
    }
    context
        .svm
        .set_account(f.accounts.nonce_tombstone, Account::default())
        .unwrap();
    assert_error(
        &mut context,
        &[f.apply(f.user_a.pubkey(), 0).instruction()],
        &[&f.user_a],
        0,
        InstructionError::Custom(Error::DvpNeverCreated as u32),
    );
    assert_eq!(context.get_account(&f.accounts.dvp_ata_b), before);
}

#[test]
fn apply_rejects_public_mode_and_forwards_unchecked_balance_metadata() {
    let mut context = TestContext::new();
    let f = ConfidentialDvpFixture::new(&mut context, true, false);
    f.create(&mut context);
    let original = context.get_account(&f.accounts.swap_dvp).unwrap();
    let mut public = original.clone();
    public.data.truncate(SWAP_DVP_ACCOUNT_LEN);
    context
        .svm
        .set_account(f.accounts.swap_dvp, public)
        .unwrap();
    assert_error(
        &mut context,
        &[f.apply(f.user_a.pubkey(), 0).instruction()],
        &[&f.user_a],
        0,
        InstructionError::Custom(Error::SwapModeMismatch as u32),
    );
    context
        .svm
        .set_account(f.accounts.swap_dvp, original)
        .unwrap();
    let mut apply = f.apply(f.user_a.pubkey(), 999);
    apply.expected_pending_balance_credit_counter(123);
    send_v1(&mut context, &[apply.instruction()], &[&f.user_a]).unwrap();
    let ct = state(&context, &f.accounts.dvp_ata_b);
    assert_eq!(u64::from(ct.expected_pending_balance_credit_counter), 123);
    assert_eq!(u64::from(ct.actual_pending_balance_credit_counter), 0);
    assert_eq!(
        f.keys
            .ae
            .decrypt(&ct.decryptable_available_balance.try_into().unwrap()),
        Some(999)
    );
    assert_eq!(get_token_balance(&context, &f.accounts.dvp_ata_b), 0);
    // A tombstone authenticates the parties, but a plain token escrow is still rejected.
    f.close_fixture(&mut context);
    context
        .svm
        .set_account(f.accounts.dvp_ata_b, Account::default())
        .unwrap();
    create_ata(
        &mut context,
        &f.accounts.swap_dvp,
        &f.accounts.mint_b,
        &TOKEN,
    );
    assert_error(
        &mut context,
        &[f.apply(f.user_a.pubkey(), 0).instruction()],
        &[&f.user_a],
        0,
        InstructionError::Custom(Error::EscrowNotConfidential as u32),
    );
}
