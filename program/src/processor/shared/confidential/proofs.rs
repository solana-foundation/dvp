//! Validation of pre-verified ZK contexts used by confidential transfers.

use core::mem::size_of;
use pinocchio::{account::AccountView, error::ProgramError, Address, ProgramResult};
use spl_token_2022::extension::confidential_transfer::ConfidentialTransferAccount;
pub use spl_token_2022::solana_zk_sdk::zk_elgamal_proof_program::proof_data::ProofType;
use spl_token_2022::solana_zk_sdk::zk_elgamal_proof_program::{
    self,
    proof_data::{
        BatchedGroupedCiphertext3HandlesValidityProofContext, BatchedRangeProofContext,
        CiphertextCiphertextEqualityProofContext, CiphertextCommitmentEqualityProofContext,
        ZeroCiphertextProofContext,
    },
    state::{ProofContextState, ProofContextStateMeta},
};

use super::{read_confidential_account, ELGAMAL_CIPHERTEXT_LEN};
use crate::{error::DvpSwapProgramError, require};

pub const ZK_ELGAMAL_PROOF_PROGRAM_ID: Address =
    Address::new_from_array(zk_elgamal_proof_program::ID.to_bytes());
const PUBKEY_LEN: usize = 32;
// Context-state header: authority pubkey, then the ProofType byte.
pub const PROOF_CONTEXT_HEADER_LEN: usize = size_of::<ProofContextStateMeta>();
// A grouped ciphertext has one commitment and three decryption handles.
const GROUPED_CIPHERTEXT_LEN: usize = PUBKEY_LEN * 4;

/// Checks owner, exact size, proof type and authority before any consuming CPI.
/// The caller must validate every context in the operation before its first CPI.
#[inline(always)]
pub fn check_proof_context(
    account: &AccountView,
    kind: ProofType,
    authority: &Address,
) -> ProgramResult {
    // Only accept proof kinds used by DvP, with their complete SDK account layout.
    let expected_len = match kind {
        ProofType::ZeroCiphertext => size_of::<ProofContextState<ZeroCiphertextProofContext>>(),
        ProofType::CiphertextCiphertextEquality => {
            size_of::<ProofContextState<CiphertextCiphertextEqualityProofContext>>()
        }
        ProofType::CiphertextCommitmentEquality => {
            size_of::<ProofContextState<CiphertextCommitmentEqualityProofContext>>()
        }
        ProofType::BatchedRangeProofU128 => {
            size_of::<ProofContextState<BatchedRangeProofContext>>()
        }
        ProofType::BatchedGroupedCiphertext3HandlesValidity => {
            size_of::<ProofContextState<BatchedGroupedCiphertext3HandlesValidityProofContext>>()
        }
        _ => return Err(DvpSwapProgramError::InvalidProofContext.into()),
    };
    // Require a ZK-program-owned context with no missing or trailing bytes.
    // The exact size also makes the header reads below safe.
    require!(
        account.owned_by(&ZK_ELGAMAL_PROOF_PROGRAM_ID) && account.data_len() == expected_len,
        DvpSwapProgramError::InvalidProofContext
    );
    let data = account.try_borrow()?;
    // The byte after the authority key identifies the verified proof kind.
    // Size alone cannot distinguish proof kinds with identical layouts.
    require!(
        data[PUBKEY_LEN] == kind as u8,
        DvpSwapProgramError::InvalidProofContext
    );
    // The header starts with the authority allowed to close this context.
    // Bind it to the expected authority for the current operation.
    require!(
        &data[..PUBKEY_LEN] == authority.as_ref(),
        DvpSwapProgramError::ProofContextAuthorityMismatch
    );
    Ok(())
}

