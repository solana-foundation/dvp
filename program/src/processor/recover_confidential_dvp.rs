use crate::{
    error::DvpSwapProgramError,
    processor::shared::{
        account_check::{verify_account_owner, verify_signer},
        confidential::{
            check_confidential_escrow, check_confidential_refund, refund_confidential_leg,
            LegBRefund,
        },
        token_utils::{
            get_mint_decimals, get_token_account_balance, transfer_checked_cpi,
            verify_ata_recipient_if_initialized, verify_canonical_ata, MAX_HOOK_REMAINING_ACCOUNTS,
        },
    },
    require, require_len,
    state::swap_dvp::{NONCE_TOMBSTONE_SEED, SWAP_DVP_SEED},
};
use pinocchio::{
    account::AccountView,
    cpi::{Seed, Signer},
    error::ProgramError,
    Address, ProgramResult,
};
use pinocchio_token_2022::{instructions::CloseAccount, ID as TOKEN_2022_PROGRAM_ID};

const FIXED_ACCOUNTS_LEN: usize = 13;

/// Processes the RecoverConfidentialDvp instruction.
///
/// Confidential leg B counterpart of
/// [`process_recover_dvp`](super::recover_dvp::process_recover_dvp): after the
/// swap closes, `user_b` recovers tokens left by a partial refund, a pending
/// credit or a public deposit. Pending credits must first be applied with
/// [`process_apply_confidential_dvp`](super::apply_confidential_dvp::process_apply_confidential_dvp).
/// The leg B escrow closes only when all public and confidential balances are
/// empty. Leg A of a closed confidential swap uses public Recover instead.
///
/// # Account Layout
/// Accounts 0-7 follow the public Recover layout, selecting `user_b`, `mint_b`
/// and the Token-2022 leg B escrow. Account 8 is the ZK ElGamal Proof program;
/// accounts 9-12 are writable equality, validity, range and zero contexts,
/// or DvP program-id placeholders according to the refund mode.
/// Transfer-hook extras follow. Closed-account and context rent goes to `user_b`.
///
/// # Instruction Data
/// The five seed addresses and nonce from public Recover, followed by
/// [`LegBRefund`]. Refund modes and context requirements follow
/// [`process_reclaim_confidential_dvp`](super::reclaim_confidential_dvp::process_reclaim_confidential_dvp).
///
pub fn process_recover_confidential_dvp(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = parse_instruction_data(instruction_data)?;
    require!(
        accounts.len() >= FIXED_ACCOUNTS_LEN,
        ProgramError::NotEnoughAccountKeys
    );
    recover(program_id, accounts, &args)
}

