use crate::{
    error::DvpSwapProgramError,
    processor::shared::{
        account_check::{verify_account_owner, verify_signer},
        confidential::{
            check_confidential_amount, check_confidential_recipient, check_proof_context,
            check_transfer_contexts, close_proof_context_cpi, confidential_transfer_cpi,
            empty_confidential_account_if_no_pending, verify_confidential_mint, CtTransferData,
            ProofType, ZK_ELGAMAL_PROOF_PROGRAM_ID,
        },
        token_utils::{
            get_mint_authority, get_mint_decimals, get_token_account_balance, transfer_checked_cpi,
            validate_mint_extensions, verify_ata_recipient, verify_ata_recipient_if_initialized,
            verify_canonical_ata,
        },
        utils::split_leg_remaining_accounts,
    },
    require, require_len,
    state::swap_dvp::ConfidentialSwapDvp,
};
use pinocchio::{
    account::AccountView,
    cpi::Signer,
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    Address, ProgramResult,
};
use pinocchio_token_2022::{instructions::CloseAccount, ID as TOKEN_2022_PROGRAM_ID};

const FIXED_ACCOUNTS_LEN: usize = 23;

/// Processes the SettleConfidentialDvp instruction.
///
/// Confidential counterpart of [`process_settle_dvp`](super::settle_dvp::process_settle_dvp):
/// the settlement authority delivers both legs atomically and refunds surplus
/// to each depositor. Leg B payment proofs bind the transferred amount to the
/// ciphertexts stored at Create. The swap and leg A escrow close; leg B escrow
/// stays open if pending or public balances remain, for Apply and Recover.
/// Pending credits are not applied during settlement.
///
/// # Account Layout
/// Accounts 0-12 follow the public Settle layout, with Token-2022 for leg B.
/// Account 13 is the ZK ElGamal Proof program. Writable proof contexts follow:
/// 14-16 are payment equality, validity and range; 17-18 bind the low and high
/// payment limbs to the agreed amount; 19 proves the remaining available
/// balance is zero; 20-22 are surplus equality, validity and range.
/// The surplus contexts use DvP program-id placeholders when `surplus_b` is
/// absent. Transfer-hook extras follow the fixed prefix and are split by
/// `leg_a_extras_count`, as in public Settle.
///
/// # Instruction Data
/// `leg_a_extras_count` (u8), `payment` ([`CtTransferData`]) and `surplus_b`
/// (`Option<CtTransferData>`), in that order. Payment goes to the configured
/// leg B settlement destination; any leg B surplus goes to `user_b`'s own ATA.
pub fn process_settle_confidential_dvp(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = parse_instruction_data(instruction_data)?;
    require!(
        accounts.len() >= FIXED_ACCOUNTS_LEN,
        ProgramError::NotEnoughAccountKeys
    );
    let swap_dvp_info = &accounts[1];
    verify_account_owner(swap_dvp_info, program_id)?;
    let confidential = ConfidentialSwapDvp::load(&swap_dvp_info.try_borrow()?)?;
    settle(program_id, accounts, &args, &confidential)
}

