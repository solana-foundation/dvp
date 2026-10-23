//! Token-2022 transfer-hook program used only by the swap program's
//! integration tests.
//!
//! Behaviors selected by the loaded program ID and account count:
//! - **Reject CT:** when loaded at `constants::REJECT_CT_PROGRAM_ID`, rejects the `u64::MAX`
//!   sentinel. Tests switch a funded mint to this hook through Token-2022.
//! - **Benign (default):** logs the account count and returns `Ok`. Tests
//!   assert the log line to confirm `transfer_checked_cpi` forwarded every
//!   account declared in the mint's `ExtraAccountMetaList`.
//! - **Malicious drain (>= 2 extras):** attempts a System transfer of
//!   lamports out of the first extra (which a malicious EAML declares as a
//!   signer targeting the settlement authority) into the second extra.
//!   The swap program strips the signer bit before forwarding, so this
//!   CPI is missing a required signer and reverts the whole terminal
//!   action (see `test_settle_rejects_signer_bearing_hook_extra`).
#![no_std]

mod constants;

use pinocchio::{
    account::AccountView, address::Address, default_allocator, error::ProgramError,
    nostd_panic_handler, program_entrypoint, ProgramResult,
};
use pinocchio_log::log;
use pinocchio_system::instructions::Transfer;

solana_address::declare_id!("HookqJupt6Khm8s8jB3p93NkhPoiAg2M7vkEhkS15CtC");

program_entrypoint!(process_instruction);
default_allocator!();
nostd_panic_handler!();

/// Lamports the malicious path tries to steal from the victim extra.
const DRAIN_LAMPORTS: u64 = 100_000_000;

pub fn process_instruction(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    log!("hook accounts: {}", accounts.len());
    // Execute encodes its amount after the eight-byte discriminator. CT must
    // supply u64::MAX, and hook extras must retain writable but lose signer.
    const EXECUTE_DISCRIMINATOR_LEN: usize = 8;
    if let Some(amount) = instruction_data
        .get(EXECUTE_DISCRIMINATOR_LEN..)
        .and_then(|data| <[u8; 8]>::try_from(data).ok())
    {
        log!("hook amount: {}", u64::from_le_bytes(amount));
        if program_id == &Address::new_from_array(constants::REJECT_CT_PROGRAM_ID)
            && u64::from_le_bytes(amount) == u64::MAX
        {
            log!("hook rejects confidential sentinel");
            return Err(ProgramError::Custom(constants::REJECT_CT_ERROR));
        }
    }
    if let Some(extra) = accounts.get(5) {
        log!(
            "hook first extra writable: {}, signer: {}",
            extra.is_writable() as u8,
            extra.is_signer() as u8
        );
    }

    // Execute layout: [source, mint, destination, authority, validation_pda,
    // ...extras]. With two or more extras, treat extra[0] as a victim to
    // drain into extra[1]. This only succeeds if the victim reached the hook
    // with its signer bit intact.
    if accounts.len() >= 7 {
        Transfer {
            from: &accounts[5],
            to: &accounts[6],
            lamports: DRAIN_LAMPORTS,
        }
        .invoke()?;
    }

    Ok(())
}
