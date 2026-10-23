//! Confidential refunds shared by Reclaim, Cancel, Reject and Recover.

use pinocchio::{account::AccountView, cpi::Signer, error::ProgramError, Address, ProgramResult};
use pinocchio_token_2022::{instructions::CloseAccount, ID as TOKEN_2022_PROGRAM_ID};

use super::{
    check_confidential_recipient, check_proof_context, check_refund_contexts,
    check_transfer_proof_contexts, close_proof_context_cpi, confidential_transfer_cpi,
    empty_confidential_account_if_no_pending, read_confidential_account, LegBRefund, ProofType,
    ZK_ELGAMAL_PROOF_PROGRAM_ID,
};
use crate::{
    error::DvpSwapProgramError,
    processor::shared::token_utils::{
        get_mint_decimals, get_token_account_balance, transfer_checked_cpi,
        verify_ata_recipient_if_initialized, verify_canonical_ata,
    },
    require,
    state::swap_dvp::SwapDvp,
};

/// Validates every refund context before any CPI, including leg A's SyncNative.
#[inline(always)]
pub fn check_confidential_refund(
    program_id: &Address,
    refund: &LegBRefund,
    escrow: &AccountView,
    destination: &AccountView,
    signer: &AccountView,
    zk_program: &AccountView,
    contexts: &[AccountView],
) -> ProgramResult {
    require!(
        zk_program.address() == &ZK_ELGAMAL_PROOF_PROGRAM_ID,
        ProgramError::IncorrectProgramId
    );
    let [equality, validity, range, zero] = contexts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    check_refund_contexts(program_id, refund, equality, validity, range, zero)?;
    match refund {
        LegBRefund::None => {
            require!(
                read_confidential_account(escrow)?.available_balance == Default::default(),
                DvpSwapProgramError::LegBRefundRequired
            );
        }
        LegBRefund::Full(_) | LegBRefund::Partial(_) => {
            check_transfer_proof_contexts(equality, validity, range, signer.address())?;
            if matches!(refund, LegBRefund::Full(_)) {
                check_proof_context(zero, ProofType::ZeroCiphertext, signer.address())?;
            }
            check_confidential_recipient(destination)?;
        }
    }
    Ok(())
}

/// Executes the selected CT refund and returns whether the CT balances allow
/// closure. None leaves public tokens alone; Reclaim and Recover drain those.
/// Call check_confidential_refund before the operation's first CPI.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn refund_confidential_leg(
    refund: &LegBRefund,
    escrow: &AccountView,
    mint: &AccountView,
    destination: &AccountView,
    authority: &AccountView,
    signer: &AccountView,
    memo: &AccountView,
    contexts: &[AccountView],
    remaining: &[AccountView],
    signers: &[Signer],
) -> Result<bool, ProgramError> {
    let [equality, validity, range, zero] = contexts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let transfer = match refund {
        LegBRefund::None => {
            let state = read_confidential_account(escrow)?;
            require!(
                state.available_balance == Default::default(),
                DvpSwapProgramError::LegBRefundRequired
            );
            return Ok(state.pending_balance_lo == Default::default()
                && state.pending_balance_hi == Default::default());
        }
        LegBRefund::Full(transfer) | LegBRefund::Partial(transfer) => transfer,
    };
    confidential_transfer_cpi(
        escrow,
        mint,
        destination,
        authority,
        equality,
        validity,
        range,
        signer.address(),
        transfer,
        memo,
        remaining,
        signers,
    )?;
    // Full must prove the post-transfer available balance is zero. Pending
    // credits never block the refund; they only prevent reset and closure.
    let reset = if matches!(refund, LegBRefund::Full(_)) {
        empty_confidential_account_if_no_pending(
            escrow,
            zero,
            authority,
            signer.address(),
            signers,
        )?
    } else {
        false
    };
    for (context, kind) in [
        (equality, ProofType::CiphertextCommitmentEquality),
        (
            validity,
            ProofType::BatchedGroupedCiphertext3HandlesValidity,
        ),
        (range, ProofType::BatchedRangeProofU128),
    ] {
        close_proof_context_cpi(context, kind, signer)?;
    }
    if matches!(refund, LegBRefund::Full(_)) {
        close_proof_context_cpi(zero, ProofType::ZeroCiphertext, signer)?;
    }
    Ok(reset)
}

/// Cancel and Reject share the refund path; callers authorize their signer.
/// As in public refunds, do not revalidate mint extensions or authorities.
#[inline(never)]
pub fn refund_and_close_confidential_dvp(
    program_id: &Address,
    fixed: &[AccountView],
    dvp: &SwapDvp,
    refund: &LegBRefund,
    leg_a_extras: &[AccountView],
    leg_b_extras: &[AccountView],
) -> ProgramResult {
    let [signer, swap, mint_a, mint_b, escrow_a, escrow_b, destination_a, destination_b, token_a, token_b, memo, zk_program, contexts @ ..] =
        fixed
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require!(
        mint_a.address() == &dvp.mint_a && mint_b.address() == &dvp.mint_b,
        ProgramError::InvalidAccountData
    );
    require!(
        token_a.address() == &dvp.token_program_a
            && token_b.address() == &dvp.token_program_b
            && token_b.address() == &TOKEN_2022_PROGRAM_ID,
        ProgramError::IncorrectProgramId
    );
    verify_canonical_ata(escrow_a, swap.address(), &dvp.mint_a, token_a)?;
    verify_canonical_ata(escrow_b, swap.address(), &dvp.mint_b, token_b)?;
    verify_canonical_ata(destination_a, &dvp.user_a, &dvp.mint_a, token_a)?;
    verify_canonical_ata(destination_b, &dvp.user_b, &dvp.mint_b, token_b)?;
    verify_ata_recipient_if_initialized(destination_a, &dvp.user_a, &dvp.mint_a)?;
    verify_ata_recipient_if_initialized(destination_b, &dvp.user_b, &dvp.mint_b)?;
    check_confidential_refund(
        program_id,
        refund,
        escrow_b,
        destination_b,
        signer,
        zk_program,
        contexts,
    )?;

    let (nonce_bytes, bump_bytes) = dvp.seed_buffers();
    let seeds = dvp.signing_seeds(&nonce_bytes, &bump_bytes);
    let signers = [Signer::from(&seeds)];
    let amount_a = get_token_account_balance(escrow_a)?;
    if amount_a > 0 {
        transfer_checked_cpi(
            escrow_a,
            mint_a,
            destination_a,
            swap,
            amount_a,
            get_mint_decimals(mint_a)?,
            token_a.address(),
            memo,
            leg_a_extras,
            &signers,
        )?;
    }
    let reset_b = refund_confidential_leg(
        refund,
        escrow_b,
        mint_b,
        destination_b,
        swap,
        signer,
        memo,
        contexts,
        leg_b_extras,
        &signers,
    )?;
    CloseAccount {
        account: escrow_a,
        destination: signer,
        authority: swap,
        token_program: token_a.address(),
    }
    .invoke_signed(&signers)?;
    // MintTo can add a public balance despite DisableNonConfidentialCredits.
    // Only user_b may withdraw it later through Reclaim or Recover with None.
    if reset_b && get_token_account_balance(escrow_b)? == 0 {
        CloseAccount {
            account: escrow_b,
            destination: signer,
            authority: swap,
            token_program: token_b.address(),
        }
        .invoke_signed(&signers)?;
    }
    signer.set_lamports(
        signer
            .lamports()
            .checked_add(swap.lamports())
            .ok_or(ProgramError::ArithmeticOverflow)?,
    );
    swap.set_lamports(0);
    swap.close()
}
