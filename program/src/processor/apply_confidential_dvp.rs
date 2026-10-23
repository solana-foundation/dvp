use crate::{
    error::DvpSwapProgramError,
    processor::shared::{
        apply_pending_balance_cpi, check_confidential_escrow, verify_canonical_ata, verify_signer,
    },
    require, require_len,
    state::swap_dvp::{ConfidentialSwapDvp, NONCE_TOMBSTONE_SEED, SWAP_DVP_SEED},
};
use pinocchio::{
    account::AccountView,
    cpi::{Seed, Signer},
    error::ProgramError,
    Address, ProgramResult,
};

/// Processes the ApplyConfidentialDvp instruction.
///
/// Moves the leg B escrow's pending confidential balance into its available
/// balance through Token-2022 ApplyPendingBalance. Either depositor or the
/// settlement authority may apply while the swap is open or after it closes.
/// There is no public counterpart; the post-close PDA and tombstone checks
/// follow [`process_recover_dvp`](super::recover_dvp::process_recover_dvp).
///
/// # Account Layout
/// 0. `[signer]` signer - user_a, user_b or settlement_authority
/// 1. `[]` swap_dvp - Open confidential swap or its closed PDA address
/// 2. `[]` nonce_tombstone - Proves the swap existed after it closes
/// 3. `[writable]` dvp_ata_b - PDA's canonical Token-2022 leg B escrow
/// 4. `[]` token_program - Token-2022 program
///
/// No proof contexts or transfer-hook extras are required.
///
/// # Instruction Data
/// `expected_pending_balance_credit_counter` (u64), the new decryptable
/// available balance (36 bytes), then the five seed addresses and nonce in
/// public Recover order. The counter and decryptable balance are forwarded
/// to Token-2022; the DvP program cannot verify the encrypted plaintext.
pub fn process_apply_confidential_dvp(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = parse_instruction_data(instruction_data)?;

    let [signer_info, swap_dvp_info, nonce_tombstone_info, dvp_ata_b_info, token_program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    verify_signer(signer_info, false)?;
    require!(
        token_program_info.address() == &pinocchio_token_2022::ID,
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
    require!(
        swap_dvp_info.address() == &expected_swap,
        ProgramError::InvalidSeeds
    );

    let (expected_tombstone, _) =
        Address::find_program_address(&[NONCE_TOMBSTONE_SEED, expected_swap.as_ref()], program_id);
    require!(
        nonce_tombstone_info.address() == &expected_tombstone,
        ProgramError::InvalidAccountData
    );

    if swap_dvp_info.owned_by(program_id) {
        let dvp = ConfidentialSwapDvp::load(&swap_dvp_info.try_borrow()?)?;
        require!(
            signer_info.address() == &dvp.base.user_a
                || signer_info.address() == &dvp.base.user_b
                || signer_info.address() == &dvp.base.settlement_authority,
            DvpSwapProgramError::SignerNotParty
        );
    } else {
        // As in Recover, a closed PDA must be empty and its permanent
        // tombstone authenticates the supplied parties, mints and nonce.
        require!(
            swap_dvp_info.owned_by(&pinocchio_system::ID) && swap_dvp_info.is_data_empty(),
            DvpSwapProgramError::DvpStillOpen
        );
        require!(
            nonce_tombstone_info.owned_by(program_id),
            DvpSwapProgramError::DvpNeverCreated
        );
        require!(
            signer_info.address() == &args.user_a
                || signer_info.address() == &args.user_b
                || signer_info.address() == &args.settlement_authority,
            DvpSwapProgramError::SignerNotParty
        );
    }

    verify_canonical_ata(
        dvp_ata_b_info,
        swap_dvp_info.address(),
        &args.mint_b,
        token_program_info,
    )?;
    check_confidential_escrow(dvp_ata_b_info)?;
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
    // Token-2022 records the supplied counter and decryptable balance. Neither
    // is used to authorize Apply or to validate the ElGamal pending balance.
    apply_pending_balance_cpi(
        dvp_ata_b_info,
        swap_dvp_info,
        args.expected_pending_balance_credit_counter,
        &args.new_decryptable_available_balance,
        &[Signer::from(&seeds)],
    )
}

#[derive(Debug, PartialEq)]
struct ApplyConfidentialDvpArgs {
    expected_pending_balance_credit_counter: u64,
    new_decryptable_available_balance: [u8; 36],
    settlement_authority: Address,
    user_a: Address,
    user_b: Address,
    mint_a: Address,
    mint_b: Address,
    nonce: u64,
}

fn parse_instruction_data(data: &[u8]) -> Result<ApplyConfidentialDvpArgs, ProgramError> {
    require_len!(data, 8 + 36 + 32 + 32 + 32 + 32 + 32 + 8);
    let mut offset = 0;

    let expected_pending_balance_credit_counter = u64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 8;
    let new_decryptable_available_balance: [u8; 36] = data[offset..offset + 36]
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    offset += 36;
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
    require!(data.len() == offset, ProgramError::InvalidInstructionData);
    Ok(ApplyConfidentialDvpArgs {
        expected_pending_balance_credit_counter,
        new_decryptable_available_balance,
        settlement_authority,
        user_a,
        user_b,
        mint_a,
        mint_b,
        nonce,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_instruction_data_decodes_fields() {
        let data = [
            &257u64.to_le_bytes()[..], // expected_pending_balance_credit_counter
            &[6; 36],                  // new_decryptable_available_balance
            &[1; 32],                  // settlement_authority
            &[2; 32],                  // user_a
            &[3; 32],                  // user_b
            &[10; 32],                 // mint_a
            &[20; 32],                 // mint_b
            &0x0102_0304_0506_0708u64.to_le_bytes(), // nonce
        ]
        .concat();
        assert_eq!(
            parse_instruction_data(&data).unwrap(),
            ApplyConfidentialDvpArgs {
                expected_pending_balance_credit_counter: 257,
                new_decryptable_available_balance: [6; 36],
                settlement_authority: Address::new_from_array([1; 32]),
                user_a: Address::new_from_array([2; 32]),
                user_b: Address::new_from_array([3; 32]),
                mint_a: Address::new_from_array([10; 32]),
                mint_b: Address::new_from_array([20; 32]),
                nonce: 0x0102_0304_0506_0708,
            }
        );
    }
}
