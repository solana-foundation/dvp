use crate::{
    error::DvpSwapProgramError,
    processor::shared::{
        account_check::{verify_account_owner, verify_signer},
        confidential::{
            check_confidential_refund, check_refund_contexts, refund_confidential_leg, LegBRefund,
            ZK_ELGAMAL_PROOF_PROGRAM_ID,
        },
        token_utils::{
            get_mint_decimals, get_token_account_balance, transfer_checked_cpi,
            verify_ata_recipient_if_initialized, verify_canonical_ata, MAX_HOOK_REMAINING_ACCOUNTS,
        },
    },
    require,
    state::swap_dvp::ConfidentialSwapDvp,
};
use pinocchio::{account::AccountView, cpi::Signer, error::ProgramError, Address, ProgramResult};
use pinocchio_token_2022::ID as TOKEN_2022_PROGRAM_ID;

const FIXED_ACCOUNTS_LEN: usize = 12;

/// Processes the ReclaimConfidentialDvp instruction.
///
/// Confidential counterpart of [`process_reclaim_dvp`](super::reclaim_dvp::process_reclaim_dvp):
/// a depositor takes back its own leg while the swap and escrows stay open.
/// Leg A uses the public transfer path. Leg B supports full or partial
/// confidential refunds to `user_b`, or withdrawal of its public balance.
///
/// # Account Layout
/// Accounts 0-6 follow the public Reclaim layout, except the signer must also
/// be writable to receive closed proof-context rent. Account 7 is the ZK
/// ElGamal Proof program; accounts 8-11 are the equality, validity, range and
/// zero proof contexts. Present contexts are writable; absent contexts use
/// the DvP program id as a placeholder. Transfer-hook extras follow this prefix.
///
/// # Instruction Data
/// [`LegBRefund`] selects the leg B refund: `None` has no transfer payload or
/// contexts; `Full` carries transfer data and all four contexts; `Partial`
/// carries transfer data and the first three contexts, leaving a remainder.
/// `user_a` supplies `None`. For `user_b`, `None` withdraws the public balance
/// only when the confidential available balance has been reset.
///
pub fn process_reclaim_confidential_dvp(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = parse_instruction_data(instruction_data)?;
    require!(
        accounts.len() >= FIXED_ACCOUNTS_LEN,
        ProgramError::NotEnoughAccountKeys
    );
    verify_account_owner(&accounts[1], program_id)?;
    let confidential = ConfidentialSwapDvp::load(&accounts[1].try_borrow()?)?;
    reclaim(program_id, accounts, &args.leg_b_refund, &confidential)
}

// Keep decoded state and transfer arguments out of the CPI execution frame.
#[inline(never)]
fn reclaim(
    program_id: &Address,
    accounts: &[AccountView],
    refund: &LegBRefund,
    confidential: &ConfidentialSwapDvp,
) -> ProgramResult {
    let (fixed, remaining) = accounts.split_at(FIXED_ACCOUNTS_LEN);
    let [signer, swap, mint, escrow, destination, token, memo, zk_program, equality, validity, range, zero] =
        fixed
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require!(
        remaining.len() <= MAX_HOOK_REMAINING_ACCOUNTS,
        ProgramError::InvalidArgument
    );
    verify_signer(signer, true)?;
    let dvp = &confidential.base;
    let is_leg_a = signer.address() == &dvp.user_a;
    let (leg_mint, leg_token) = if is_leg_a {
        require!(
            matches!(refund, LegBRefund::None),
            ProgramError::InvalidInstructionData
        );
        (&dvp.mint_a, &dvp.token_program_a)
    } else {
        require!(
            signer.address() == &dvp.user_b,
            DvpSwapProgramError::SignerNotParty
        );
        require!(
            token.address() == &TOKEN_2022_PROGRAM_ID,
            ProgramError::IncorrectProgramId
        );
        (&dvp.mint_b, &dvp.token_program_b)
    };
    require!(mint.address() == leg_mint, ProgramError::InvalidAccountData);
    require!(
        token.address() == leg_token,
        ProgramError::IncorrectProgramId
    );
    verify_canonical_ata(escrow, swap.address(), leg_mint, token)?;
    verify_canonical_ata(destination, signer.address(), leg_mint, token)?;
    verify_ata_recipient_if_initialized(destination, signer.address(), leg_mint)?;
    let contexts = &fixed[8..];
    if is_leg_a {
        require!(
            zk_program.address() == &ZK_ELGAMAL_PROOF_PROGRAM_ID,
            ProgramError::IncorrectProgramId
        );
        check_refund_contexts(program_id, refund, equality, validity, range, zero)?;
    } else {
        check_confidential_refund(
            program_id,
            refund,
            escrow,
            destination,
            signer,
            zk_program,
            contexts,
        )?;
    }
    let (nonce_bytes, bump_bytes) = dvp.seed_buffers();
    let seeds = dvp.signing_seeds(&nonce_bytes, &bump_bytes);
    let signers = [Signer::from(&seeds)];
    // None on leg B requires a reset available balance, but pending credits
    // may remain. Only the public balance is withdrawn in this mode.
    if matches!(refund, LegBRefund::None) {
        let amount = get_token_account_balance(escrow)?;
        if amount > 0 {
            transfer_checked_cpi(
                escrow,
                mint,
                destination,
                swap,
                amount,
                get_mint_decimals(mint)?,
                token.address(),
                memo,
                remaining,
                &signers,
            )?;
        }
    } else {
        refund_confidential_leg(
            refund,
            escrow,
            mint,
            destination,
            swap,
            signer,
            memo,
            contexts,
            remaining,
            &signers,
        )?;
    }
    // Reclaim leaves the swap and both escrows open for funding again.
    Ok(())
}

#[derive(Debug, PartialEq)]
struct ReclaimConfidentialDvpArgs {
    leg_b_refund: LegBRefund,
}

fn parse_instruction_data(data: &[u8]) -> Result<ReclaimConfidentialDvpArgs, ProgramError> {
    let leg_b_refund = LegBRefund::try_from(data)?;
    Ok(ReclaimConfidentialDvpArgs { leg_b_refund })
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;

    #[test]
    fn parse_instruction_data_decodes_fields() {
        let transfer_bytes = [
            &[6; 36][..], // new_source_decryptable_available_balance
            &[4; 64],     // auditor_ciphertext_lo
            &[5; 64],     // auditor_ciphertext_hi
        ]
        .concat();
        let transfer = crate::processor::shared::confidential::CtTransferData {
            new_source_decryptable_available_balance: [6; 36],
            auditor_ciphertext_lo: [4; 64],
            auditor_ciphertext_hi: [5; 64],
        };
        for (tag, leg_b_refund) in [
            (0, LegBRefund::None),
            (1, LegBRefund::Full(transfer.clone())),
            (2, LegBRefund::Partial(transfer)),
        ] {
            let mut data = alloc::vec![tag];
            if tag != 0 {
                data.extend_from_slice(&transfer_bytes);
            }
            assert_eq!(
                parse_instruction_data(&data).unwrap(),
                ReclaimConfidentialDvpArgs { leg_b_refund }
            );
        }
    }
}
