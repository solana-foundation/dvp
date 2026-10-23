//! PDA-signed Token-2022 CT calls and signer-authorized ZK context closing.
//! Wire layouts match Token-2022's ConfidentialTransferInstruction.

use core::mem::size_of;
use pinocchio::{
    account::AccountView,
    cpi::{invoke, invoke_signed, invoke_signed_with_bounds, Signer},
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    Address, ProgramResult,
};
use pinocchio_token_2022::ID as TOKEN_2022_PROGRAM_ID;
use spl_token_2022::{
    extension::{
        confidential_transfer::instruction::{
            ApplyPendingBalanceData, ConfidentialTransferInstruction,
            ConfigureAccountInstructionData, EmptyAccountInstructionData, TransferInstructionData,
        },
        ExtensionType,
    },
    solana_zk_sdk::zk_elgamal_proof_program::instruction::ProofInstruction,
};

use super::{
    check_confidential_recipient, check_confidential_zero, check_proof_context,
    check_transfer_proof_contexts, CtTransferData, ProofType, AE_CIPHERTEXT_LEN,
    ZK_ELGAMAL_PROOF_PROGRAM_ID,
};
use crate::{
    processor::shared::{invoke_memo, requires_memo, MAX_HOOK_REMAINING_ACCOUNTS},
    require,
};

// TokenInstruction::Reallocate grows account data to fit the requested extensions.
const TOKEN_INSTRUCTION_RESIZE_ACCOUNT_DATA: u8 = 29;
// TokenInstruction::ConfidentialTransferExtension selects the CT sub-instruction family.
const TOKEN_INSTRUCTION_CONFIDENTIAL_TRANSFER_EXTENSION: u8 = 27;
// Top-level TokenInstruction tag followed by the confidential sub-instruction.
const CT_HEADER_LEN: usize = 2;
const TRANSFER_FIXED_ACCOUNTS: usize = 7;
const MAX_CT_TRANSFER_ACCOUNTS: usize = TRANSFER_FIXED_ACCOUNTS + MAX_HOOK_REMAINING_ACCOUNTS;
pub const MAX_PENDING_BALANCE_CREDIT_COUNTER: u64 = 65_536;

