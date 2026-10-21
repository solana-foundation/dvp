extern crate alloc;

use crate::error::DvpSwapProgramError;
use alloc::vec::Vec;
use codama::CodamaAccount;
use pinocchio::{cpi::Seed, error::ProgramError, Address as Pubkey};

pub const SWAP_DVP_SEED: &[u8] = b"dvp";

/// Seed prefix for the per-DvP nonce tombstone. The tombstone PDA is
/// derived from `[NONCE_TOMBSTONE_SEED, swap_dvp_pubkey]`, created at
/// CreateDvp, and never closed — its existence permanently marks a
/// `(seeds, nonce)` PDA address as used so it can't be re-instantiated.
pub const NONCE_TOMBSTONE_SEED: &[u8] = b"nonce";

/// Max byte length of `ref_string` (the stored buffer size).
pub const MAX_REF_STRING_LEN: usize = 64;

/// Atomic DvP escrow for a P2P token swap.
///
/// `user_a` (seller) delivers `amount_a` of `mint_a` (the asset);
/// `user_b` (buyer) delivers `amount_b` of `mint_b` (the cash). Only the
/// `settlement_authority` can settle; either party (or the authority)
/// can abort before settlement.
///
/// Seeds: `[b"dvp", settlement_authority, user_a, user_b, mint_a, mint_b,
/// nonce.to_le_bytes(), bump]`. `bump` is derived by `find_program_address`
/// at create time and stored on the account so post-create instructions
/// can re-sign as this PDA without re-running the derivation.
#[derive(Clone, Debug, PartialEq, CodamaAccount)]
#[repr(C)]
pub struct SwapDvp {
    pub bump: u8,
    pub user_a: Pubkey,
    pub user_b: Pubkey,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub settlement_authority: Pubkey,
    /// Owner of `mint_a`, captured at Create.
    pub token_program_a: Pubkey,
    /// Owner of `mint_b`, captured at Create.
    pub token_program_b: Pubkey,
    pub amount_a: u64,
    pub amount_b: u64,
    /// Cluster time (`Clock::unix_timestamp`), not wall-clock. See README.
    pub expiry_timestamp: i64,
    pub nonce: u64,
    /// Opaque client reference (e.g. an off-chain order ID), stored as
    /// UTF-8 zero-padded to the right (`MAX_REF_STRING_LEN` bytes);
    /// clients trim trailing zeros to recover the string. The program
    /// never reads it.
    pub ref_string: [u8; 64],
    /// Wallet receiving user_a's settlement proceeds — the cash leg
    /// (`mint_b`) — at its canonical ATA. Resolved at Create: the
    /// optional instruction arg defaults to `user_a`, so Settle never
    /// branches. Only Settle reads this; refunds (Reclaim/Cancel/
    /// Reject and Settle surplus) always go to the depositor.
    pub user_a_settlement_destination: Pubkey,
    /// Wallet receiving user_b's settlement proceeds — the asset leg
    /// (`mint_a`). Defaults to `user_b`; same rules as above.
    pub user_b_settlement_destination: Pubkey,
    /// `mint_a`'s mint authority captured at Create, or the default (all
    /// zero) pubkey when the mint has none. Settle rejects if it no longer
    /// matches, so a leg can't gain a fresh authority post-consent.
    pub mint_a_authority: Pubkey,
    /// `mint_b`'s mint authority captured at Create. Same rule as above.
    pub mint_b_authority: Pubkey,
    /// `None` = settlement allowed any time before `expiry_timestamp`.
    /// `Some(t)` = additionally requires `now >= t`. Kept last: it is the
    /// only variable-width Borsh option, so its fixed on-chain sentinel is
    /// trailing and the generated client decoder can ignore it.
    pub earliest_settlement_timestamp: Option<i64>,
}

impl SwapDvp {
    pub const LEN: usize = 1   // bump
        + 32 * 7               // user_a, user_b, mint_a, mint_b, settlement_authority, token_program_a, token_program_b
        + 8 * 4                // amount_a, amount_b, expiry_timestamp, nonce
        + MAX_REF_STRING_LEN   // ref_string (zero-padded)
        + 32 * 2               // user_a_settlement_destination, user_b_settlement_destination
        + 32 * 2               // mint_a_authority, mint_b_authority
        + 1 + 8; // earliest_settlement_timestamp (opt)

    /// Public instructions accept only the original, fixed-size layout.
    pub fn load(data: &[u8]) -> Result<Self, ProgramError> {
        check_layout_len(data.len(), Self::LEN)?;
        Self::decode_base(data)
    }

