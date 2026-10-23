//! Confidential DvP keys, verified balances and unsigned transaction sessions.
//! Sending, retries and shared-seed delivery belong to the caller.

mod balance;
mod history;
mod keys;
mod lifecycle;
mod transaction;
pub use crate::ciphertext::AmountCiphertexts;
pub use balance::{
    read_available_balance, read_escrow_account, read_escrow_balance, recover_escrow_balance,
    verify_confidential_swap, BalanceEvent, EscrowBalance,
};
pub use history::{BalanceHistory, ExecutedTransaction};
pub use keys::{derive_shared_seed, EscrowKeys};
pub use lifecycle::{
    apply_session, create_session, mint_auditor, refund_session,
    resolve_confidential_hook_accounts, settle_session, MintAccount, RecipientAccount,
    RefundAmount, RefundInstruction, RefundRequest, SettleRequest, TransferSource,
};
pub use transaction::{PlannedTransaction, SessionConfig, TransactionFormat, TransactionSession};

use balance::check_escrow_keys;
#[cfg(test)]
use balance::{checked_available_balance, ciphertext_matches};
use transaction::RECORD_PROGRAM_ID;

/// Helpers for this crate's integration tests, outside the integrator API.
#[cfg(any(test, feature = "test-utils"))]
#[doc(hidden)]
pub mod test_utils;

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub enum ConfidentialError {
    #[error("amount B must be between 1 and 2^48 - 1")]
    InvalidAmount,
    #[error("transfer amount {0} exceeds the 2^48 - 1 limit")]
    TransferAmountTooLarge(u64),
    #[error("partial refund amount must be positive")]
    ZeroPartialRefund,
    #[error("available balance {available} is less than the required {required}")]
    InsufficientAvailable { available: u64, required: u64 },
    #[error("surplus {0} exceeds the 2^48 - 1 transfer limit; reclaim part first")]
    SurplusTooLarge(u64),
    #[error("recipient has not been approved")]
    RecipientNotApproved,
    #[error("recipient confidential credits are disabled")]
    RecipientCreditsDisabled,
    #[error("recipient pending counter cannot accept {required} more credits")]
    RecipientPendingCounterFull { required: u64 },
    #[error("invalid confidential account: {0}")]
    Account(&'static str),
    #[error("derived ElGamal key does not match the escrow")]
    EscrowKeyMismatch,
    #[error("expected amount does not match the swap ciphertexts")]
    AmountMismatch,
    #[error("the decryptable balance is invalid or does not match the ElGamal balance")]
    BalanceMismatch,
    #[error("transaction history is incomplete or does not match the escrow snapshot")]
    IncompleteHistory,
    #[error("balance recovery failed: {history}; initial read failed: {initial}")]
    BalanceRecoveryFailed {
        #[source]
        initial: Box<ConfidentialError>,
        history: Box<ConfidentialError>,
    },
    #[error("balance arithmetic overflow or underflow")]
    Arithmetic,
    #[error("proof generation failed: {0}")]
    Proof(String),
    #[error("transaction preparation failed: {0}")]
    Transaction(String),
    #[error("transaction size {actual} exceeds the {limit}-byte limit")]
    TransactionTooLarge { actual: usize, limit: usize },
    #[error("hooked Settle requires v1")]
    HookedSettleRequiresV1,
    #[error("swap verification failed: {0}")]
    Swap(#[from] crate::verify::SwapDvpVerifyError),
}

const AMOUNT_LO_BITS: usize = 16;
pub const MAX_TRANSFER_AMOUNT: u64 = (1 << 48) - 1;
