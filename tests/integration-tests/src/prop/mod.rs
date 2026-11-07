//! Property-based tests for the DvP swap program.
//!
//! Each property drives the program on LiteSVM with generated sequences and
//! compares every step with a small reference model. Reuses the `utils` /
//! `state_utils` helpers of this crate.

mod coverage;
mod extensions;
mod helpers;
mod legs;
mod lifecycle;
mod permissions_time;
mod recover;