// Keep decoded state and arguments in the caller's SBF frame so validation
// and CPI temporaries fit within the 4096-byte limit of this frame.
#[inline(never)]
fn settle(
    program_id: &Address,
    accounts: &[AccountView],
    args: &SettleConfidentialDvpArgs,
    confidential: &ConfidentialSwapDvp,
) -> ProgramResult {
    let (fixed, leg_a_extras, leg_b_extras) =
        split_leg_remaining_accounts(accounts, &[args.leg_a_extras_count], FIXED_ACCOUNTS_LEN)?;
    let [settlement_authority_info, swap_dvp_info, mint_a_info, mint_b_info, dvp_ata_a_info, dvp_ata_b_info, user_a_destination_ata_b_info, user_b_destination_ata_a_info, user_a_ata_a_info, user_b_ata_b_info, token_program_a_info, token_program_b_info, memo_program_info, zk_elgamal_proof_program_info, payment_equality_context_info, payment_validity_context_info, payment_range_context_info, eq_lo_context_info, eq_hi_context_info, zero_context_info, surplus_equality_context_info, surplus_validity_context_info, surplus_range_context_info] =
        fixed
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    check_transfer_contexts(
        program_id,
        args.surplus.is_some(),
        surplus_equality_context_info,
        surplus_validity_context_info,
        surplus_range_context_info,
    )?;
    verify_signer(settlement_authority_info, true)?;
    let dvp = &confidential.base;
    require!(
        settlement_authority_info.address() == &dvp.settlement_authority,
        DvpSwapProgramError::SettlementAuthorityMismatch
    );

    // Revalidate the mints and authorities agreed at Create, as in public Settle.
    require!(
        mint_a_info.address() == &dvp.mint_a && mint_b_info.address() == &dvp.mint_b,
        ProgramError::InvalidAccountData
    );
    require!(
        token_program_a_info.address() == &dvp.token_program_a
            && token_program_b_info.address() == &dvp.token_program_b
            && token_program_b_info.address() == &TOKEN_2022_PROGRAM_ID
            && zk_elgamal_proof_program_info.address() == &ZK_ELGAMAL_PROOF_PROGRAM_ID,
        ProgramError::IncorrectProgramId
    );
    require!(
        mint_a_info.owned_by(&dvp.token_program_a) && mint_b_info.owned_by(&dvp.token_program_b),
        ProgramError::InvalidAccountOwner
    );
    validate_mint_extensions(mint_a_info)?;
    validate_mint_extensions(mint_b_info)?;
    verify_confidential_mint(mint_b_info)?;
    require!(
        get_mint_authority(mint_a_info)?.unwrap_or_default() == dvp.mint_a_authority
            && get_mint_authority(mint_b_info)?.unwrap_or_default() == dvp.mint_b_authority,
        DvpSwapProgramError::MintAuthorityChanged
    );
    let now = Clock::get()?.unix_timestamp;
    require!(now <= dvp.expiry_timestamp, DvpSwapProgramError::DvpExpired);
    if let Some(earliest) = dvp.earliest_settlement_timestamp {
        require!(now >= earliest, DvpSwapProgramError::SettlementTooEarly);
    }

    // Deliveries go to the agreed destinations; surplus goes to the depositors.
    verify_canonical_ata(
        dvp_ata_a_info,
        swap_dvp_info.address(),
        &dvp.mint_a,
        token_program_a_info,
    )?;
    verify_canonical_ata(
        dvp_ata_b_info,
        swap_dvp_info.address(),
        &dvp.mint_b,
        token_program_b_info,
    )?;
    verify_canonical_ata(
        user_a_destination_ata_b_info,
        &dvp.user_a_settlement_destination,
        &dvp.mint_b,
        token_program_b_info,
    )?;
    verify_ata_recipient(
        user_a_destination_ata_b_info,
        &dvp.user_a_settlement_destination,
        &dvp.mint_b,
    )?;
    verify_canonical_ata(
        user_b_destination_ata_a_info,
        &dvp.user_b_settlement_destination,
        &dvp.mint_a,
        token_program_a_info,
    )?;
    verify_ata_recipient(
        user_b_destination_ata_a_info,
        &dvp.user_b_settlement_destination,
        &dvp.mint_a,
    )?;
    verify_canonical_ata(
        user_a_ata_a_info,
        &dvp.user_a,
        &dvp.mint_a,
        token_program_a_info,
    )?;
    verify_ata_recipient_if_initialized(user_a_ata_a_info, &dvp.user_a, &dvp.mint_a)?;
    verify_canonical_ata(
        user_b_ata_b_info,
        &dvp.user_b,
        &dvp.mint_b,
        token_program_b_info,
    )?;
    verify_ata_recipient_if_initialized(user_b_ata_b_info, &dvp.user_b, &dvp.mint_b)?;

    check_confidential_recipient(user_a_destination_ata_b_info)?;
    if args.surplus.is_some() {
        check_confidential_recipient(user_b_ata_b_info)?;
    }

    // Validate every context, including surplus and zero, before the first CPI.
    // Only real contexts are included; absent surplus placeholders are excluded.
    let required_contexts = [
        (
            payment_equality_context_info,
            ProofType::CiphertextCommitmentEquality,
        ),
        (
            payment_validity_context_info,
            ProofType::BatchedGroupedCiphertext3HandlesValidity,
        ),
        (payment_range_context_info, ProofType::BatchedRangeProofU128),
        (eq_lo_context_info, ProofType::CiphertextCiphertextEquality),
        (eq_hi_context_info, ProofType::CiphertextCiphertextEquality),
        (zero_context_info, ProofType::ZeroCiphertext),
    ];
    let surplus_contexts = [
        (
            surplus_equality_context_info,
            ProofType::CiphertextCommitmentEquality,
        ),
        (
            surplus_validity_context_info,
            ProofType::BatchedGroupedCiphertext3HandlesValidity,
        ),
        (surplus_range_context_info, ProofType::BatchedRangeProofU128),
    ];
    let contexts = required_contexts
        .iter()
        .chain(surplus_contexts.iter().filter(|_| args.surplus.is_some()));
    for &(context, kind) in contexts.clone() {
        require!(context.is_writable(), ProgramError::InvalidAccountData);
        check_proof_context(context, kind, settlement_authority_info.address())?;
    }
    check_confidential_amount(
        dvp_ata_b_info,
        payment_validity_context_info,
        eq_lo_context_info,
        eq_hi_context_info,
        &confidential.amount_b_ciphertext_lo,
        &confidential.amount_b_ciphertext_hi,
        settlement_authority_info.address(),
    )?;

    // Reading WSOL balance can issue SyncNative, so all proof checks precede it.
    // Leg B sufficiency is enforced by Token-2022's transfer range proof.
    let escrow_a_balance = get_token_account_balance(dvp_ata_a_info)?;
    require!(
        escrow_a_balance >= dvp.amount_a,
        DvpSwapProgramError::LegNotFunded
    );
    let decimals_a = get_mint_decimals(mint_a_info)?;

    let (nonce_bytes, bump_bytes) = dvp.seed_buffers();
    let swap_dvp_seeds = dvp.signing_seeds(&nonce_bytes, &bump_bytes);
    let signer_seeds = [Signer::from(&swap_dvp_seeds)];

    confidential_transfer_cpi(
        dvp_ata_b_info,
        mint_b_info,
        user_a_destination_ata_b_info,
        swap_dvp_info,
        payment_equality_context_info,
        payment_validity_context_info,
        payment_range_context_info,
        settlement_authority_info.address(),
        &args.payment,
        memo_program_info,
        leg_b_extras,
        &signer_seeds,
    )?;
    transfer_checked_cpi(
        dvp_ata_a_info,
        mint_a_info,
        user_b_destination_ata_a_info,
        swap_dvp_info,
        dvp.amount_a,
        decimals_a,
        token_program_a_info.address(),
        memo_program_info,
        leg_a_extras,
        &signer_seeds,
    )?;
    let surplus_a = escrow_a_balance - dvp.amount_a;
    if surplus_a > 0 {
        transfer_checked_cpi(
            dvp_ata_a_info,
            mint_a_info,
            user_a_ata_a_info,
            swap_dvp_info,
            surplus_a,
            decimals_a,
            token_program_a_info.address(),
            memo_program_info,
            leg_a_extras,
            &signer_seeds,
        )?;
    }
    if let Some(surplus) = &args.surplus {
        // As in public Settle, surplus reuses this leg's hook extras.
        confidential_transfer_cpi(
            dvp_ata_b_info,
            mint_b_info,
            user_b_ata_b_info,
            swap_dvp_info,
            surplus_equality_context_info,
            surplus_validity_context_info,
            surplus_range_context_info,
            settlement_authority_info.address(),
            surplus,
            memo_program_info,
            leg_b_extras,
            &signer_seeds,
        )?;
    }

    // Match the post-transfer available balance to zero. Pending credits must
    // not block settlement, so EmptyAccount runs only when pending is empty.
    let reset_b = empty_confidential_account_if_no_pending(
        dvp_ata_b_info,
        zero_context_info,
        swap_dvp_info,
        settlement_authority_info.address(),
        &signer_seeds,
    )?;
    for &(context, kind) in contexts {
        close_proof_context_cpi(context, kind, settlement_authority_info)?;
    }
    CloseAccount {
        account: dvp_ata_a_info,
        destination: settlement_authority_info,
        authority: swap_dvp_info,
        token_program: token_program_a_info.address(),
    }
    .invoke_signed(&signer_seeds)?;
    // MintTo can leave public tokens despite DisableNonConfidentialCredits.
    // Keep escrow B for user_b to recover after the swap has closed.
    if reset_b && get_token_account_balance(dvp_ata_b_info)? == 0 {
        CloseAccount {
            account: dvp_ata_b_info,
            destination: settlement_authority_info,
            authority: swap_dvp_info,
            token_program: token_program_b_info.address(),
        }
        .invoke_signed(&signer_seeds)?;
    }
    settlement_authority_info.set_lamports(
        settlement_authority_info
            .lamports()
            .checked_add(swap_dvp_info.lamports())
            .ok_or(ProgramError::ArithmeticOverflow)?,
    );
    swap_dvp_info.set_lamports(0);
    swap_dvp_info.close()?;
    Ok(())
}

