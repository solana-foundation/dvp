use crate::{
    error::DvpSwapProgramError,
    require,
    state::swap_dvp::{NONCE_TOMBSTONE_SEED, SWAP_DVP_SEED},
};
use pinocchio::{
    account::AccountView,
    address::Address,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::rent::Rent,
    ProgramResult,
};
use pinocchio_system::instructions::{Allocate, Assign, CreateAccount, Transfer};

pub fn create_pda_account<const N: usize>(
    payer: &AccountView,
    rent: &Rent,
    space: usize,
    owner: &Address,
    new_pda_account: &AccountView,
    new_pda_signer_seeds: [Seed; N],
) -> ProgramResult {
    let signers = [Signer::from(&new_pda_signer_seeds)];
    let required_lamports = rent.try_minimum_balance(space)?.max(1);

    if new_pda_account.lamports() > 0 {
        let required_lamports = required_lamports.saturating_sub(new_pda_account.lamports());
        if required_lamports > 0 {
            Transfer {
                from: payer,
                to: new_pda_account,
                lamports: required_lamports,
            }
            .invoke()?;
        }
        Allocate {
            account: new_pda_account,
            space: space as u64,
        }
        .invoke_signed(&signers)?;
        Assign {
            account: new_pda_account,
            owner,
        }
        .invoke_signed(&signers)
    } else {
        CreateAccount {
            from: payer,
            to: new_pda_account,
            lamports: required_lamports,
            space: space as u64,
            owner,
        }
        .invoke_signed(&signers)
    }
}

/// SwapDvp seed inputs in seed order. After close, only the nonce
/// tombstone authenticates them.
pub struct SwapDvpSeeds<'a> {
    pub settlement_authority: &'a Address,
    pub user_a: &'a Address,
    pub user_b: &'a Address,
    pub mint_a: &'a Address,
    pub mint_b: &'a Address,
    pub nonce: u64,
}

/// A SwapDvp PDA re-derived from caller-supplied seed inputs, for the paths
/// that may run after the swap state is gone.
pub struct SwapDvpPda<'a> {
    seeds: SwapDvpSeeds<'a>,
    address: Address,
    nonce: [u8; 8],
    bump: [u8; 1],
}

impl<'a> SwapDvpPda<'a> {
    /// Re-derives the PDA and requires `swap` to be at that address.
    pub fn derive(
        program_id: &Address,
        swap: &AccountView,
        seeds: SwapDvpSeeds<'a>,
    ) -> Result<Self, ProgramError> {
        let nonce = seeds.nonce.to_le_bytes();
        let (address, bump) = Address::find_program_address(
            &[
                SWAP_DVP_SEED,
                seeds.settlement_authority.as_ref(),
                seeds.user_a.as_ref(),
                seeds.user_b.as_ref(),
                seeds.mint_a.as_ref(),
                seeds.mint_b.as_ref(),
                &nonce,
            ],
            program_id,
        );
        require!(swap.address() == &address, ProgramError::InvalidSeeds);
        Ok(Self {
            seeds,
            address,
            nonce,
            bump: [bump],
        })
    }

    /// Requires `tombstone` at the nonce tombstone address of this PDA.
    pub fn verify_tombstone_address(
        &self,
        program_id: &Address,
        tombstone: &AccountView,
    ) -> ProgramResult {
        let (expected, _) = Address::find_program_address(
            &[NONCE_TOMBSTONE_SEED, self.address.as_ref()],
            program_id,
        );
        require!(
            tombstone.address() == &expected,
            ProgramError::InvalidAccountData
        );
        Ok(())
    }

    /// Requires a closed swap whose tombstone exists. The tombstone outlives
    /// the trade and proves a DvP with exactly these seed inputs was created
    /// by this program, which authenticates the seed parties.
    pub fn verify_closed(
        &self,
        program_id: &Address,
        swap: &AccountView,
        tombstone: &AccountView,
    ) -> ProgramResult {
        // A live SwapDvp is program-owned; its open-swap paths validate
        // against stored state instead.
        require!(
            swap.owned_by(&pinocchio_system::ID) && swap.is_data_empty(),
            DvpSwapProgramError::DvpStillOpen
        );
        self.verify_tombstone_address(program_id, tombstone)?;
        require!(
            tombstone.owned_by(program_id),
            DvpSwapProgramError::DvpNeverCreated
        );
        Ok(())
    }

    /// PDA signer seeds, including the bump.
    pub fn signer_seeds(&self) -> [Seed<'_>; 8] {
        [
            Seed::from(SWAP_DVP_SEED),
            Seed::from(self.seeds.settlement_authority.as_ref()),
            Seed::from(self.seeds.user_a.as_ref()),
            Seed::from(self.seeds.user_b.as_ref()),
            Seed::from(self.seeds.mint_a.as_ref()),
            Seed::from(self.seeds.mint_b.as_ref()),
            Seed::from(&self.nonce),
            Seed::from(&self.bump),
        ]
    }
}