// Separate the decoded transfer payload from validation and CPI temporaries.
#[inline(never)]
fn recover(
    program_id: &Address,
    accounts: &[AccountView],
    args: &RecoverConfidentialDvpArgs,
) -> ProgramResult {
    let (fixed, remaining) = accounts.split_at(FIXED_ACCOUNTS_LEN);
    let [signer, swap, tombstone, mint, escrow, destination, token, memo, zk_program, contexts @ ..] =
        fixed
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require!(
        remaining.len() <= MAX_HOOK_REMAINING_ACCOUNTS,
        ProgramError::InvalidArgument
    );
    verify_signer(signer, true)?;
    require!(
        signer.address() == &args.user_b,
        DvpSwapProgramError::SignerNotParty
    );
    require!(
        mint.address() == &args.mint_b,
        ProgramError::InvalidAccountData
    );
    require!(
        token.address() == &TOKEN_2022_PROGRAM_ID,
        ProgramError::IncorrectProgramId
    );
    let nonce_bytes = args.nonce.to_le_bytes();
    let (expected_swap, bump) = Address::find_program_address(
        &[
            SWAP_DVP_SEED,
            args.settlement_authority.as_ref(),
            args.user_a.as_ref(),
            args.user_b.as_ref(),
            args.mint_a.as_ref(),
            args.mint_b.as_ref(),
            &nonce_bytes,
        ],
        program_id,
    );
    require!(swap.address() == &expected_swap, ProgramError::InvalidSeeds);
    require!(
        swap.owned_by(&pinocchio_system::ID) && swap.is_data_empty(),
        DvpSwapProgramError::DvpStillOpen
    );
    // With state gone, this tombstone authenticates the supplied seed parties.
    let (expected_tombstone, _) =
        Address::find_program_address(&[NONCE_TOMBSTONE_SEED, expected_swap.as_ref()], program_id);
    require!(
        tombstone.address() == &expected_tombstone,
        ProgramError::InvalidAccountData
    );
    require!(
        tombstone.owned_by(program_id),
        DvpSwapProgramError::DvpNeverCreated
    );
    verify_canonical_ata(escrow, swap.address(), &args.mint_b, token)?;
    verify_account_owner(escrow, token.address())?;
    check_confidential_escrow(escrow)?;
    verify_canonical_ata(destination, signer.address(), &args.mint_b, token)?;
    verify_ata_recipient_if_initialized(destination, signer.address(), &args.mint_b)?;
    check_confidential_refund(
        program_id,
        &args.leg_b_refund,
        escrow,
        destination,
        signer,
        zk_program,
        contexts,
    )?;
    let bump_bytes = [bump];
    let seeds = [
        Seed::from(SWAP_DVP_SEED),
        Seed::from(args.settlement_authority.as_ref()),
        Seed::from(args.user_a.as_ref()),
        Seed::from(args.user_b.as_ref()),
        Seed::from(args.mint_a.as_ref()),
        Seed::from(args.mint_b.as_ref()),
        Seed::from(&nonce_bytes),
        Seed::from(&bump_bytes),
    ];
    let signers = [Signer::from(&seeds)];
    let reset = refund_confidential_leg(
        &args.leg_b_refund,
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
    if matches!(args.leg_b_refund, LegBRefund::None) {
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
    }
    // Partial always leaves the escrow open. Full may leave pending or public
    // tokens; None may withdraw public tokens while pending is still present.
    if reset && get_token_account_balance(escrow)? == 0 {
        CloseAccount {
            account: escrow,
            destination: signer,
            authority: swap,
            token_program: token.address(),
        }
        .invoke_signed(&signers)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
struct RecoverConfidentialDvpArgs {
    settlement_authority: Address,
    user_a: Address,
    user_b: Address,
    mint_a: Address,
    mint_b: Address,
    nonce: u64,
    leg_b_refund: LegBRefund,
}

fn parse_instruction_data(data: &[u8]) -> Result<RecoverConfidentialDvpArgs, ProgramError> {
    require_len!(data, 32 + 32 + 32 + 32 + 32 + 8);
    let mut offset = 0;

    let settlement_authority = Address::new_from_array(
        data[offset..offset + 32]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 32;
    let user_a = Address::new_from_array(
        data[offset..offset + 32]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 32;
    let user_b = Address::new_from_array(
        data[offset..offset + 32]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 32;
    let mint_a = Address::new_from_array(
        data[offset..offset + 32]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 32;
    let mint_b = Address::new_from_array(
        data[offset..offset + 32]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 32;
    let nonce = u64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 8;
    let leg_b_refund = LegBRefund::try_from(&data[offset..])?;
    Ok(RecoverConfidentialDvpArgs {
        settlement_authority,
        user_a,
        user_b,
        mint_a,
        mint_b,
        nonce,
        leg_b_refund,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_instruction_data_decodes_fields() {
        let prefix: &[&[u8]] = &[
            &[1; 32],                                // settlement_authority
            &[2; 32],                                // user_a
            &[3; 32],                                // user_b
            &[10; 32],                               // mint_a
            &[20; 32],                               // mint_b
            &0x0102_0304_0506_0708u64.to_le_bytes(), // nonce
        ];
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
            let mut data = prefix.concat();
            data.push(tag);
            if tag != 0 {
                data.extend_from_slice(&transfer_bytes);
            }
            assert_eq!(
                parse_instruction_data(&data).unwrap(),
                RecoverConfidentialDvpArgs {
                    settlement_authority: Address::new_from_array([1; 32]),
                    user_a: Address::new_from_array([2; 32]),
                    user_b: Address::new_from_array([3; 32]),
                    mint_a: Address::new_from_array([10; 32]),
                    mint_b: Address::new_from_array([20; 32]),
                    nonce: 0x0102_0304_0506_0708,
                    leg_b_refund
                }
            );
        }
    }
}
