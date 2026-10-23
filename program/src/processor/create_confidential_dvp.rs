extern crate alloc;

use crate::{
    error::DvpSwapProgramError,
    processor::create_dvp::{verify_party_signer_capable, MAX_DVP_DURATION_SECS},
    processor::shared::account_check::{
        verify_account_owner, verify_ata_program, verify_signer, verify_system_account,
        verify_system_program, verify_token_program,
    },
    processor::shared::confidential::{
        configure_confidential_account_cpi, disable_non_confidential_credits_cpi,
        reallocate_confidential_account_cpi, verify_confidential_mint,
    },
    processor::shared::pda_utils::create_pda_account,
    processor::shared::token_utils::{
        get_mint_authority, get_token_account_balance, validate_mint_extensions,
        verify_canonical_ata, verify_escrow_not_preloaded,
    },
    require, require_len,
    state::swap_dvp::{
        ConfidentialSwapDvp, SwapDvp, MAX_REF_STRING_LEN, NONCE_TOMBSTONE_SEED, SWAP_DVP_SEED,
    },
};
use pinocchio::{
    account::AccountView,
    address::Address,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};
use pinocchio_associated_token_account::instructions::CreateIdempotent as CreateAtaIdempotent;

/// Processes the CreateConfidentialDvp instruction.
///
/// Confidential counterpart of [`process_create_dvp`](super::create_dvp::process_create_dvp):
/// creates a swap with a public leg A and an encrypted leg B amount, and
/// configures the Token-2022 leg B escrow for confidential transfers.
///
/// # Account Layout
/// Accounts 0-13 follow the public Create layout. `mint_b` must support
/// confidential transfers and `token_program_b` must be Token-2022.
/// Account 14 is the read-only instructions sysvar, used to locate the inline
/// PubkeyValidity proof for the escrow's ElGamal key.
///
/// # Instruction Data
/// `amount_a`, `expiry_timestamp` and `nonce` are followed by the low and high
/// ciphertexts of the agreed leg B amount, `decryptable_zero_balance` and a
/// non-zero relative `pubkey_validity_proof_offset`. There is no public
/// `amount_b`. The reference string, settlement destinations and earliest
/// settlement timestamp use the same optional encoding as public Create.
pub fn process_create_confidential_dvp(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = parse_instruction_data(instruction_data)?;
    let [payer_info, swap_dvp_info, nonce_tombstone_info, settlement_authority_info, user_a_info, user_b_info, mint_a_info, mint_b_info, dvp_ata_a_info, dvp_ata_b_info, system_program_info, token_program_a_info, token_program_b_info, associated_token_program_info, instructions_sysvar_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    verify_signer(payer_info, true)?;
    verify_system_account(swap_dvp_info, true)?;
    verify_system_program(system_program_info)?;
    verify_token_program(token_program_a_info)?;
    require!(
        token_program_b_info.address() == &pinocchio_token_2022::ID,
        DvpSwapProgramError::MintNotConfidential
    );
    verify_confidential_mint(mint_b_info)?;
    verify_ata_program(associated_token_program_info)?;
    require!(
        instructions_sysvar_info.address() == &pinocchio::sysvars::instructions::INSTRUCTIONS_ID,
        ProgramError::UnsupportedSysvar
    );
    // settlement_authority receives the closed-account rent at Settle/Cancel.
    // An executable account can't be credited lamports (ExecutableLamportChange),
    // so reject it at creation rather than stranding funds until Reject/Reclaim.
    require!(
        !settlement_authority_info.executable(),
        DvpSwapProgramError::SettlementAuthorityExecutable
    );
    // Each party must be a wallet-style identity so it can authorize the
    // unwind paths (Reject/Reclaim/Recover) as a signer, either directly
    // as a keypair or as a smart-wallet PDA via CPI. The settlement
    // authority needs no such check: if it can't sign, the parties still
    // recover their funds via Reject/Reclaim.
    verify_party_signer_capable(user_a_info)?;
    verify_party_signer_capable(user_b_info)?;
    verify_account_owner(mint_a_info, token_program_a_info.address())?;
    verify_account_owner(mint_b_info, token_program_b_info.address())?;
    validate_mint_extensions(mint_a_info)?;
    validate_mint_extensions(mint_b_info)?;

    let now = Clock::get()?.unix_timestamp;
    validate_args(
        &args,
        settlement_authority_info.address(),
        user_a_info.address(),
        user_b_info.address(),
        mint_a_info.address(),
        mint_b_info.address(),
        now,
    )?;

    let nonce_bytes = args.nonce.to_le_bytes();
    let (expected_swap_dvp, bump) = Address::find_program_address(
        &[
            SWAP_DVP_SEED,
            settlement_authority_info.address().as_ref(),
            user_a_info.address().as_ref(),
            user_b_info.address().as_ref(),
            mint_a_info.address().as_ref(),
            mint_b_info.address().as_ref(),
            &nonce_bytes,
        ],
        program_id,
    );
    require!(
        swap_dvp_info.address() == &expected_swap_dvp,
        ProgramError::InvalidSeeds
    );

    // Resolve the destination defaults here (the consent point) so
    // Settle never branches: delivery always goes to the stored
    // destination's canonical ATA.
    let user_a_settlement_destination = args
        .user_a_settlement_destination
        .unwrap_or(*user_a_info.address());
    let user_b_settlement_destination = args
        .user_b_settlement_destination
        .unwrap_or(*user_b_info.address());
    require!(
        user_a_settlement_destination != expected_swap_dvp
            && user_b_settlement_destination != expected_swap_dvp,
        DvpSwapProgramError::SettlementDestinationIsSwapDvp
    );

    // Nonce tombstone, derived from the SwapDvp address so it's 1:1 with
    // this trade's seeds. It's created below and never closed, so a
    // non-system owner here means the nonce was already used - reject
    // before re-creating the (closed) SwapDvp at the same address.
    let (expected_tombstone, tombstone_bump) = Address::find_program_address(
        &[NONCE_TOMBSTONE_SEED, expected_swap_dvp.as_ref()],
        program_id,
    );
    require!(
        nonce_tombstone_info.address() == &expected_tombstone,
        ProgramError::InvalidAccountData
    );
    require!(
        nonce_tombstone_info.owned_by(&pinocchio_system::ID),
        DvpSwapProgramError::NonceAlreadyUsed
    );

    // dvp_ata_a is the DvP PDA's ATA for mint_a (asset escrow).
    verify_canonical_ata(
        dvp_ata_a_info,
        swap_dvp_info.address(),
        mint_a_info.address(),
        token_program_a_info,
    )?;
    // dvp_ata_b is the DvP PDA's ATA for mint_b (leg B escrow).
    verify_canonical_ata(
        dvp_ata_b_info,
        swap_dvp_info.address(),
        mint_b_info.address(),
        token_program_b_info,
    )?;

    let base = SwapDvp {
        bump,
        user_a: *user_a_info.address(),
        user_b: *user_b_info.address(),
        mint_a: *mint_a_info.address(),
        mint_b: *mint_b_info.address(),
        settlement_authority: *settlement_authority_info.address(),
        token_program_a: *token_program_a_info.address(),
        token_program_b: *token_program_b_info.address(),
        amount_a: args.amount_a,
        // Confidential mode stores a sentinel in the fixed-width public base.
        amount_b: u64::MAX,
        expiry_timestamp: args.expiry_timestamp,
        nonce: args.nonce,
        ref_string: args.ref_string,
        user_a_settlement_destination,
        user_b_settlement_destination,
        mint_a_authority: get_mint_authority(mint_a_info)?.unwrap_or_default(),
        mint_b_authority: get_mint_authority(mint_b_info)?.unwrap_or_default(),
        earliest_settlement_timestamp: args.earliest_settlement_timestamp,
    };
    let (nonce_bytes, bump_bytes) = base.seed_buffers();
    let swap_dvp_seeds = base.signing_seeds(&nonce_bytes, &bump_bytes);

    let rent = Rent::get()?;
    // A preload above the rent reserve would be adopted into the live
    // PDA and swept to the closer at the terminal instructions.
    // Up to the reserve is harmless: the payer tops up to exactly it.
    require!(
        swap_dvp_info.lamports() <= rent.try_minimum_balance(ConfidentialSwapDvp::LEN)?,
        DvpSwapProgramError::SwapDvpPreloadedWithLamports
    );
    create_pda_account(
        payer_info,
        &rent,
        ConfidentialSwapDvp::LEN,
        program_id,
        swap_dvp_info,
        swap_dvp_seeds.clone(),
    )?;

    // Mark this nonce used. The tombstone holds no data - its mere
    // existence (program-owned) is the signal - and is never closed.
    let tombstone_bump_bytes = [tombstone_bump];
    let tombstone_seeds = [
        Seed::from(NONCE_TOMBSTONE_SEED),
        Seed::from(expected_swap_dvp.as_ref()),
        Seed::from(&tombstone_bump_bytes),
    ];
    create_pda_account(
        payer_info,
        &rent,
        0,
        program_id,
        nonce_tombstone_info,
        tombstone_seeds,
    )?;

    CreateAtaIdempotent {
        funding_account: payer_info,
        account: dvp_ata_a_info,
        wallet: swap_dvp_info,
        mint: mint_a_info,
        system_program: system_program_info,
        token_program: token_program_a_info,
    }
    .invoke()?;

    CreateAtaIdempotent {
        funding_account: payer_info,
        account: dvp_ata_b_info,
        wallet: swap_dvp_info,
        mint: mint_b_info,
        system_program: system_program_info,
        token_program: token_program_b_info,
    }
    .invoke()?;

    // A non-native escrow must start with no lamports beyond rent, or
    // the close paths would sweep the excess to the closer.
    verify_escrow_not_preloaded(dvp_ata_a_info, &rent)?;
    // Check the current ATA size before Reallocate increases its rent reserve.
    verify_escrow_not_preloaded(dvp_ata_b_info, &rent)?;

    require!(
        get_token_account_balance(dvp_ata_b_info)? == 0,
        DvpSwapProgramError::EscrowPublicBalanceNotEmpty
    );

    let signers = [Signer::from(&swap_dvp_seeds)];
    reallocate_confidential_account_cpi(
        dvp_ata_b_info,
        payer_info,
        system_program_info,
        swap_dvp_info,
        &signers,
    )?;
    configure_confidential_account_cpi(
        dvp_ata_b_info,
        mint_b_info,
        instructions_sysvar_info,
        swap_dvp_info,
        &args.decryptable_zero_balance,
        args.pubkey_validity_proof_offset,
        &signers,
    )?;
    disable_non_confidential_credits_cpi(dvp_ata_b_info, swap_dvp_info, &signers)?;

    let dvp = ConfidentialSwapDvp {
        base,
        amount_b_ciphertext_lo: args.amount_b_ciphertext_lo,
        amount_b_ciphertext_hi: args.amount_b_ciphertext_hi,
    };
    swap_dvp_info
        .try_borrow_mut()?
        .copy_from_slice(&dvp.to_bytes());
    Ok(())
}

#[derive(Debug, PartialEq)]
struct CreateConfidentialDvpArgs {
    amount_a: u64,
    expiry_timestamp: i64,
    nonce: u64,
    amount_b_ciphertext_lo: [u8; 64],
    amount_b_ciphertext_hi: [u8; 64],
    decryptable_zero_balance: [u8; 36],
    pubkey_validity_proof_offset: i8,
    ref_string: [u8; MAX_REF_STRING_LEN],
    user_a_settlement_destination: Option<Address>,
    user_b_settlement_destination: Option<Address>,
    earliest_settlement_timestamp: Option<i64>,
}

fn parse_instruction_data(data: &[u8]) -> Result<CreateConfidentialDvpArgs, ProgramError> {
    require_len!(data, 8 + 8 + 8 + 64 + 64 + 36 + 1 + 4);
    let mut offset = 0;

    let amount_a = u64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 8;
    let expiry_timestamp = i64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 8;
    let nonce = u64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    offset += 8;
    let amount_b_ciphertext_lo: [u8; 64] = data[offset..offset + 64]
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    offset += 64;
    let amount_b_ciphertext_hi: [u8; 64] = data[offset..offset + 64]
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    offset += 64;
    let decryptable_zero_balance: [u8; 36] = data[offset..offset + 36]
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    offset += 36;
    let pubkey_validity_proof_offset = data[offset] as i8;
    offset += 1;
    require!(
        pubkey_validity_proof_offset != 0,
        ProgramError::InvalidInstructionData
    );

    let ref_string = match data[offset] {
        0 => {
            offset += 1;
            [0u8; MAX_REF_STRING_LEN]
        }
        1 => {
            // Tag + u32 length prefix + the three remaining option tags.
            require_len!(data, offset + 1 + 4 + 3);
            let ref_string_wire_len = u32::from_le_bytes(
                data[offset + 1..offset + 5]
                    .try_into()
                    .map_err(|_| ProgramError::InvalidInstructionData)?,
            ) as usize;
            require!(
                ref_string_wire_len <= MAX_REF_STRING_LEN,
                DvpSwapProgramError::RefStringTooLong
            );
            // The string bytes plus the three remaining option tags.
            require_len!(data, offset + 5 + ref_string_wire_len + 3);
            let mut ref_string = [0u8; MAX_REF_STRING_LEN];
            ref_string[..ref_string_wire_len]
                .copy_from_slice(&data[offset + 5..offset + 5 + ref_string_wire_len]);
            offset += 5 + ref_string_wire_len;
            ref_string
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    };

    let user_a_settlement_destination = match data[offset] {
        0 => {
            offset += 1;
            None
        }
        1 => {
            // Tag + pubkey payload + the two remaining option tags.
            require_len!(data, offset + 1 + 32 + 2);
            let mut destination = [0u8; 32];
            destination.copy_from_slice(&data[offset + 1..offset + 33]);
            offset += 33;
            Some(Address::new_from_array(destination))
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    };

    let user_b_settlement_destination = match data[offset] {
        0 => {
            offset += 1;
            None
        }
        1 => {
            // Tag + pubkey payload + the remaining option tag.
            require_len!(data, offset + 1 + 32 + 1);
            let mut destination = [0u8; 32];
            destination.copy_from_slice(&data[offset + 1..offset + 33]);
            offset += 33;
            Some(Address::new_from_array(destination))
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    };

    let earliest_settlement_timestamp = match data[offset] {
        0 => {
            offset += 1;
            None
        }
        1 => {
            require_len!(data, offset + 1 + 8);
            let value = i64::from_le_bytes(
                data[offset + 1..offset + 9]
                    .try_into()
                    .map_err(|_| ProgramError::InvalidInstructionData)?,
            );
            offset += 1 + 8;
            Some(value)
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    };

    require!(data.len() == offset, ProgramError::InvalidInstructionData);
    Ok(CreateConfidentialDvpArgs {
        amount_a,
        expiry_timestamp,
        nonce,
        amount_b_ciphertext_lo,
        amount_b_ciphertext_hi,
        decryptable_zero_balance,
        pubkey_validity_proof_offset,
        ref_string,
        user_a_settlement_destination,
        user_b_settlement_destination,
        earliest_settlement_timestamp,
    })
}

/// Reject DvPs that can never settle, are degenerate, or have leg
/// configurations the rest of the processor would mishandle later.
fn validate_args(
    args: &CreateConfidentialDvpArgs,
    settlement_authority: &Address,
    user_a: &Address,
    user_b: &Address,
    mint_a: &Address,
    mint_b: &Address,
    now: i64,
) -> Result<(), ProgramError> {
    require!(
        args.expiry_timestamp > now,
        DvpSwapProgramError::ExpiryNotInFuture
    );
    require!(
        args.expiry_timestamp <= now.saturating_add(MAX_DVP_DURATION_SECS),
        DvpSwapProgramError::ExpiryTooFarInFuture
    );
    if let Some(earliest) = args.earliest_settlement_timestamp {
        require!(
            earliest <= args.expiry_timestamp,
            DvpSwapProgramError::EarliestAfterExpiry
        );
    }
    require!(user_a != user_b, DvpSwapProgramError::SelfDvp);
    require!(
        settlement_authority != user_a && settlement_authority != user_b,
        DvpSwapProgramError::SettlementAuthorityIsParty
    );
    require!(mint_a != mint_b, DvpSwapProgramError::SameMint);
    require!(args.amount_a != 0, DvpSwapProgramError::ZeroAmount);
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;

    const PROOF_OFFSET: usize = 8 * 3 + 64 * 2 + 36;
    const OPTIONS_OFFSET: usize = PROOF_OFFSET + 1;

    #[test]
    fn validate_confidential_terms() {
        const NOW: i64 = 1_780_000_000;
        let user_a = Address::new_from_array([1; 32]);
        let user_b = Address::new_from_array([2; 32]);
        let authority = Address::new_from_array([3; 32]);
        let mint_a = Address::new_from_array([10; 32]);
        let mint_b = Address::new_from_array([20; 32]);
        let args = || CreateConfidentialDvpArgs {
            amount_a: 1_000,
            expiry_timestamp: NOW + 3_600,
            nonce: 42,
            // The program stores encrypted amount limbs without validating plaintext.
            amount_b_ciphertext_lo: [0; 64],
            amount_b_ciphertext_hi: [0; 64],
            decryptable_zero_balance: [0; 36],
            pubkey_validity_proof_offset: -1,
            ref_string: [0; MAX_REF_STRING_LEN],
            user_a_settlement_destination: None,
            user_b_settlement_destination: None,
            earliest_settlement_timestamp: None,
        };
        for (expiry, earliest, error) in [
            (NOW + 3_600, None, None),
            (NOW + MAX_DVP_DURATION_SECS, None, None),
            (NOW + 3_600, Some(NOW - 1), None),
            (NOW + 3_600, Some(NOW + 3_600), None),
            (NOW, None, Some(DvpSwapProgramError::ExpiryNotInFuture)),
            (NOW - 1, None, Some(DvpSwapProgramError::ExpiryNotInFuture)),
            (
                NOW + MAX_DVP_DURATION_SECS + 1,
                None,
                Some(DvpSwapProgramError::ExpiryTooFarInFuture),
            ),
            (
                NOW + 3_600,
                Some(NOW + 3_601),
                Some(DvpSwapProgramError::EarliestAfterExpiry),
            ),
        ] {
            let mut input = args();
            input.expiry_timestamp = expiry;
            input.earliest_settlement_timestamp = earliest;
            assert_eq!(
                validate_args(&input, &authority, &user_a, &user_b, &mint_a, &mint_b, NOW),
                error.map_or(Ok(()), |error| Err(error.into()))
            );
        }
        for (authority, user_a, user_b, mint_a, mint_b, error) in [
            (
                authority,
                user_a,
                user_a,
                mint_a,
                mint_b,
                DvpSwapProgramError::SelfDvp,
            ),
            (
                user_a,
                user_a,
                user_b,
                mint_a,
                mint_b,
                DvpSwapProgramError::SettlementAuthorityIsParty,
            ),
            (
                user_b,
                user_a,
                user_b,
                mint_a,
                mint_b,
                DvpSwapProgramError::SettlementAuthorityIsParty,
            ),
            (
                authority,
                user_a,
                user_b,
                mint_a,
                mint_a,
                DvpSwapProgramError::SameMint,
            ),
        ] {
            assert_eq!(
                validate_args(&args(), &authority, &user_a, &user_b, &mint_a, &mint_b, NOW),
                Err(error.into())
            );
        }
        let mut zero_asset = args();
        zero_asset.amount_a = 0;
        assert_eq!(
            validate_args(
                &zero_asset,
                &authority,
                &user_a,
                &user_b,
                &mint_a,
                &mint_b,
                NOW
            ),
            Err(DvpSwapProgramError::ZeroAmount.into())
        );
    }

    #[test]
    fn create_options_and_proof_offset_are_strict() {
        let mut data = alloc::vec![0; OPTIONS_OFFSET + 4];
        data[PROOF_OFFSET] = 255; // signed offset -1
        assert_eq!(
            process_create_confidential_dvp(&crate::ID, &[], &data),
            Err(ProgramError::NotEnoughAccountKeys)
        );
        for len in 0..data.len() {
            assert_eq!(
                process_create_confidential_dvp(&crate::ID, &[], &data[..len]),
                Err(ProgramError::InvalidInstructionData)
            );
        }
        for offset in OPTIONS_OFFSET..OPTIONS_OFFSET + 4 {
            data[offset] = 2;
            assert_eq!(
                process_create_confidential_dvp(&crate::ID, &[], &data),
                Err(ProgramError::InvalidInstructionData)
            );
            data[offset] = 0;
        }
        data[PROOF_OFFSET] = 0;
        assert_eq!(
            process_create_confidential_dvp(&crate::ID, &[], &data),
            Err(ProgramError::InvalidInstructionData)
        );
    }

    #[test]
    fn create_present_options_and_string_bounds() {
        let mut prefix = alloc::vec![0; OPTIONS_OFFSET];
        prefix[PROOF_OFFSET] = 255;
        for len in [0u32, 64, 65] {
            let mut data = prefix.clone();
            data.push(1);
            data.extend_from_slice(&len.to_le_bytes());
            data.extend(core::iter::repeat_n(b'x', len as usize));
            for size in [32, 32, 8] {
                data.push(1);
                data.extend(core::iter::repeat_n(0, size));
            }
            if len <= 64 {
                assert_eq!(
                    process_create_confidential_dvp(&crate::ID, &[], &data),
                    Err(ProgramError::NotEnoughAccountKeys)
                );
                for truncated in 0..data.len() {
                    assert_eq!(
                        process_create_confidential_dvp(&crate::ID, &[], &data[..truncated]),
                        Err(ProgramError::InvalidInstructionData)
                    );
                }
            } else {
                assert_eq!(
                    process_create_confidential_dvp(&crate::ID, &[], &data),
                    Err(DvpSwapProgramError::RefStringTooLong.into())
                );
            }
        }
    }

    #[test]
    fn ref_string_preserves_non_utf8_bytes_like_public_create() {
        let mut data = alloc::vec![0; OPTIONS_OFFSET];
        data[PROOF_OFFSET] = 255;
        data.push(1); // ref_string is present
        data.extend_from_slice(&1u32.to_le_bytes());
        data.push(0xff); // opaque reference byte, not valid UTF-8
        data.extend_from_slice(&[0; 3]); // remaining options absent
        let mut expected = [0; MAX_REF_STRING_LEN];
        expected[0] = 0xff;
        assert_eq!(parse_instruction_data(&data).unwrap().ref_string, expected);
    }

    #[test]
    fn parse_instruction_data_decodes_fields() {
        let prefix = [
            &1_000u64.to_le_bytes()[..],             // amount_a
            &(-1_780_000_000i64).to_le_bytes(),      // expiry_timestamp
            &0x0102_0304_0506_0708u64.to_le_bytes(), // nonce
            &[4; 64],                                // amount_b_ciphertext_lo
            &[5; 64],                                // amount_b_ciphertext_hi
            &[6; 36],                                // decryptable_zero_balance
            &(-1i8).to_le_bytes(),                   // pubkey_validity_proof_offset
        ]
        .concat();
        for options_present in [false, true] {
            let mut data = prefix.clone();
            let mut ref_string = [0; MAX_REF_STRING_LEN];
            if options_present {
                ref_string[..5].copy_from_slice(b"trade");
                data.extend_from_slice(
                    &[
                        &[1][..],
                        &5u32.to_le_bytes(),
                        b"trade", // ref_string
                        &[1],
                        &[7; 32], // user_a_settlement_destination
                        &[1],
                        &[8; 32], // user_b_settlement_destination
                        &[1],
                        &(-1_780_003_600i64).to_le_bytes(), // earliest_settlement_timestamp
                    ]
                    .concat(),
                );
            } else {
                data.extend_from_slice(&[0; 4]); // all options absent
            }
            assert_eq!(
                parse_instruction_data(&data).unwrap(),
                CreateConfidentialDvpArgs {
                    amount_a: 1_000,
                    expiry_timestamp: -1_780_000_000,
                    nonce: 0x0102_0304_0506_0708,
                    amount_b_ciphertext_lo: [4; 64],
                    amount_b_ciphertext_hi: [5; 64],
                    decryptable_zero_balance: [6; 36],
                    pubkey_validity_proof_offset: -1,
                    ref_string,
                    user_a_settlement_destination: options_present
                        .then(|| Address::new_from_array([7; 32])),
                    user_b_settlement_destination: options_present
                        .then(|| Address::new_from_array([8; 32])),
                    earliest_settlement_timestamp: options_present.then_some(-1_780_003_600),
                }
            );
            data.push(0);
            assert_eq!(
                parse_instruction_data(&data),
                Err(ProgramError::InvalidInstructionData)
            );
        }
    }
}
