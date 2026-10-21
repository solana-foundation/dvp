//! Test-only PDA driver for the production CT helpers. This exercises the CPI
//! boundaries without enabling the unfinished DvP lifecycle instructions.

use dvp_swap_program::processor::shared::*;
use pinocchio::{
    account::AccountView,
    cpi::{Seed, Signer},
    default_allocator,
    error::ProgramError,
    program_entrypoint, Address, ProgramResult,
};

solana_address::declare_id!("FCDHD6mkL4c7hMLxdLy2a9aCjraYJJwpF6gnbukwHRV5");
program_entrypoint!(process_instruction);
default_allocator!();

pub const AUTHORITY_SEED: &[u8] = b"ct-test";

pub fn process_instruction(
    program_id: &Address,
    accounts: &[AccountView],
    data: &[u8],
) -> ProgramResult {
    let (tag, data) = data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    let (address, bump) = Address::find_program_address(&[AUTHORITY_SEED], program_id);
    let bump = [bump];
    let seeds = [Seed::from(AUTHORITY_SEED), Seed::from(bump.as_slice())];
    let signers = [Signer::from(&seeds)];
    match tag {
        0 => configure(accounts, data, &address, &signers),
        1 => apply(accounts, data, &address, &signers),
        2 => transfer_and_reset(accounts, data, &address, &signers),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn verify_authority(account: &AccountView, expected: &Address) -> ProgramResult {
    if account.address() != expected {
        return Err(ProgramError::InvalidSeeds);
    }
    Ok(())
}

// Keep fixture dispatch frames separate from the bounded transfer CPI frame.
#[inline(never)]
fn configure(
    accounts: &[AccountView],
    data: &[u8],
    expected: &Address,
    signers: &[Signer],
) -> ProgramResult {
    let [payer, authority, escrow, mint, instructions, _token, system] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    verify_authority(authority, expected)?;
    let balance: &[u8; AE_CIPHERTEXT_LEN] = data
        .get(..AE_CIPHERTEXT_LEN)
        .ok_or(ProgramError::InvalidInstructionData)?
        .try_into()
        .unwrap();
    let offset = *data
        .get(AE_CIPHERTEXT_LEN)
        .ok_or(ProgramError::InvalidInstructionData)? as i8;
    reallocate_confidential_account_cpi(escrow, payer, system, authority, signers)?;
    configure_confidential_account_cpi(
        escrow,
        mint,
        instructions,
        authority,
        balance,
        offset,
        signers,
    )?;
    disable_non_confidential_credits_cpi(escrow, authority, signers)
}

#[inline(never)]
fn apply(
    accounts: &[AccountView],
    data: &[u8],
    expected: &Address,
    signers: &[Signer],
) -> ProgramResult {
    let [authority, escrow, _token] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    verify_authority(authority, expected)?;
    let (counter, balance) = data
        .split_at_checked(8)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let balance = balance
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    apply_pending_balance_cpi(
        escrow,
        authority,
        u64::from_le_bytes(counter.try_into().unwrap()),
        balance,
        signers,
    )
}

#[inline(never)]
fn transfer_and_reset(
    accounts: &[AccountView],
    data: &[u8],
    expected: &Address,
    signers: &[Signer],
) -> ProgramResult {
    let [authority, escrow, mint, destination, signer, eq_lo, eq_hi, equality, validity, range, zero, _token, _zk, memo, remaining @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    verify_authority(authority, expected)?;
    if !signer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if data.len() != CtTransferData::LEN + 2 * ELGAMAL_CIPHERTEXT_LEN {
        return Err(ProgramError::InvalidInstructionData);
    }
    let (transfer, stored) = data.split_at(CtTransferData::LEN);
    let transfer = CtTransferData::try_from(transfer)?;
    let (stored_lo, stored_hi) = stored.split_at(ELGAMAL_CIPHERTEXT_LEN);
    let contexts = [
        (eq_lo, ProofType::CiphertextCiphertextEquality),
        (eq_hi, ProofType::CiphertextCiphertextEquality),
        (equality, ProofType::CiphertextCommitmentEquality),
        (
            validity,
            ProofType::BatchedGroupedCiphertext3HandlesValidity,
        ),
        (range, ProofType::BatchedRangeProofU128),
        (zero, ProofType::ZeroCiphertext),
    ];
    // Every context is checked before the first CPI, including the zero context
    // whose ciphertext is only matched after the transfer changes available.
    for (context, kind) in contexts {
        check_proof_context(context, kind, signer.address())?;
    }
    check_confidential_amount(
        escrow,
        validity,
        eq_lo,
        eq_hi,
        stored_lo.try_into().unwrap(),
        stored_hi.try_into().unwrap(),
        signer.address(),
    )?;
    confidential_transfer_cpi(
        escrow,
        mint,
        destination,
        authority,
        equality,
        validity,
        range,
        signer.address(),
        &transfer,
        memo,
        remaining,
        signers,
    )?;
    empty_confidential_account_if_no_pending(escrow, zero, authority, signer.address(), signers)?;
    for (context, kind) in contexts {
        close_proof_context_cpi(context, kind, signer)?;
    }
    Ok(())
}