#[derive(Debug, PartialEq)]
struct SettleConfidentialDvpArgs {
    leg_a_extras_count: u8,
    payment: CtTransferData,
    surplus: Option<CtTransferData>,
}

fn parse_instruction_data(data: &[u8]) -> Result<SettleConfidentialDvpArgs, ProgramError> {
    require_len!(data, 1);
    let mut offset = 0;

    let leg_a_extras_count = data[offset];
    offset += 1;
    require_len!(data, offset + CtTransferData::LEN + 1);
    let payment = CtTransferData::try_from(&data[offset..offset + CtTransferData::LEN])?;
    offset += CtTransferData::LEN;
    let surplus = match data[offset] {
        0 if data.len() == offset + 1 => None,
        1 => Some(CtTransferData::try_from(&data[offset + 1..])?),
        _ => return Err(ProgramError::InvalidInstructionData),
    };
    Ok(SettleConfidentialDvpArgs {
        leg_a_extras_count,
        payment,
        surplus,
    })
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;

    #[test]
    fn settle_surplus_option_is_strict() {
        for tag in 0..=1 {
            let mut data = alloc::vec![0; 1 + CtTransferData::LEN + 1 + usize::from(tag) * CtTransferData::LEN];
            data[1 + CtTransferData::LEN] = tag;
            assert_eq!(
                process_settle_confidential_dvp(&crate::ID, &[], &data),
                Err(ProgramError::NotEnoughAccountKeys)
            );
            for len in 0..data.len() {
                assert_eq!(
                    process_settle_confidential_dvp(&crate::ID, &[], &data[..len]),
                    Err(ProgramError::InvalidInstructionData)
                );
            }
            data.push(0);
            assert_eq!(
                process_settle_confidential_dvp(&crate::ID, &[], &data),
                Err(ProgramError::InvalidInstructionData)
            );
        }
        let mut data = [0; 1 + CtTransferData::LEN + 1];
        data[1 + CtTransferData::LEN] = 2;
        assert_eq!(
            process_settle_confidential_dvp(&crate::ID, &[], &data),
            Err(ProgramError::InvalidInstructionData)
        );
    }

    #[test]
    fn parse_instruction_data_decodes_fields() {
        let payment = CtTransferData {
            new_source_decryptable_available_balance: [6; 36],
            auditor_ciphertext_lo: [4; 64],
            auditor_ciphertext_hi: [5; 64],
        };
        let surplus = CtTransferData {
            new_source_decryptable_available_balance: [9; 36],
            auditor_ciphertext_lo: [7; 64],
            auditor_ciphertext_hi: [8; 64],
        };
        for surplus_present in [false, true] {
            let mut data = [
                &[3][..],     // leg_a_extras_count
                &[6; 36][..], // new_source_decryptable_available_balance
                &[4; 64],     // auditor_ciphertext_lo
                &[5; 64],     // auditor_ciphertext_hi
            ]
            .concat();
            data.push(u8::from(surplus_present));
            if surplus_present {
                data.extend_from_slice(
                    &[
                        &[9; 36][..], // surplus decryptable balance
                        &[7; 64],     // surplus auditor_ciphertext_lo
                        &[8; 64],     // surplus auditor_ciphertext_hi
                    ]
                    .concat(),
                );
            }
            assert_eq!(
                parse_instruction_data(&data).unwrap(),
                SettleConfidentialDvpArgs {
                    leg_a_extras_count: 3,
                    payment: payment.clone(),
                    surplus: surplus_present.then(|| surplus.clone()),
                }
            );
        }
    }
}
