//! Token-2022 confidential account checks shared by the lifecycle handlers.

use pinocchio::{account::AccountView, error::ProgramError, ProgramResult};
use pinocchio_token_2022::ID as TOKEN_2022_PROGRAM_ID;
use spl_token_2022_interface::{
    extension::{
        confidential_transfer::{ConfidentialTransferAccount, ConfidentialTransferMint},
        ExtensionType, StateWithExtensions,
    },
    state::Account,
};

use crate::{
    error::DvpSwapProgramError,
    processor::shared::token_utils::{
        find_account_extension, validate_token_2022_mint_extensions, CheckedMint,
    },
    require,
};

#[inline(always)]
fn confidential_account(
    account: &AccountView,
) -> Result<Option<ConfidentialTransferAccount>, ProgramError> {
    if !account.owned_by(&TOKEN_2022_PROGRAM_ID) {
        return Ok(None);
    }
    let data = account.try_borrow()?;
    let state = StateWithExtensions::<Account>::unpack(&data)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    Ok(find_account_extension::<ConfidentialTransferAccount>(&state)?.copied())
}

/// Returns an owned view so no account-data borrow survives a subsequent CPI.
#[inline(always)]
pub fn read_confidential_account(
    account: &AccountView,
) -> Result<ConfidentialTransferAccount, ProgramError> {
    confidential_account(account)?.ok_or(DvpSwapProgramError::EscrowNotConfidential.into())
}

/// Checks the four confidential recipient requirements before a Transfer CPI.
#[inline(always)]
pub fn check_confidential_recipient(account: &AccountView) -> ProgramResult {
    let state =
        confidential_account(account)?.ok_or(DvpSwapProgramError::RecipientNotConfidential)?;
    require!(
        bool::from(state.approved),
        DvpSwapProgramError::RecipientNotApproved
    );
    require!(
        bool::from(state.allow_confidential_credits),
        DvpSwapProgramError::RecipientConfidentialCreditsDisabled
    );
    require!(
        u64::from(state.pending_balance_credit_counter)
            < u64::from(state.maximum_pending_balance_credit_counter),
        DvpSwapProgramError::RecipientPendingCounterFull
    );
    Ok(())
}

/// The extension identifies confidential escrow after SwapDvp has closed.
/// Malformed TLV is rejected, so it cannot hide the extension from the guard.
#[inline(always)]
pub fn has_confidential_transfer_account(info: &AccountView) -> Result<bool, ProgramError> {
    Ok(confidential_account(info)?.is_some())
}

/// Validate and read leg B's extensions once before configuring the escrow.
#[inline(always)]
pub fn verify_confidential_create_mint(mint: &AccountView) -> ProgramResult {
    require!(
        mint.owned_by(&TOKEN_2022_PROGRAM_ID),
        DvpSwapProgramError::MintNotConfidential
    );
    let data = mint.try_borrow()?;
    let state = CheckedMint::unpack(&data)?;
    let config = state
        .extension::<ConfidentialTransferMint>()?
        .ok_or(DvpSwapProgramError::MintNotConfidential)?;
    // Without auto-approval, only the CT authority can approve the escrow.
    require!(
        bool::from(config.auto_approve_new_accounts) || config.authority.as_ref().is_some(),
        DvpSwapProgramError::MintConfidentialApprovalImpossible
    );
    // Even a zero-rate fee config requires fee proofs on confidential transfers.
    require!(
        !state.has(ExtensionType::TransferFeeConfig) && !state.has(ExtensionType::NonTransferable),
        DvpSwapProgramError::BlockedMintExtension
    );
    Ok(())
}

/// Leg B still needs the CT config at Settle. Its escrow is already approved.
#[inline(always)]
pub fn verify_confidential_settle_mint(mint: &AccountView, epoch: u64) -> ProgramResult {
    let data = mint.try_borrow()?;
    let state = CheckedMint::unpack(&data)?;
    validate_token_2022_mint_extensions(&state, epoch, false)?;
    state
        .extension::<ConfidentialTransferMint>()?
        .ok_or(DvpSwapProgramError::MintNotConfidential)?;
    Ok(())
}
