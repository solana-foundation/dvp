//! Token-2022 confidential account checks shared by the lifecycle handlers.

use pinocchio::{account::AccountView, error::ProgramError, ProgramResult};
use pinocchio_token_2022::ID as TOKEN_2022_PROGRAM_ID;
use spl_token_2022::{
    extension::{
        confidential_transfer::{ConfidentialTransferAccount, ConfidentialTransferMint},
        BaseStateWithExtensions, ExtensionType, StateWithExtensions,
    },
    state::{Account, Mint},
};

use crate::{error::DvpSwapProgramError, require};

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
    let types = state
        .get_extension_types()
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !types.contains(&ExtensionType::ConfidentialTransferAccount) {
        return Ok(None);
    }
    state
        .get_extension::<ConfidentialTransferAccount>()
        .copied()
        .map(Some)
        .map_err(|_| ProgramError::InvalidAccountData)
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
/// Parse the entire TLV list so malformed extension data cannot bypass the guard.
#[inline(always)]
pub fn has_confidential_transfer_account(info: &AccountView) -> Result<bool, ProgramError> {
    Ok(confidential_account(info)?.is_some())
}

/// Confidential leg B requires Token-2022 and the mint's CT configuration.
#[inline(always)]
pub fn verify_confidential_mint(mint: &AccountView) -> ProgramResult {
    require!(
        mint.owned_by(&TOKEN_2022_PROGRAM_ID),
        DvpSwapProgramError::MintNotConfidential
    );
    let data = mint.try_borrow()?;
    let state =
        StateWithExtensions::<Mint>::unpack(&data).map_err(|_| ProgramError::InvalidAccountData)?;
    state
        .get_extension::<ConfidentialTransferMint>()
        .map_err(|_| DvpSwapProgramError::MintNotConfidential)?;
    Ok(())
}
