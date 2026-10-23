use super::{ConfidentialError, EscrowKeys, AMOUNT_LO_BITS};
use crate::verify::{find_swap_dvp_escrow_ata, ConfidentialSwapDvp};
use curve25519_dalek::scalar::Scalar;
use solana_pubkey::Pubkey;
use solana_zk_sdk::encryption::{elgamal::ElGamalCiphertext, pedersen::G};
use spl_token_2022_interface::{
    extension::{
        confidential_transfer::ConfidentialTransferAccount, BaseStateWithExtensions,
        StateWithExtensions,
    },
    state::Account,
};

/// The snapshot and history must refer to the same bank/commitment. A recovered
/// number is accepted only when it reproduces the on-chain ElGamal plaintext point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EscrowBalance {
    pub available: u64,
    pub pending: u64,
    pub pending_credit_counter: u64,
    pub public: u64,
}

pub fn read_escrow_account(
    address: &Pubkey,
    owner: &Pubkey,
    data: &[u8],
    swap: &Pubkey,
    mint: &Pubkey,
) -> Result<(ConfidentialTransferAccount, u64), ConfidentialError> {
    let token = spl_token_2022_interface::ID;
    if *owner != token || *address != find_swap_dvp_escrow_ata(swap, mint, &token) {
        return Err(ConfidentialError::Account("escrow owner or address"));
    }
    let account = StateWithExtensions::<Account>::unpack(data)
        .map_err(|_| ConfidentialError::Account("token layout"))?;
    if account.base.owner != *swap || account.base.mint != *mint {
        return Err(ConfidentialError::Account("escrow authority or mint"));
    }
    let ct = *account
        .get_extension::<ConfidentialTransferAccount>()
        .map_err(|_| ConfidentialError::Account("missing CT extension"))?;
    Ok((ct, account.base.amount))
}

pub fn verify_confidential_swap(
    swap: &ConfidentialSwapDvp,
    escrow: &ConfidentialTransferAccount,
    keys: &EscrowKeys,
    expected_amount_b: u64,
) -> Result<(), ConfidentialError> {
    if swap.base.token_program_b != spl_token_2022_interface::ID {
        return Err(ConfidentialError::Account("leg B token program"));
    }
    check_escrow_keys(escrow, keys)?;
    if keys.encrypt_amount(expected_amount_b)? != swap.amount_b {
        return Err(ConfidentialError::AmountMismatch);
    }
    if !bool::from(escrow.approved) {
        return Err(ConfidentialError::Account("escrow requires mint approval"));
    }
    if !bool::from(escrow.allow_confidential_credits) {
        return Err(ConfidentialError::Account("confidential credits disabled"));
    }
    checked_available_balance(escrow, keys)?;
    Ok(())
}

pub fn check_escrow_keys(
    state: &ConfidentialTransferAccount,
    keys: &EscrowKeys,
) -> Result<(), ConfidentialError> {
    if state.elgamal_pubkey != (*keys.elgamal.pubkey()).into() {
        return Err(ConfidentialError::EscrowKeyMismatch);
    }
    Ok(())
}

/// Compare a u64 candidate directly with the decrypted group point. This does
/// not solve a discrete logarithm and works above the SDK's u32 decode limit.
pub fn ciphertext_matches(ciphertext: &ElGamalCiphertext, keys: &EscrowKeys, amount: u64) -> bool {
    ciphertext.decrypt(keys.elgamal.secret()).target == Scalar::from(amount) * G
}

pub fn checked_available_balance(
    state: &ConfidentialTransferAccount,
    keys: &EscrowKeys,
) -> Result<u64, ConfidentialError> {
    check_escrow_keys(state, keys)?;
    let ae = state
        .decryptable_available_balance
        .try_into()
        .map_err(|_| ConfidentialError::BalanceMismatch)?;
    let amount = keys
        .ae
        .decrypt(&ae)
        .ok_or(ConfidentialError::BalanceMismatch)?;
    let ciphertext = state
        .available_balance
        .try_into()
        .map_err(|_| ConfidentialError::BalanceMismatch)?;
    if !ciphertext_matches(&ciphertext, keys, amount) {
        return Err(ConfidentialError::BalanceMismatch);
    }
    Ok(amount)
}

/// Successful Token-2022 balance operations in execution order, including CPIs.
/// Transfer limbs use the escrow's source/destination handles from the verified
/// grouped-ciphertext context, not the auditor ciphertexts in Transfer's arguments.
#[derive(Clone, Debug)]
pub enum BalanceEvent {
    Credit {
        lo: ElGamalCiphertext,
        hi: ElGamalCiphertext,
    },
    Debit {
        lo: ElGamalCiphertext,
        hi: ElGamalCiphertext,
    },
    Deposit(u64),
    Withdraw(u64),
    Apply,
    Empty,
}

fn transfer_amount(
    lo: &ElGamalCiphertext,
    hi: &ElGamalCiphertext,
    keys: &EscrowKeys,
) -> Result<u64, ConfidentialError> {
    let lo = lo
        .decrypt_u32(keys.elgamal.secret())
        .ok_or(ConfidentialError::IncompleteHistory)?;
    let hi = hi
        .decrypt_u32(keys.elgamal.secret())
        .ok_or(ConfidentialError::IncompleteHistory)?;
    if lo >= 1 << AMOUNT_LO_BITS {
        return Err(ConfidentialError::IncompleteHistory);
    }
    Ok(lo + (hi << AMOUNT_LO_BITS))
}

