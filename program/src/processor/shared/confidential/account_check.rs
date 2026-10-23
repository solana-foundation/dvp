//! Shared account and mode checks for confidential instructions.

use pinocchio::{account::AccountView, error::ProgramError, Address, ProgramResult};

use super::{has_confidential_transfer_account, LegBRefund};

use crate::error::DvpSwapProgramError;

#[inline(always)]
fn check_optional(info: &AccountView, present: bool, program_id: &Address) -> ProgramResult {
    if present == (info.address() == program_id) {
        return Err(ProgramError::InvalidInstructionData);
    }
    if present && !info.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

#[inline(always)]
pub fn check_transfer_contexts(
    program_id: &Address,
    present: bool,
    equality: &AccountView,
    validity: &AccountView,
    range: &AccountView,
) -> ProgramResult {
    for info in [equality, validity, range] {
        check_optional(info, present, program_id)?;
    }
    Ok(())
}

#[inline(always)]
pub fn check_refund_contexts(
    program_id: &Address,
    refund: &LegBRefund,
    equality: &AccountView,
    validity: &AccountView,
    range: &AccountView,
    zero: &AccountView,
) -> ProgramResult {
    // Full and Partial need transfer contexts; only Full needs a zero context.
    check_transfer_contexts(
        program_id,
        !matches!(refund, LegBRefund::None),
        equality,
        validity,
        range,
    )?;
    check_optional(zero, matches!(refund, LegBRefund::Full(_)), program_id)
}

#[inline(always)]
pub fn check_confidential_escrow(escrow: &AccountView) -> ProgramResult {
    if !has_confidential_transfer_account(escrow)? {
        return Err(DvpSwapProgramError::EscrowNotConfidential.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use crate::processor::shared::confidential::{CtTransferData, AE_CIPHERTEXT_LEN};
    use crate::processor::{
        process_apply_confidential_dvp, process_cancel_confidential_dvp,
        process_reclaim_confidential_dvp, process_recover_confidential_dvp,
        process_reject_confidential_dvp,
    };

    type Processor = fn(&Address, &[AccountView], &[u8]) -> ProgramResult;

    #[test]
    fn refund_and_apply_wire_formats() {
        let cases: [(Processor, usize); 4] = [
            (process_reclaim_confidential_dvp, 0), // only leg_b_refund
            (process_cancel_confidential_dvp, 1),  // leg_a_extras_count
            (process_reject_confidential_dvp, 1),  // leg_a_extras_count
            (process_recover_confidential_dvp, 32 * 5 + 8), // five seed pubkeys + nonce
        ];
        for (process, prefix_len) in cases {
            for tag in 0..=2 {
                let mut data = alloc::vec![0; prefix_len];
                data.push(tag);
                if tag != 0 {
                    data.extend_from_slice(&[0; CtTransferData::LEN]);
                }
                // Valid data reaches the account-count guard; malformed data fails first.
                assert_eq!(
                    process(&crate::ID, &[], &data),
                    Err(ProgramError::NotEnoughAccountKeys)
                );
                for len in 0..data.len() {
                    assert_eq!(
                        process(&crate::ID, &[], &data[..len]),
                        Err(ProgramError::InvalidInstructionData)
                    );
                }
                data.push(0);
                assert_eq!(
                    process(&crate::ID, &[], &data),
                    Err(ProgramError::InvalidInstructionData)
                );
            }
            let mut invalid = alloc::vec![0; prefix_len];
            invalid.push(3);
            assert_eq!(
                process(&crate::ID, &[], &invalid),
                Err(ProgramError::InvalidInstructionData)
            );
        }

        // pending counter + decryptable balance + five seed pubkeys + nonce.
        const APPLY_DATA_LEN: usize = 8 + AE_CIPHERTEXT_LEN + 32 * 5 + 8;
        let process = |data: &[u8]| process_apply_confidential_dvp(&crate::ID, &[], data);
        assert_eq!(
            process(&[0; APPLY_DATA_LEN]),
            Err(ProgramError::NotEnoughAccountKeys)
        );
        for len in 0..APPLY_DATA_LEN {
            assert_eq!(
                process(&alloc::vec![0; len]),
                Err(ProgramError::InvalidInstructionData)
            );
        }
        assert_eq!(
            process(&[0; APPLY_DATA_LEN + 1]),
            Err(ProgramError::InvalidInstructionData)
        );
    }
}
