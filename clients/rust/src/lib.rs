// Generated derives refer to optional serde support not provided by this crate.
#![allow(unexpected_cfgs)]

// Re-export generated code
#[allow(warnings)]
pub mod generated;
pub use generated::*;

// Handwritten checked, verify-before-fund helpers (survive client
// regeneration; the generated readers do not check owner or exact size).
pub mod ciphertext;
#[cfg(feature = "confidential")]
pub mod confidential;
pub mod verify;

#[cfg(test)]
mod cpi_flag_regression;

// Re-export commonly used items
pub use generated::errors::*;
pub use generated::programs::*;