/// Replay from the zero state created by ConfigureAccount. The caller supplies
/// successful, ordered history up to this snapshot; missing or reordered events
/// cannot supply a different balance because all three ciphertexts are checked.
pub fn recover_escrow_balance(
    state: &ConfidentialTransferAccount,
    public: u64,
    keys: &EscrowKeys,
    history: &[BalanceEvent],
) -> Result<EscrowBalance, ConfidentialError> {
    check_escrow_keys(state, keys)?;
    let (mut available, mut pending_lo, mut pending_hi, mut counter) = (0u64, 0u64, 0u64, 0u64);
    for event in history {
        match event {
            BalanceEvent::Credit { lo, hi } => {
                let amount = transfer_amount(lo, hi, keys)?;
                pending_lo = pending_lo
                    .checked_add(amount & ((1 << AMOUNT_LO_BITS) - 1))
                    .ok_or(ConfidentialError::Arithmetic)?;
                pending_hi = pending_hi
                    .checked_add(amount >> AMOUNT_LO_BITS)
                    .ok_or(ConfidentialError::Arithmetic)?;
                counter = counter
                    .checked_add(1)
                    .ok_or(ConfidentialError::Arithmetic)?;
            }
            BalanceEvent::Deposit(amount) => {
                if *amount > super::MAX_TRANSFER_AMOUNT {
                    return Err(ConfidentialError::IncompleteHistory);
                }
                pending_lo = pending_lo
                    .checked_add(amount & ((1 << AMOUNT_LO_BITS) - 1))
                    .ok_or(ConfidentialError::Arithmetic)?;
                pending_hi = pending_hi
                    .checked_add(amount >> AMOUNT_LO_BITS)
                    .ok_or(ConfidentialError::Arithmetic)?;
                counter = counter
                    .checked_add(1)
                    .ok_or(ConfidentialError::Arithmetic)?;
            }
            BalanceEvent::Debit { lo, hi } => {
                available = available
                    .checked_sub(transfer_amount(lo, hi, keys)?)
                    .ok_or(ConfidentialError::Arithmetic)?
            }
            BalanceEvent::Withdraw(amount) => {
                available = available
                    .checked_sub(*amount)
                    .ok_or(ConfidentialError::Arithmetic)?
            }
            BalanceEvent::Apply => {
                available = available
                    .checked_add(combine(pending_lo, pending_hi)?)
                    .ok_or(ConfidentialError::Arithmetic)?;
                pending_lo = 0;
                pending_hi = 0;
                counter = 0;
            }
            BalanceEvent::Empty => {
                if available != 0 || pending_lo != 0 || pending_hi != 0 {
                    return Err(ConfidentialError::IncompleteHistory);
                }
            }
        }
    }
    for (ciphertext, amount) in [
        (state.available_balance, available),
        (state.pending_balance_lo, pending_lo),
        (state.pending_balance_hi, pending_hi),
    ] {
        let ciphertext = ciphertext
            .try_into()
            .map_err(|_| ConfidentialError::IncompleteHistory)?;
        if !ciphertext_matches(&ciphertext, keys, amount) {
            return Err(ConfidentialError::IncompleteHistory);
        }
    }
    if counter != u64::from(state.pending_balance_credit_counter) {
        return Err(ConfidentialError::IncompleteHistory);
    }
    Ok(EscrowBalance {
        available,
        pending: combine(pending_lo, pending_hi)?,
        pending_credit_counter: counter,
        public,
    })
}

fn combine(lo: u64, hi: u64) -> Result<u64, ConfidentialError> {
    hi.checked_mul(1 << AMOUNT_LO_BITS)
        .and_then(|v| v.checked_add(lo))
        .ok_or(ConfidentialError::Arithmetic)
}

/// Read only the available balance for outgoing transfers. Pending credits do
/// not affect their proofs and need not be decoded when AE matches ElGamal.
pub fn read_available_balance(
    state: &ConfidentialTransferAccount,
    keys: &EscrowKeys,
    history: &[BalanceEvent],
) -> Result<u64, ConfidentialError> {
    check_escrow_keys(state, keys)?;
    checked_available_balance(state, keys).or_else(|initial| {
        recover_escrow_balance(state, 0, keys, history)
            .map(|b| b.available)
            .map_err(|history| ConfidentialError::BalanceRecoveryFailed {
                initial: Box::new(initial),
                history: Box::new(history),
            })
    })
}

/// Uses AE only after verifying its value against ElGamal. Falls back to history
/// for a damaged or deliberately false AE balance, or pending sums above u32.
pub fn read_escrow_balance(
    state: &ConfidentialTransferAccount,
    public: u64,
    keys: &EscrowKeys,
    history: &[BalanceEvent],
) -> Result<EscrowBalance, ConfidentialError> {
    check_escrow_keys(state, keys)?;
    let initial = match checked_available_balance(state, keys) {
        Ok(available) => {
            let lo: ElGamalCiphertext = state
                .pending_balance_lo
                .try_into()
                .map_err(|_| ConfidentialError::Account("pending ciphertext"))?;
            let hi: ElGamalCiphertext = state
                .pending_balance_hi
                .try_into()
                .map_err(|_| ConfidentialError::Account("pending ciphertext"))?;
            if let (Some(lo), Some(hi)) = (
                lo.decrypt_u32(keys.elgamal.secret()),
                hi.decrypt_u32(keys.elgamal.secret()),
            ) {
                return Ok(EscrowBalance {
                    available,
                    pending: combine(lo, hi)?,
                    pending_credit_counter: state.pending_balance_credit_counter.into(),
                    public,
                });
            }
            ConfidentialError::IncompleteHistory
        }
        Err(error) => error,
    };
    recover_escrow_balance(state, public, keys, history).map_err(|history| {
        ConfidentialError::BalanceRecoveryFailed {
            initial: Box::new(initial),
            history: Box::new(history),
        }
    })
}
