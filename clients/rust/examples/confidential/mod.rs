//! Copyable integration recipes. Build with `cargo build -p dvp-swap-program-client
//! --example confidential --features confidential,fetch`.
//!
//! Call create, verify/fund using your Token-2022 wallet, apply, then settle.
//! Fetch fresh snapshots after each confirmed operation. Refunds are an alternative
//! to settlement; cleanup is only for abandoned preparations. See rust-client.md.
pub mod create_apply;
pub mod refunds;
pub mod send;
pub mod settle;
