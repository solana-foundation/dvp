use crate::{
    error::DvpSwapProgramError,
    processor::shared::account_check::{verify_account_owner, verify_signer, verify_token_program},
    processor::shared::confidential::has_confidential_transfer_account,
    processor::shared::token_utils::{
        get_mint_decimals, get_token_account_balance, transfer_checked_cpi,
        verify_ata_recipient_if_initialized, verify_canonical_ata,
    },
    require, require_len,
    state::swap_dvp::{NONCE_TOMBSTONE_SEED, SWAP_DVP_SEED},
};
use pinocchio::{
    account::AccountView,
    address::Address,
    cpi::{Seed, Signer},
    error::ProgramError,
    ProgramResult,
};
use pinocchio_token_2022::instructions::CloseAccount;

/// Length of the fixed account prefix; anything beyond this is treated
/// as transfer-hook remaining accounts for the single recovery CPI.
const FIXED_ACCOUNTS_LEN: usize = 8;

/// Wire size of the instruction data: five 32-byte seed addresses plus
/// the u64 nonce.
const INSTRUCTION_DATA_LEN: usize = 32 * 5 + 8;

/// Processes the RecoverDvp instruction.
///
/// Recovers deposits that land after the trade is closed. Funding is a
/// raw SPL transfer that never reads the SwapDvp, so a delayed or
/// front-run deposit can arrive after Settle/Cancel/Reject closed the
/// escrows: anyone can recreate the escrow ATA for the dead PDA, the
/// in-flight transfer settles into it, and the tombstone prevents ever
/// reviving the SwapDvp to sign it out again.
///
/// The state is gone, so the caller supplies the original seed inputs
/// and the program re-derives the PDA. The tombstone at the derived
/// address proves a DvP with exactly these parties/mints/nonce existed;
/// since the parties are seed inputs, that also authenticates the party
/// check below.
///
/// Permissioned to either depositor; the signer selects the leg, as in
/// ReclaimDvp: user_a recovers the `mint_a` escrow, user_b the `mint_b`
/// escrow. Drains the escrow's actual balance to the signer's canonical
/// ATA, then closes the escrow (rent to the signer) so it stops
/// trapping deposits. Only valid once the SwapDvp is closed; Reclaim is
/// the live-trade path. No extension validation, same policy as the
/// other unwind paths: skipping it means the program never blocks
/// recovery, but the drain is still a real `TransferChecked`, so a mint
/// recreated as `NonTransferable` or fee-bearing after close is
/// unmovable or taxed by the token program itself. That is a
/// trusted-authority risk, not a guarantee this path can override. The
/// token program is bound to the escrow account rather than the mint's
/// current owner, preserving the original ATA address. After mint recreation
/// under another token program, an empty escrow can still be closed because
/// no transfer runs. A non-zero Token-2022 escrow cannot be drained in that
/// state: TransferChecked rejects the mint owner mismatch.
///
/// # Account Layout
/// 0. `[signer, writable]` signer - Depositor of the leg being recovered; receives the closed escrow's rent
/// 1. `[]` swap_dvp - Closed SwapDvp address; must be system-owned and empty (signs the CPIs)
/// 2. `[]` nonce_tombstone - Tombstone PDA for `swap_dvp`; must be program-owned
/// 3. `[]` mint - Mint of the leg being recovered
/// 4. `[writable]` dvp_escrow_ata - Recreated escrow ATA (drained, then closed)
/// 5. `[writable]` signer_dest_ata - Signer's canonical ATA for the leg's mint (caller must pre-initialize if the escrow has a non-zero balance)
/// 6. `[]` token_program - SPL Token or Token-2022; must own `dvp_escrow_ata`
/// 7. `[]` memo_program - SPL Memo program; only used if signer_dest_ata requires a memo
///
/// Trailing accounts (variable): transfer-hook extras forwarded to the
/// recovery `TransferChecked` CPI, as in ReclaimDvp.
///
/// # Instruction Data
/// The original DvP seed inputs, in PDA-seed order:
/// * `settlement_authority` (Pubkey)
/// * `user_a` (Pubkey)
/// * `user_b` (Pubkey)
/// * `mint_a` (Pubkey)
/// * `mint_b` (Pubkey)
/// * `nonce` (u64)
pub fn process_recover_dvp(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    require!(
        accounts.len() >= FIXED_ACCOUNTS_LEN,
        ProgramError::NotEnoughAccountKeys
    );
    let (fixed, remaining) = accounts.split_at(FIXED_ACCOUNTS_LEN);
    let [signer_info, swap_dvp_info, nonce_tombstone_info, mint_info, dvp_escrow_ata_info, signer_dest_ata_info, token_program_info, memo_program_info] =
        fixed
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Writable: the signer receives the closed escrow's rent.
    verify_signer(signer_info, true)?;

    let args = parse_instruction_data(instruction_data)?;

    // Leg selection by signer identity, as in ReclaimDvp. The claimed
    // parties are trusted only because they are seed inputs validated
    // by the tombstone check below.
    let leg_mint = if signer_info.address() == &args.user_a {
        &args.mint_a
    } else if signer_info.address() == &args.user_b {
        &args.mint_b
    } else {
        return Err(DvpSwapProgramError::SignerNotParty.into());
    };

    require!(
        mint_info.address() == leg_mint,
        ProgramError::InvalidAccountData
    );
    verify_token_program(token_program_info)?;

    let nonce_bytes = args.nonce.to_le_bytes();
    let (expected_swap_dvp, bump) = Address::find_program_address(
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
        swap_dvp_info.address() == &expected_swap_dvp,
        ProgramError::InvalidSeeds
    );

    // Post-close only. A live SwapDvp is program-owned and ReclaimDvp
    // applies there, validating against stored state instead.
    require!(
        swap_dvp_info.owned_by(&pinocchio_system::ID) && swap_dvp_info.is_data_empty(),
        DvpSwapProgramError::DvpStillOpen
    );

    // The tombstone outlives the trade; it proves a DvP with exactly
    // these seed inputs was once created by this program.
    let (expected_tombstone, _) = Address::find_program_address(
        &[NONCE_TOMBSTONE_SEED, expected_swap_dvp.as_ref()],
        program_id,
    );
    require!(
        nonce_tombstone_info.address() == &expected_tombstone,
        ProgramError::InvalidAccountData
    );
    require!(
        nonce_tombstone_info.owned_by(program_id),
        DvpSwapProgramError::DvpNeverCreated
    );

    // dvp_escrow_ata: the dead PDA's canonical escrow for the leg's
    // mint, the only address the documented funding path deposits to.
    // The token program is bound to the escrow account, not to the
    // mint's current owner. Following a recreated mint's owner would derive
    // a different ATA. Preserving the address lets an empty old escrow close;
    // draining a non-zero T22 escrow still requires the mint's owner to match.
    // The escrow authenticates its own namespace: only the ATA program can
    // create an account at this derived address, and it sets the owner
    // to the token program used in the derivation.
    verify_canonical_ata(
        dvp_escrow_ata_info,
        swap_dvp_info.address(),
        leg_mint,
        token_program_info,
    )?;
    verify_account_owner(dvp_escrow_ata_info, token_program_info.address())?;
    require!(
        !has_confidential_transfer_account(dvp_escrow_ata_info)?,
        DvpSwapProgramError::SwapModeMismatch
    );
    // signer_dest_ata: the depositor's ATA for the leg's mint.
    verify_canonical_ata(
        signer_dest_ata_info,
        signer_info.address(),
        leg_mint,
        token_program_info,
    )?;
    // A canonical ATA can be owner-reassigned via legacy SPL SetAuthority
    // without changing its pubkey; reject a reassigned account so the drain
    // can't deliver to the new owner. Matches Cancel/Reject/Settle.
    verify_ata_recipient_if_initialized(signer_dest_ata_info, signer_info.address(), leg_mint)?;

    let bump_bytes = [bump];
    let swap_dvp_seeds = [
        Seed::from(SWAP_DVP_SEED),
        Seed::from(args.settlement_authority.as_ref()),
        Seed::from(args.user_a.as_ref()),
        Seed::from(args.user_b.as_ref()),
        Seed::from(args.mint_a.as_ref()),
        Seed::from(args.mint_b.as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(bump_bytes.as_ref()),
    ];
    let signer_seeds = [Signer::from(&swap_dvp_seeds)];

    // Errors with InvalidAccountOwner if the escrow was never recreated
    // (nothing to recover).
    let escrow_balance = get_token_account_balance(dvp_escrow_ata_info)?;

    if escrow_balance > 0 {
        transfer_checked_cpi(
            dvp_escrow_ata_info,
            mint_info,
            signer_dest_ata_info,
            swap_dvp_info,
            escrow_balance,
            get_mint_decimals(mint_info)?,
            token_program_info.address(),
            memo_program_info,
            remaining,
            &signer_seeds,
        )?;
    }

    // Close even when empty: removes the recreated deposit trap and
    // sweeps its rent to the depositor.
    CloseAccount {
        account: dvp_escrow_ata_info,
        destination: signer_info,
        authority: swap_dvp_info,
        token_program: token_program_info.address(),
    }
    .invoke_signed(&signer_seeds)
}

struct RecoverDvpArgs {
    settlement_authority: Address,
    user_a: Address,
    user_b: Address,
    mint_a: Address,
    mint_b: Address,
    nonce: u64,
}

/// Wire layout (fixed, 168 bytes):
///   settlement_authority(32) | user_a(32) | user_b(32) |
///   mint_a(32) | mint_b(32) | nonce(8)
fn parse_instruction_data(data: &[u8]) -> Result<RecoverDvpArgs, ProgramError> {
    require_len!(data, INSTRUCTION_DATA_LEN);

    let mut offset = 0;
    let mut read_address = || {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&data[offset..offset + 32]);
        offset += 32;
        Address::new_from_array(bytes)
    };

    let settlement_authority = read_address();
    let user_a = read_address();
    let user_b = read_address();
    let mint_a = read_address();
    let mint_b = read_address();

    let nonce = u64::from_le_bytes(
        data[offset..INSTRUCTION_DATA_LEN]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );

    Ok(RecoverDvpArgs {
        settlement_authority,
        user_a,
        user_b,
        mint_a,
        mint_b,
        nonce,
    })
}