/// Checks the three pre-verified contexts required by Token-2022 CT Transfer.
/// All must belong to the expected authority. This validates their metadata;
/// Token-2022 checks their contents against each other and the transfer during CPI.
#[inline(always)]
pub fn check_transfer_proof_contexts(
    equality: &AccountView,
    validity: &AccountView,
    range: &AccountView,
    authority: &Address,
) -> ProgramResult {
    // Equality links the source's remaining-balance ciphertext to a commitment
    // used by the range proof.
    check_proof_context(equality, ProofType::CiphertextCommitmentEquality, authority)?;
    // Validity covers the low/high transfer ciphertexts with decryption handles
    // for the source, destination and auditor.
    check_proof_context(
        validity,
        ProofType::BatchedGroupedCiphertext3HandlesValidity,
        authority,
    )?;
    // Token-2022 CT Transfer requires U128: 64 bits for the remaining balance,
    // 16 for transfer lo, 32 for transfer hi and 16 for padding.
    check_proof_context(range, ProofType::BatchedRangeProofU128, authority)
}

/// Binds both payment limbs to the stored amount under the escrow's key.
/// Pass this same validity account to the payment Transfer CPI.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub fn check_confidential_amount(
    escrow: &AccountView,
    validity: &AccountView,
    eq_lo: &AccountView,
    eq_hi: &AccountView,
    stored_lo: &[u8; ELGAMAL_CIPHERTEXT_LEN],
    stored_hi: &[u8; ELGAMAL_CIPHERTEXT_LEN],
    authority: &Address,
) -> ProgramResult {
    check_proof_context(
        validity,
        ProofType::BatchedGroupedCiphertext3HandlesValidity,
        authority,
    )?;
    check_proof_context(eq_lo, ProofType::CiphertextCiphertextEquality, authority)?;
    check_proof_context(eq_hi, ProofType::CiphertextCiphertextEquality, authority)?;
    let state = read_confidential_account(escrow)?;
    let data = validity.try_borrow()?;
    let context = &data[PROOF_CONTEXT_HEADER_LEN..];
    require!(
        &context[..PUBKEY_LEN] == bytemuck::bytes_of(&state.elgamal_pubkey),
        DvpSwapProgramError::ConfidentialAmountBMismatch
    );
    // Three pubkeys precede grouped_lo and grouped_hi. The first two elements
    // of each group (commitment + source handle) form the source ciphertext.
    let lo_offset = 3 * PUBKEY_LEN;
    let hi_offset = lo_offset + GROUPED_CIPHERTEXT_LEN;
    for (account, offset, stored) in [(eq_lo, lo_offset, stored_lo), (eq_hi, hi_offset, stored_hi)]
    {
        let data = account.try_borrow()?;
        let equality = &data[PROOF_CONTEXT_HEADER_LEN..];
        let first_ct = 2 * PUBKEY_LEN;
        let second_ct = first_ct + ELGAMAL_CIPHERTEXT_LEN;
        let escrow_key = bytemuck::bytes_of(&state.elgamal_pubkey);
        let transfer_ct = &context[offset..offset + ELGAMAL_CIPHERTEXT_LEN];
        require!(
            // First key: transfer ciphertext uses the escrow key.
            &equality[..PUBKEY_LEN] == escrow_key
                // Second key: stored ciphertext uses the same escrow key.
                && &equality[PUBKEY_LEN..first_ct] == escrow_key
                // First ciphertext: this limb of the prepared transfer.
                && &equality[first_ct..second_ct] == transfer_ct
                // Second ciphertext: the corresponding limb stored in the swap.
                && &equality[second_ct..] == stored,
            DvpSwapProgramError::ConfidentialAmountBMismatch
        );
    }
    Ok(())
}

/// Matches the zero proof to the actual available ciphertext after transfers.
/// Returns the checked state so callers can inspect pending credits without reparsing.
/// Pending credits are deliberately excluded from this check.
#[inline(always)]
pub fn check_confidential_zero(
    escrow: &AccountView,
    zero: &AccountView,
    authority: &Address,
) -> Result<ConfidentialTransferAccount, ProgramError> {
    check_proof_context(zero, ProofType::ZeroCiphertext, authority)?;
    let state = read_confidential_account(escrow)?;
    let data = zero.try_borrow()?;
    let context = &data[PROOF_CONTEXT_HEADER_LEN..];
    require!(
        &context[..PUBKEY_LEN] == bytemuck::bytes_of(&state.elgamal_pubkey)
            && &context[PUBKEY_LEN..] == bytemuck::bytes_of(&state.available_balance),
        DvpSwapProgramError::EscrowBalanceNotZero
    );
    Ok(state)
}
