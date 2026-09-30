#![no_std]

pub mod discriminator;
pub mod error;
// pub mod events;
pub mod instructions;
pub mod processor;
pub mod state;

#[cfg(not(feature = "no-entrypoint"))]
pub mod entrypoint;

use pinocchio::address::declare_id;
declare_id!("dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq");

#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    name: "DvP Swap Program",
    project_url: "https://github.com/solana-foundation/dvp",
    contacts: "link:https://github.com/solana-foundation/dvp/security/advisories/new",
    policy: "https://github.com/solana-foundation/dvp/security/policy",
    source_code: "https://github.com/solana-foundation/dvp",
    auditors: "Cantina"
}