    /// Owned `(nonce, bump)` byte buffers. Bind to a local so
    /// `signing_seeds` can borrow from them across the CPI.
    pub fn seed_buffers(&self) -> ([u8; 8], [u8; 1]) {
        (self.nonce.to_le_bytes(), [self.bump])
    }

    /// PDA seed array (with bump) for signing CPIs as the SwapDvp authority.
    pub fn signing_seeds<'a>(
        &'a self,
        nonce_bytes: &'a [u8; 8],
        bump_bytes: &'a [u8; 1],
    ) -> [Seed<'a>; 8] {
        [
            Seed::from(SWAP_DVP_SEED),
            Seed::from(self.settlement_authority.as_ref()),
            Seed::from(self.user_a.as_ref()),
            Seed::from(self.user_b.as_ref()),
            Seed::from(self.mint_a.as_ref()),
            Seed::from(self.mint_b.as_ref()),
            Seed::from(nonce_bytes),
            Seed::from(bump_bytes),
        ]
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(Self::LEN);
        data.push(self.bump);
        data.extend_from_slice(self.user_a.as_ref());
        data.extend_from_slice(self.user_b.as_ref());
        data.extend_from_slice(self.mint_a.as_ref());
        data.extend_from_slice(self.mint_b.as_ref());
        data.extend_from_slice(self.settlement_authority.as_ref());
        data.extend_from_slice(self.token_program_a.as_ref());
        data.extend_from_slice(self.token_program_b.as_ref());
        data.extend_from_slice(&self.amount_a.to_le_bytes());
        data.extend_from_slice(&self.amount_b.to_le_bytes());
        data.extend_from_slice(&self.expiry_timestamp.to_le_bytes());
        data.extend_from_slice(&self.nonce.to_le_bytes());
        data.extend_from_slice(&self.ref_string);
        data.extend_from_slice(self.user_a_settlement_destination.as_ref());
        data.extend_from_slice(self.user_b_settlement_destination.as_ref());
        data.extend_from_slice(self.mint_a_authority.as_ref());
        data.extend_from_slice(self.mint_b_authority.as_ref());

        match self.earliest_settlement_timestamp {
            Some(timestamp) => {
                data.push(1);
                data.extend_from_slice(&timestamp.to_le_bytes());
            }
            None => {
                data.push(0);
                data.extend_from_slice(&i64::MAX.to_le_bytes());
            }
        }

        data
    }

    fn decode_base(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != Self::LEN {
            return Err(ProgramError::InvalidAccountData);
        }

        let mut offset: usize = 0;

        let bump = data[offset];
        offset += 1;

        let user_a = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let user_b = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let mint_a = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let mint_b = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let settlement_authority = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let token_program_a = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let token_program_b = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let amount_a = u64::from_le_bytes(
            data[offset..offset + 8]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 8;

        let amount_b = u64::from_le_bytes(
            data[offset..offset + 8]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 8;

        let expiry_timestamp = i64::from_le_bytes(
            data[offset..offset + 8]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 8;

        let nonce = u64::from_le_bytes(
            data[offset..offset + 8]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 8;

        let ref_string: [u8; MAX_REF_STRING_LEN] = data[offset..offset + MAX_REF_STRING_LEN]
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        offset += MAX_REF_STRING_LEN;

        let user_a_settlement_destination = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let user_b_settlement_destination = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let mint_a_authority = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let mint_b_authority = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        // Tag is the source of truth; the payload after a `0` tag is a
        // sentinel (see `to_bytes`) and is intentionally not validated.
        let earliest_settlement_timestamp = match data[offset] {
            0 => None,
            1 => Some(i64::from_le_bytes(
                data[offset + 1..offset + 9]
                    .try_into()
                    .map_err(|_| ProgramError::InvalidAccountData)?,
            )),
            _ => return Err(ProgramError::InvalidAccountData),
        };

        Ok(Self {
            bump,
            user_a,
            user_b,
            mint_a,
            mint_b,
            settlement_authority,
            token_program_a,
            token_program_b,
            amount_a,
            amount_b,
            expiry_timestamp,
            nonce,
            ref_string,
            user_a_settlement_destination,
            user_b_settlement_destination,
            mint_a_authority,
            mint_b_authority,
            earliest_settlement_timestamp,
        })
    }
}

/// Program-side view only: the IDL continues to describe the public base.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfidentialSwapDvp {
    pub base: SwapDvp,
    pub amount_b_ciphertext_lo: [u8; 64],
    pub amount_b_ciphertext_hi: [u8; 64],
}

impl ConfidentialSwapDvp {
    pub const LEN: usize = SwapDvp::LEN + 128;

    /// The confidential tail follows the complete fixed-width public base.
    pub fn load(data: &[u8]) -> Result<Self, ProgramError> {
        check_layout_len(data.len(), Self::LEN)?;
        Ok(Self {
            base: SwapDvp::decode_base(&data[..SwapDvp::LEN])?,
            amount_b_ciphertext_lo: data[SwapDvp::LEN..SwapDvp::LEN + 64].try_into().unwrap(),
            amount_b_ciphertext_hi: data[SwapDvp::LEN + 64..].try_into().unwrap(),
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut base = self.base.clone();
        base.amount_b = u64::MAX;
        let mut data = base.to_bytes();
        data.extend_from_slice(&self.amount_b_ciphertext_lo);
        data.extend_from_slice(&self.amount_b_ciphertext_hi);
        data
    }
}

fn check_layout_len(actual: usize, expected: usize) -> Result<(), ProgramError> {
    match actual {
        len if len == expected => Ok(()),
        SwapDvp::LEN | ConfidentialSwapDvp::LEN => {
            Err(DvpSwapProgramError::SwapModeMismatch.into())
        }
        _ => Err(ProgramError::InvalidAccountData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_swap_dvp() -> SwapDvp {
        SwapDvp {
            bump: 254,
            user_a: Pubkey::new_from_array([1u8; 32]),
            user_b: Pubkey::new_from_array([2u8; 32]),
            mint_a: Pubkey::new_from_array([3u8; 32]),
            mint_b: Pubkey::new_from_array([4u8; 32]),
            settlement_authority: Pubkey::new_from_array([5u8; 32]),
            token_program_a: Pubkey::new_from_array([6u8; 32]),
            token_program_b: Pubkey::new_from_array([7u8; 32]),
            amount_a: 1_000,
            amount_b: 2_500,
            expiry_timestamp: 1_780_000_000,
            nonce: 42,
            ref_string: [8u8; 64],
            user_a_settlement_destination: Pubkey::new_from_array([9u8; 32]),
            user_b_settlement_destination: Pubkey::new_from_array([10u8; 32]),
            mint_a_authority: Pubkey::new_from_array([11u8; 32]),
            mint_b_authority: Pubkey::new_from_array([12u8; 32]),
            earliest_settlement_timestamp: None,
        }
    }

    #[test]
    fn test_fixed_layout_and_confidential_tail() {
        // Pin the protocol sizes independently of the implementation constants.
        assert_eq!(SwapDvp::LEN, 458);
        assert_eq!(ConfidentialSwapDvp::LEN, 586);

        for earliest in [None, Some(1_770_000_000i64)] {
            let base = SwapDvp {
                earliest_settlement_timestamp: earliest,
                ..test_swap_dvp()
            };
            // Independent wire fixture in field order, without using to_bytes.
            let bytes = [
                &[254u8][..],                                // bump
                &[1u8; 32],                                  // user_a
                &[2u8; 32],                                  // user_b
                &[3u8; 32],                                  // mint_a
                &[4u8; 32],                                  // mint_b
                &[5u8; 32],                                  // settlement_authority
                &[6u8; 32],                                  // token_program_a
                &[7u8; 32],                                  // token_program_b
                &1_000u64.to_le_bytes(),                     // amount_a
                &2_500u64.to_le_bytes(),                     // amount_b
                &1_780_000_000i64.to_le_bytes(),             // expiry_timestamp
                &42u64.to_le_bytes(),                        // nonce
                &[8u8; 64],                                  // ref_string
                &[9u8; 32],                                  // user_a_settlement_destination
                &[10u8; 32],                                 // user_b_settlement_destination
                &[11u8; 32],                                 // mint_a_authority
                &[12u8; 32],                                 // mint_b_authority
                &[u8::from(earliest.is_some())],             // earliest option tag
                &earliest.unwrap_or(i64::MAX).to_le_bytes(), // earliest value/sentinel
            ]
            .concat();
            assert_eq!(SwapDvp::load(&bytes).unwrap(), base);
            assert_eq!(base.to_bytes(), bytes);

            let confidential = ConfidentialSwapDvp {
                base,
                amount_b_ciphertext_lo: [13u8; 64],
                amount_b_ciphertext_hi: [14u8; 64],
            };
            // amount_b follows bump, seven pubkeys and amount_a.
            let amount_b_offset = 1 + 32 * 7 + 8;
            let amount_b_end = amount_b_offset + core::mem::size_of::<u64>();
            let mut expected_base = bytes.clone();
            expected_base[amount_b_offset..amount_b_end].copy_from_slice(&u64::MAX.to_le_bytes());

            // Independent confidential fixture: base with sentinel, then both ciphertexts.
            let confidential_bytes = [
                expected_base.as_slice(), // base with amount_b = u64::MAX
                &[13u8; 64],              // amount_b_ciphertext_lo
                &[14u8; 64],              // amount_b_ciphertext_hi
            ]
            .concat();
            assert_eq!(confidential_bytes.len(), ConfidentialSwapDvp::LEN);
            assert_eq!(confidential.to_bytes(), confidential_bytes);

            let decoded = ConfidentialSwapDvp::load(&confidential_bytes).unwrap();
            let mut expected = confidential;
            expected.base.amount_b = u64::MAX;
            assert_eq!(decoded, expected);
            assert_eq!(decoded.to_bytes(), confidential_bytes);
            assert_eq!(
                SwapDvp::load(&confidential_bytes),
                Err(DvpSwapProgramError::SwapModeMismatch.into())
            );
            assert_eq!(
                ConfidentialSwapDvp::load(&bytes),
                Err(DvpSwapProgramError::SwapModeMismatch.into())
            );
        }
    }

    #[test]
    fn test_loaders_reject_other_lengths_and_invalid_tags() {
        // Cover every truncated layout and the first oversized confidential layout.
        for len in 0..=ConfidentialSwapDvp::LEN + 1 {
            if matches!(len, SwapDvp::LEN | ConfidentialSwapDvp::LEN) {
                continue;
            }
            let bytes = alloc::vec![0; len];
            assert_eq!(SwapDvp::load(&bytes), Err(ProgramError::InvalidAccountData));
            assert_eq!(
                ConfidentialSwapDvp::load(&bytes),
                Err(ProgramError::InvalidAccountData)
            );
        }

        let confidential = ConfidentialSwapDvp {
            base: test_swap_dvp(),
            amount_b_ciphertext_lo: [13u8; 64],
            amount_b_ciphertext_hi: [14u8; 64],
        };
        let mut bytes = confidential.to_bytes();
        // The public base ends with an option tag followed by an i64 payload.
        let option_tag_offset = SwapDvp::LEN - 1 - core::mem::size_of::<i64>();
        bytes[option_tag_offset] = 2;
        assert_eq!(
            SwapDvp::load(&bytes[..SwapDvp::LEN]),
            Err(ProgramError::InvalidAccountData)
        );
        assert_eq!(
            ConfidentialSwapDvp::load(&bytes),
            Err(ProgramError::InvalidAccountData)
        );
    }

    /// Three-arm match on the option tag: 0 → None, 1 → Some, anything
    /// else → Err. The `_` arm is easy to drop in a refactor, and the
    /// failure mode (silently reading garbage past the tag as a valid
    /// `Some(_)`) is hard to spot in review.
    #[test]
    fn test_load_rejects_invalid_option_tag() {
        let dvp = test_swap_dvp();
        let mut bytes = dvp.to_bytes();
        // Tag offset = bump(1) + 7*pubkey(224) + 4*u64-or-i64(32)
        //   + ref_string(64) + 2*destination pubkey(64)
        //   + 2*authority pubkey(64) = 449.
        let option_tag_offset = 1 + 32 * 7 + 8 * 4 + MAX_REF_STRING_LEN + 32 * 2 + 32 * 2;
        bytes[option_tag_offset] = 2;
        let err = SwapDvp::load(&bytes).expect_err("must reject invalid tag");
        assert!(matches!(err, ProgramError::InvalidAccountData));
    }

    /// to_bytes -> load round-trips across the earliest-option
    /// None/Some cases and present/absent (default) mint authorities, where
    /// the layout could drift.
    #[test]
    fn test_to_bytes_load_roundtrip() {
        let base = test_swap_dvp();

        let key = Pubkey::new_from_array([11u8; 32]);
        let default = Pubkey::default();
        let cases = [
            (None, default, default),
            (Some(1_780_000_000), key, default),
            (None, default, key),
            (Some(1_780_000_000), key, key),
        ];

        for (earliest, auth_a, auth_b) in cases {
            let dvp = SwapDvp {
                earliest_settlement_timestamp: earliest,
                mint_a_authority: auth_a,
                mint_b_authority: auth_b,
                ..base.clone()
            };
            let bytes = dvp.to_bytes();
            assert_eq!(bytes.len(), SwapDvp::LEN);
            assert_eq!(SwapDvp::load(&bytes).unwrap(), dvp);
        }
    }
}
