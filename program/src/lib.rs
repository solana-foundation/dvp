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