/// Adds the CT extension to an existing escrow. The payer covers added rent.
#[inline(always)]
pub fn reallocate_confidential_account_cpi(
    escrow: &AccountView,
    payer: &AccountView,
    system_program: &AccountView,
    authority: &AccountView,
    signers: &[Signer],
) -> ProgramResult {
    let mut data = [0; 1 + size_of::<u16>()];
    data[0] = TOKEN_INSTRUCTION_RESIZE_ACCOUNT_DATA;
    data[1..].copy_from_slice(&(ExtensionType::ConfidentialTransferAccount as u16).to_le_bytes());
    let metas = [
        InstructionAccount::writable(escrow.address()),
        InstructionAccount::writable_signer(payer.address()),
        InstructionAccount::readonly(system_program.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    invoke_signed(
        &InstructionView {
            program_id: &TOKEN_2022_PROGRAM_ID,
            accounts: &metas,
            data: &data,
        },
        &[escrow, payer, system_program, authority],
        signers,
    )
}

/// Configures the escrow using an inline PubkeyValidity proof addressed through
/// the instructions sysvar. Offset zero is reserved for context-state accounts.
#[inline(always)]
pub fn configure_confidential_account_cpi(
    escrow: &AccountView,
    mint: &AccountView,
    instructions_sysvar: &AccountView,
    authority: &AccountView,
    decryptable_zero: &[u8; AE_CIPHERTEXT_LEN],
    proof_offset: i8,
    signers: &[Signer],
) -> ProgramResult {
    require!(proof_offset != 0, ProgramError::InvalidInstructionData);
    let payload = ConfigureAccountInstructionData {
        decryptable_zero_balance: (*decryptable_zero).into(),
        maximum_pending_balance_credit_counter: MAX_PENDING_BALANCE_CREDIT_COUNTER.into(),
        proof_instruction_offset: proof_offset,
    };
    let mut data = [0; CT_HEADER_LEN + size_of::<ConfigureAccountInstructionData>()];
    data[..CT_HEADER_LEN].copy_from_slice(&[
        TOKEN_INSTRUCTION_CONFIDENTIAL_TRANSFER_EXTENSION,
        ConfidentialTransferInstruction::ConfigureAccount as u8,
    ]);
    data[CT_HEADER_LEN..].copy_from_slice(bytemuck::bytes_of(&payload));
    let metas = [
        InstructionAccount::writable(escrow.address()),
        InstructionAccount::readonly(mint.address()),
        InstructionAccount::readonly(instructions_sysvar.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    invoke_signed(
        &InstructionView {
            program_id: &TOKEN_2022_PROGRAM_ID,
            accounts: &metas,
            data: &data,
        },
        &[escrow, mint, instructions_sysvar, authority],
        signers,
    )
}

#[inline(always)]
pub fn disable_non_confidential_credits_cpi(
    escrow: &AccountView,
    authority: &AccountView,
    signers: &[Signer],
) -> ProgramResult {
    let metas = [
        InstructionAccount::writable(escrow.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    let data = [
        TOKEN_INSTRUCTION_CONFIDENTIAL_TRANSFER_EXTENSION,
        ConfidentialTransferInstruction::DisableNonConfidentialCredits as u8,
    ];
    invoke_signed(
        &InstructionView {
            program_id: &TOKEN_2022_PROGRAM_ID,
            accounts: &metas,
            data: &data,
        },
        &[escrow, authority],
        signers,
    )
}

#[inline(always)]
pub fn apply_pending_balance_cpi(
    escrow: &AccountView,
    authority: &AccountView,
    expected_counter: u64,
    new_decryptable: &[u8; AE_CIPHERTEXT_LEN],
    signers: &[Signer],
) -> ProgramResult {
    let payload = ApplyPendingBalanceData {
        expected_pending_balance_credit_counter: expected_counter.into(),
        new_decryptable_available_balance: (*new_decryptable).into(),
    };
    let mut data = [0; CT_HEADER_LEN + size_of::<ApplyPendingBalanceData>()];
    data[..CT_HEADER_LEN].copy_from_slice(&[
        TOKEN_INSTRUCTION_CONFIDENTIAL_TRANSFER_EXTENSION,
        ConfidentialTransferInstruction::ApplyPendingBalance as u8,
    ]);
    data[CT_HEADER_LEN..].copy_from_slice(bytemuck::bytes_of(&payload));
    let metas = [
        InstructionAccount::writable(escrow.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    invoke_signed(
        &InstructionView {
            program_id: &TOKEN_2022_PROGRAM_ID,
            accounts: &metas,
            data: &data,
        },
        &[escrow, authority],
        signers,
    )
}

/// CT Transfer with three context-state accounts (all proof offsets are zero).
/// Checks transfer contexts and recipient before the optional memo CPI. Callers
/// still check the operation's other contexts and amount binding beforehand.
/// Hook extras preserve writable flags but never receive signer privileges.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn confidential_transfer_cpi(
    from: &AccountView,
    mint: &AccountView,
    to: &AccountView,
    authority: &AccountView,
    equality: &AccountView,
    validity: &AccountView,
    range: &AccountView,
    proof_authority: &Address,
    transfer: &CtTransferData,
    memo_program: &AccountView,
    remaining: &[AccountView],
    signers: &[Signer],
) -> ProgramResult {
    require!(
        remaining.len() <= MAX_HOOK_REMAINING_ACCOUNTS,
        ProgramError::InvalidArgument
    );
    check_transfer_proof_contexts(equality, validity, range, proof_authority)?;
    check_confidential_recipient(to)?;
    if requires_memo(to)? {
        invoke_memo(memo_program)?;
    }
    let total = TRANSFER_FIXED_ACCOUNTS + remaining.len();
    // Only the first `total` entries are passed to the CPI.
    let mut metas: [InstructionAccount; MAX_CT_TRANSFER_ACCOUNTS] =
        core::array::from_fn(|_| InstructionAccount::readonly(from.address()));
    let mut infos = [from; MAX_CT_TRANSFER_ACCOUNTS];
    metas[..TRANSFER_FIXED_ACCOUNTS].clone_from_slice(&[
        InstructionAccount::writable(from.address()),
        InstructionAccount::readonly(mint.address()),
        InstructionAccount::writable(to.address()),
        InstructionAccount::readonly(equality.address()),
        InstructionAccount::readonly(validity.address()),
        InstructionAccount::readonly(range.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ]);
    infos[..TRANSFER_FIXED_ACCOUNTS]
        .copy_from_slice(&[from, mint, to, equality, validity, range, authority]);
    for (i, info) in remaining.iter().enumerate() {
        let index = TRANSFER_FIXED_ACCOUNTS + i;
        metas[index] = InstructionAccount::new(info.address(), info.is_writable(), false);
        infos[index] = info;
    }

    let payload = TransferInstructionData {
        new_source_decryptable_available_balance: transfer
            .new_source_decryptable_available_balance
            .into(),
        transfer_amount_auditor_ciphertext_lo: transfer.auditor_ciphertext_lo.into(),
        transfer_amount_auditor_ciphertext_hi: transfer.auditor_ciphertext_hi.into(),
        equality_proof_instruction_offset: 0,
        ciphertext_validity_proof_instruction_offset: 0,
        range_proof_instruction_offset: 0,
    };
    let mut data = [0; CT_HEADER_LEN + size_of::<TransferInstructionData>()];
    data[..CT_HEADER_LEN].copy_from_slice(&[
        TOKEN_INSTRUCTION_CONFIDENTIAL_TRANSFER_EXTENSION,
        ConfidentialTransferInstruction::Transfer as u8,
    ]);
    data[CT_HEADER_LEN..].copy_from_slice(bytemuck::bytes_of(&payload));
    invoke_signed_with_bounds::<MAX_CT_TRANSFER_ACCOUNTS>(
        &InstructionView {
            program_id: &TOKEN_2022_PROGRAM_ID,
            accounts: &metas[..total],
            data: &data,
        },
        &infos[..total],
        signers,
    )
}

/// Checks the post-transfer available balance against the zero proof, then
/// resets it only if both pending ciphertexts are all-zero bytes. Returns
/// whether EmptyAccount ran; public token balance must also be zero to close.
// Keep the decoded CT state and CPI buffers in a separate SBF frame.
#[inline(never)]
pub fn empty_confidential_account_if_no_pending(
    escrow: &AccountView,
    zero: &AccountView,
    authority: &AccountView,
    proof_authority: &Address,
    signers: &[Signer],
) -> Result<bool, ProgramError> {
    let state = check_confidential_zero(escrow, zero, proof_authority)?;
    if state.pending_balance_lo != Default::default()
        || state.pending_balance_hi != Default::default()
    {
        return Ok(false);
    }
    let metas = [
        InstructionAccount::writable(escrow.address()),
        InstructionAccount::readonly(zero.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    let payload = EmptyAccountInstructionData {
        proof_instruction_offset: 0,
    };
    let mut data = [0; CT_HEADER_LEN + size_of::<EmptyAccountInstructionData>()];
    data[..CT_HEADER_LEN].copy_from_slice(&[
        TOKEN_INSTRUCTION_CONFIDENTIAL_TRANSFER_EXTENSION,
        ConfidentialTransferInstruction::EmptyAccount as u8,
    ]);
    data[CT_HEADER_LEN..].copy_from_slice(bytemuck::bytes_of(&payload));
    invoke_signed(
        &InstructionView {
            program_id: &TOKEN_2022_PROGRAM_ID,
            accounts: &metas,
            data: &data,
        },
        &[escrow, zero, authority],
        signers,
    )?;
    Ok(true)
}

/// Closes a verified context with the outer signer, returning rent to that
/// same signer. The token-authority PDA does not sign this CPI.
#[inline(always)]
pub fn close_proof_context_cpi(
    context: &AccountView,
    kind: ProofType,
    signer: &AccountView,
) -> ProgramResult {
    require!(signer.is_signer(), ProgramError::MissingRequiredSignature);
    require!(
        signer.is_writable() && context.is_writable(),
        ProgramError::InvalidAccountData
    );
    check_proof_context(context, kind, signer.address())?;
    let metas = [
        InstructionAccount::writable(context.address()),
        InstructionAccount::writable(signer.address()),
        InstructionAccount::readonly_signer(signer.address()),
    ];
    invoke(
        &InstructionView {
            program_id: &ZK_ELGAMAL_PROOF_PROGRAM_ID,
            accounts: &metas,
            data: &[ProofInstruction::CloseContextState as u8],
        },
        &[context, signer, signer],
    )
}
