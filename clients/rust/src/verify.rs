//! Checked, verify-before-fund helpers for the DvP swap client.
//!
//! The generated readers Borsh-decode any 458-byte account with no owner or
//! PDA check. Escrows are funded by raw token transfers to an ATA derived
//! from the `swap_dvp` pubkey, so an attacker-owned account that decodes as a
//! `SwapDvp` lets a funder deposit into an ATA the attacker can drain.
//!
//! These helpers require, before treating an account as a canonical `SwapDvp`:
//! owner is the DvP program, size is a supported public/confidential layout, and the
//! address is the canonical PDA for the decoded terms. [`find_swap_dvp_address`]
//! and [`find_swap_dvp_escrow_ata`] derive those addresses from agreed terms.

use crate::accounts::SwapDvp;
use crate::generated::programs::DVP_SWAP_PROGRAM_ID;
use solana_pubkey::{pubkey, Pubkey};

/// Fixed on-chain size of a `SwapDvp` account (`SwapDvp::LEN`). The final
/// `earliest_settlement_timestamp` is always 1 tag byte + 8 payload bytes,
/// even for `None` (whose payload is an ignored sentinel).
pub const SWAP_DVP_ACCOUNT_LEN: usize = 1  // bump
    + 32 * 7   // user_a, user_b, mint_a, mint_b, settlement_authority, token_program_a, token_program_b
    + 8 * 4    // amount_a, amount_b, expiry_timestamp, nonce
    + 64       // ref_string
    + 32 * 2   // user_a_settlement_destination, user_b_settlement_destination
    + 32 * 2   // mint_a_authority, mint_b_authority
    + 1 + 8; // earliest_settlement_timestamp (tag + payload)

/// Public base followed by the two amount B ciphertexts.
pub const CONFIDENTIAL_SWAP_DVP_ACCOUNT_LEN: usize =
    SWAP_DVP_ACCOUNT_LEN + 2 * crate::ciphertext::ELGAMAL_CIPHERTEXT_LEN;

#[derive(Clone, Debug, PartialEq)]
pub struct ConfidentialSwapDvp {
    pub base: SwapDvp,
    pub amount_b: crate::ciphertext::AmountCiphertexts,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SwapDvpAccount {
    Public(SwapDvp),
    Confidential(ConfidentialSwapDvp),
}

impl SwapDvpAccount {
    /// Shared terms. For a confidential swap, base.amount_b is a sentinel;
    /// use the seed and agreed amount with verify_confidential_swap instead.
    pub fn base(&self) -> &SwapDvp {
        match self {
            Self::Public(base) => base,
            Self::Confidential(state) => &state.base,
        }
    }

    /// Terms of a public swap, or `None` for a confidential one. Use this,
    /// not [`Self::base`], when a public-only integration decides whether to
    /// fund.
    pub fn public(&self) -> Option<&SwapDvp> {
        match self {
            Self::Public(base) => Some(base),
            Self::Confidential(_) => None,
        }
    }
}

/// Seed prefix for the `SwapDvp` PDA (matches `SWAP_DVP_SEED` on-chain).
pub const SWAP_DVP_SEED: &[u8] = b"dvp";

/// Canonical SPL Associated Token Account program.
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

/// Token programs the DvP program accepts for either leg.
const SPL_TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const TOKEN_2022_PROGRAM_ID: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

/// Why a fetched account was rejected as a canonical `SwapDvp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapDvpVerifyError {
    /// Account is not owned by the DvP program.
    WrongOwner { expected: Pubkey, actual: Pubkey },
    /// Account data is not exactly [`SWAP_DVP_ACCOUNT_LEN`] bytes.
    WrongSize { expected: usize, actual: usize },
    /// Neither supported layout length matched.
    UnsupportedSize { actual: usize },
    /// Account data could not be parsed as a `SwapDvp`.
    Malformed(String),
    /// Account is program-owned but not at its canonical PDA for the
    /// decoded terms.
    WrongAddress { expected: Pubkey, actual: Pubkey },
}

impl core::fmt::Display for SwapDvpVerifyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SwapDvpVerifyError::WrongOwner { expected, actual } => write!(
                f,
                "account is not owned by the DvP program (expected owner {expected}, got {actual})"
            ),
            SwapDvpVerifyError::WrongSize { expected, actual } => write!(
                f,
                "account has {actual} bytes of data; a canonical SwapDvp is exactly {expected} bytes"
            ),
            SwapDvpVerifyError::UnsupportedSize { actual } => write!(f,
                "account has {actual} bytes; expected {SWAP_DVP_ACCOUNT_LEN} or {CONFIDENTIAL_SWAP_DVP_ACCOUNT_LEN}"),
            SwapDvpVerifyError::Malformed(msg) => {
                write!(f, "account data is not a valid SwapDvp: {msg}")
            }
            SwapDvpVerifyError::WrongAddress { expected, actual } => write!(
                f,
                "account {actual} is program-owned but not at its canonical PDA for the stored terms (expected address {expected})"
            ),
        }
    }
}

impl std::error::Error for SwapDvpVerifyError {}

impl SwapDvp {
    /// Parse an on-chain `SwapDvp`, rejecting any buffer that is not exactly
    /// [`SWAP_DVP_ACCOUNT_LEN`] bytes.
    ///
    /// The size gate is what makes reusing the generated Borsh decoder safe.
    /// The on-chain layout is fixed-width, but Borsh's `Option` is not (`None`
    /// is 1 byte, `Some` is 9), and `from_bytes` ignores trailing bytes, so on
    /// its own it can't tell the real 458-byte account from the 450-byte
    /// forgery. Pinned to 458 bytes, Borsh decodes `None` and `Some`
    /// unambiguously and still rejects an invalid option tag.
    pub fn try_from_bytes(data: &[u8]) -> Result<Self, SwapDvpVerifyError> {
        if data.len() != SWAP_DVP_ACCOUNT_LEN {
            return Err(SwapDvpVerifyError::WrongSize {
                expected: SWAP_DVP_ACCOUNT_LEN,
                actual: data.len(),
            });
        }

        SwapDvp::from_bytes(data).map_err(|e| SwapDvpVerifyError::Malformed(e.to_string()))
    }
}

/// Derives the canonical `SwapDvp` PDA from agreed terms (on-chain seeds
/// `[b"dvp", settlement_authority, user_a, user_b, mint_a, mint_b, nonce_le]`).
/// Compare this against any address a counterparty supplies.
pub fn find_swap_dvp_address(
    settlement_authority: &Pubkey,
    user_a: &Pubkey,
    user_b: &Pubkey,
    mint_a: &Pubkey,
    mint_b: &Pubkey,
    nonce: u64,
) -> (Pubkey, u8) {
    let nonce_bytes = nonce.to_le_bytes();
    Pubkey::find_program_address(
        &[
            SWAP_DVP_SEED,
            settlement_authority.as_ref(),
            user_a.as_ref(),
            user_b.as_ref(),
            mint_a.as_ref(),
            mint_b.as_ref(),
            &nonce_bytes,
        ],
        &DVP_SWAP_PROGRAM_ID,
    )
}

/// Derives a leg's escrow ATA (the `SwapDvp` PDA's ATA for a mint/token
/// program). Funders send raw transfers here, so derive it from a verified
/// `swap_dvp` PDA, not from an unverified supplied address.
pub fn find_swap_dvp_escrow_ata(
    swap_dvp: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Pubkey {
    Pubkey::find_program_address(
        &[swap_dvp.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

/// Verifies raw account fields (owner, size, layout, canonical PDA) and
/// returns the decoded mode and terms. Core used by `decode_swap_dvp_account`;
/// available without the `fetch` feature.
///
/// `data` must be the full account data. Do not pass data fetched with an
/// RPC `data_slice`: a 458-byte prefix of a confidential account decodes as
/// public.
pub fn verify_swap_dvp_bytes(
    expected_address: &Pubkey,
    owner: &Pubkey,
    data: &[u8],
) -> Result<SwapDvpAccount, SwapDvpVerifyError> {
    if *owner != DVP_SWAP_PROGRAM_ID {
        return Err(SwapDvpVerifyError::WrongOwner {
            expected: DVP_SWAP_PROGRAM_ID,
            actual: *owner,
        });
    }

    let state = match data.len() {
        SWAP_DVP_ACCOUNT_LEN => SwapDvpAccount::Public(SwapDvp::try_from_bytes(data)?),
        CONFIDENTIAL_SWAP_DVP_ACCOUNT_LEN => {
            let base = SwapDvp::try_from_bytes(&data[..SWAP_DVP_ACCOUNT_LEN])?;
            if base.amount_b != u64::MAX {
                return Err(SwapDvpVerifyError::Malformed(
                    "confidential amount B sentinel".into(),
                ));
            }
            let tail = &data[SWAP_DVP_ACCOUNT_LEN..];
            let (lo, hi) = tail.split_at(crate::ciphertext::ELGAMAL_CIPHERTEXT_LEN);
            SwapDvpAccount::Confidential(ConfidentialSwapDvp {
                base,
                amount_b: crate::ciphertext::AmountCiphertexts {
                    lo: lo.try_into().unwrap(),
                    hi: hi.try_into().unwrap(),
                },
            })
        }
        actual => return Err(SwapDvpVerifyError::UnsupportedSize { actual }),
    };
    let dvp = state.base();

    for (leg, program) in [("A", &dvp.token_program_a), ("B", &dvp.token_program_b)] {
        if *program != SPL_TOKEN_PROGRAM_ID && *program != TOKEN_2022_PROGRAM_ID {
            return Err(SwapDvpVerifyError::Malformed(format!(
                "unsupported token program {leg}: {program}"
            )));
        }
    }

    let (expected_pda, _bump) = find_swap_dvp_address(
        &dvp.settlement_authority,
        &dvp.user_a,
        &dvp.user_b,
        &dvp.mint_a,
        &dvp.mint_b,
        dvp.nonce,
    );
    if expected_pda != *expected_address {
        return Err(SwapDvpVerifyError::WrongAddress {
            expected: expected_pda,
            actual: *expected_address,
        });
    }

    Ok(state)
}

/// Fetched-account wrapper over [`verify_swap_dvp_bytes`]. Use instead of the
/// generated `SwapDvp::from_bytes` before acting on terms.
#[cfg(feature = "fetch")]
pub fn decode_swap_dvp_account(
    expected_address: &Pubkey,
    account: &solana_account::Account,
) -> Result<SwapDvpAccount, SwapDvpVerifyError> {
    verify_swap_dvp_bytes(expected_address, &account.owner, &account.data)
}

/// Check confidential funding state over fetched swap and escrow snapshots.
/// The caller must also compare the returned public terms with the agreed trade
/// before SPL wallet funding.
#[cfg(all(feature = "fetch", feature = "confidential"))]
pub fn verify_confidential_funding(
    swap_address: &Pubkey,
    swap_account: &solana_account::Account,
    escrow_address: &Pubkey,
    escrow_account: &solana_account::Account,
    keys: &crate::confidential::EscrowKeys,
    expected_amount_b: u64,
) -> Result<
    (
        ConfidentialSwapDvp,
        spl_token_2022_interface::extension::confidential_transfer::ConfidentialTransferAccount,
    ),
    crate::confidential::ConfidentialError,
> {
    use crate::confidential::{read_escrow_account, verify_confidential_swap, ConfidentialError};
    let SwapDvpAccount::Confidential(swap) = decode_swap_dvp_account(swap_address, swap_account)?
    else {
        return Err(ConfidentialError::Account("expected confidential swap"));
    };
    let (escrow, _) = read_escrow_account(
        escrow_address,
        &escrow_account.owner,
        &escrow_account.data,
        swap_address,
        &swap.base.mint_b,
    )?;
    verify_confidential_swap(&swap, &escrow, keys, expected_amount_b)?;
    Ok((swap, escrow))
}

/// Public terms of an agreed confidential trade. Destinations are resolved:
/// Create stores a user's own address when no destination is given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedConfidentialTerms {
    pub settlement_authority: Pubkey,
    pub user_a: Pubkey,
    pub user_b: Pubkey,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub token_program_a: Pubkey,
    pub amount_a: u64,
    pub amount_b: u64,
    pub nonce: u64,
    pub expiry_timestamp: i64,
    pub earliest_settlement_timestamp: Option<i64>,
    pub user_a_settlement_destination: Pubkey,
    pub user_b_settlement_destination: Pubkey,
}

/// Swap and escrow B checked against [`ExpectedConfidentialTerms`], with both
/// canonical escrow addresses. Fund these, not counterparty-supplied addresses.
#[cfg(all(feature = "fetch", feature = "confidential"))]
#[derive(Clone, Debug)]
pub struct VerifiedConfidentialTerms {
    pub swap_address: Pubkey,
    pub swap: ConfidentialSwapDvp,
    pub escrow_a: Pubkey,
    pub escrow_b: Pubkey,
    pub state:
        spl_token_2022_interface::extension::confidential_transfer::ConfidentialTransferAccount,
}

/// Funding and settlement gate: [`verify_confidential_funding`] at the swap
/// PDA and escrow B derived from `terms`, then every public term compared
/// with the agreed trade. The PDA covers the parties, mints, settlement
/// authority and nonce; the ciphertexts cover amount B.
#[cfg(all(feature = "fetch", feature = "confidential"))]
pub fn verify_confidential_terms(
    swap_account: &solana_account::Account,
    escrow_b_account: &solana_account::Account,
    keys: &crate::confidential::EscrowKeys,
    terms: &ExpectedConfidentialTerms,
) -> Result<VerifiedConfidentialTerms, crate::confidential::ConfidentialError> {
    use crate::confidential::ConfidentialError;
    let (swap_address, _) = find_swap_dvp_address(
        &terms.settlement_authority,
        &terms.user_a,
        &terms.user_b,
        &terms.mint_a,
        &terms.mint_b,
        terms.nonce,
    );
    let escrow_b = find_swap_dvp_escrow_ata(&swap_address, &terms.mint_b, &TOKEN_2022_PROGRAM_ID);
    let (swap, state) = verify_confidential_funding(
        &swap_address,
        swap_account,
        &escrow_b,
        escrow_b_account,
        keys,
        terms.amount_b,
    )?;
    let base = &swap.base;
    for (field, matches) in [
        (
            "token program A",
            base.token_program_a == terms.token_program_a,
        ),
        ("amount A", base.amount_a == terms.amount_a),
        ("expiry", base.expiry_timestamp == terms.expiry_timestamp),
        (
            "earliest settlement",
            base.earliest_settlement_timestamp == terms.earliest_settlement_timestamp,
        ),
        (
            "user A settlement destination",
            base.user_a_settlement_destination == terms.user_a_settlement_destination,
        ),
        (
            "user B settlement destination",
            base.user_b_settlement_destination == terms.user_b_settlement_destination,
        ),
    ] {
        if !matches {
            return Err(ConfidentialError::Account(field));
        }
    }
    Ok(VerifiedConfidentialTerms {
        swap_address,
        swap,
        escrow_a: find_swap_dvp_escrow_ata(&swap_address, &terms.mint_a, &terms.token_program_a),
        escrow_b,
        state,
    })
}

/// Query both exact account sizes. Every result is checked against its owner and
/// canonical PDA before being returned; unrelated or malformed accounts are skipped.
/// RPC failures still propagate. Fetch escrow snapshots separately for
/// verify_confidential_swap before funding.
#[cfg(feature = "fetch")]
pub fn fetch_swap_dvp_accounts(
    rpc: &solana_client::rpc_client::RpcClient,
) -> Result<Vec<(Pubkey, SwapDvpAccount)>, Box<dyn std::error::Error>> {
    use solana_client::{rpc_config::RpcProgramAccountsConfig, rpc_filter::RpcFilterType};
    let mut result = Vec::new();
    for size in [SWAP_DVP_ACCOUNT_LEN, CONFIDENTIAL_SWAP_DVP_ACCOUNT_LEN] {
        let config = RpcProgramAccountsConfig {
            filters: Some(vec![RpcFilterType::DataSize(size as u64)]),
            account_config: solana_client::rpc_config::RpcAccountInfoConfig {
                encoding: Some(solana_account_decoder_client_types::UiAccountEncoding::Base64),
                ..Default::default()
            },
            ..Default::default()
        };
        for (address, ui_account) in
            rpc.get_program_ui_accounts_with_config(&DVP_SWAP_PROGRAM_ID, config)?
        {
            let (Ok(owner), Some(data)) = (ui_account.owner.parse(), ui_account.data.decode())
            else {
                continue;
            };
            if let Ok(swap) = verify_swap_dvp_bytes(&address, &owner, &data) {
                result.push((address, swap));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshSerialize;

    fn sample() -> SwapDvp {
        SwapDvp {
            bump: 254,
            user_a: Pubkey::new_from_array([1u8; 32]),
            user_b: Pubkey::new_from_array([2u8; 32]),
            mint_a: Pubkey::new_from_array([3u8; 32]),
            mint_b: Pubkey::new_from_array([4u8; 32]),
            settlement_authority: Pubkey::new_from_array([5u8; 32]),
            token_program_a: SPL_TOKEN_PROGRAM_ID,
            token_program_b: TOKEN_2022_PROGRAM_ID,
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

    /// On-chain fixed layout: 1 tag byte + 8 payload bytes for the option,
    /// with `i64::MAX` as the None sentinel.
    fn on_chain_bytes(dvp: &SwapDvp) -> Vec<u8> {
        let mut data = Vec::with_capacity(SWAP_DVP_ACCOUNT_LEN);
        data.push(dvp.bump);
        for pk in [
            &dvp.user_a,
            &dvp.user_b,
            &dvp.mint_a,
            &dvp.mint_b,
            &dvp.settlement_authority,
            &dvp.token_program_a,
            &dvp.token_program_b,
        ] {
            data.extend_from_slice(pk.as_ref());
        }
        data.extend_from_slice(&dvp.amount_a.to_le_bytes());
        data.extend_from_slice(&dvp.amount_b.to_le_bytes());
        data.extend_from_slice(&dvp.expiry_timestamp.to_le_bytes());
        data.extend_from_slice(&dvp.nonce.to_le_bytes());
        data.extend_from_slice(&dvp.ref_string);
        data.extend_from_slice(dvp.user_a_settlement_destination.as_ref());
        data.extend_from_slice(dvp.user_b_settlement_destination.as_ref());
        data.extend_from_slice(dvp.mint_a_authority.as_ref());
        data.extend_from_slice(dvp.mint_b_authority.as_ref());
        match dvp.earliest_settlement_timestamp {
            Some(t) => {
                data.push(1);
                data.extend_from_slice(&t.to_le_bytes());
            }
            None => {
                data.push(0);
                data.extend_from_slice(&i64::MAX.to_le_bytes());
            }
        }
        data
    }

    #[test]
    fn strict_try_from_bytes_accepts_on_chain_none_layout() {
        let dvp = sample();
        let bytes = on_chain_bytes(&dvp);
        assert_eq!(bytes.len(), SWAP_DVP_ACCOUNT_LEN);
        let parsed = SwapDvp::try_from_bytes(&bytes).unwrap();
        assert_eq!(parsed, dvp);
    }

    #[test]
    fn strict_try_from_bytes_accepts_some() {
        let mut dvp = sample();
        dvp.earliest_settlement_timestamp = Some(1_770_000_000);
        let parsed = SwapDvp::try_from_bytes(&on_chain_bytes(&dvp)).unwrap();
        assert_eq!(parsed.earliest_settlement_timestamp, Some(1_770_000_000));
    }

    #[test]
    fn strict_try_from_bytes_rejects_borsh_none_forgery() {
        // The forgery: Borsh None is a lone `0` tag, 8 bytes short.
        let mut short = Vec::new();
        sample().serialize(&mut short).unwrap();
        assert_eq!(short.len(), 450);
        assert!(matches!(
            SwapDvp::try_from_bytes(&short),
            Err(SwapDvpVerifyError::WrongSize { .. })
        ));
    }

    #[test]
    fn generated_from_bytes_requires_the_public_layout() {
        // Patched by scripts/lib/patch-swap-dvp-exact-size.ts: the generated
        // reader must not return the base of a 586-byte confidential account.
        let bytes = on_chain_bytes(&sample());
        assert_eq!(SwapDvp::from_bytes(&bytes).unwrap(), sample());
        let mut confidential = bytes.clone();
        confidential.resize(CONFIDENTIAL_SWAP_DVP_ACCOUNT_LEN, 7);
        let mut short = Vec::new();
        sample().serialize(&mut short).unwrap();
        for data in [confidential, short] {
            assert_eq!(
                SwapDvp::from_bytes(&data).unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn strict_try_from_bytes_rejects_oversize() {
        let mut over = on_chain_bytes(&sample());
        over.push(0);
        assert!(matches!(
            SwapDvp::try_from_bytes(&over),
            Err(SwapDvpVerifyError::WrongSize { .. })
        ));
    }

    #[test]
    fn strict_try_from_bytes_rejects_invalid_option_tag() {
        let mut bytes = on_chain_bytes(&sample());
        let tag_offset = SWAP_DVP_ACCOUNT_LEN - 1 - 8;
        bytes[tag_offset] = 2;
        assert!(matches!(
            SwapDvp::try_from_bytes(&bytes),
            Err(SwapDvpVerifyError::Malformed(_))
        ));
    }

    #[test]
    fn verify_rejects_wrong_owner() {
        let dvp = sample();
        let (addr, _) = find_swap_dvp_address(
            &dvp.settlement_authority,
            &dvp.user_a,
            &dvp.user_b,
            &dvp.mint_a,
            &dvp.mint_b,
            dvp.nonce,
        );
        let not_program = Pubkey::new_from_array([99u8; 32]);
        let err = verify_swap_dvp_bytes(&addr, &not_program, &on_chain_bytes(&dvp)).unwrap_err();
        assert!(matches!(err, SwapDvpVerifyError::WrongOwner { .. }));
    }

    #[test]
    fn verify_rejects_wrong_address() {
        let dvp = sample();
        let wrong = Pubkey::new_from_array([77u8; 32]);
        let err =
            verify_swap_dvp_bytes(&wrong, &DVP_SWAP_PROGRAM_ID, &on_chain_bytes(&dvp)).unwrap_err();
        assert!(matches!(err, SwapDvpVerifyError::WrongAddress { .. }));
    }

    #[test]
    fn verify_rejects_unsupported_token_programs() {
        for leg_b in [false, true] {
            let mut dvp = sample();
            let fake = Pubkey::new_from_array([6u8; 32]);
            if leg_b {
                dvp.token_program_b = fake;
            } else {
                dvp.token_program_a = fake;
            }
            let (addr, _) = find_swap_dvp_address(
                &dvp.settlement_authority,
                &dvp.user_a,
                &dvp.user_b,
                &dvp.mint_a,
                &dvp.mint_b,
                dvp.nonce,
            );
            let err = verify_swap_dvp_bytes(&addr, &DVP_SWAP_PROGRAM_ID, &on_chain_bytes(&dvp))
                .unwrap_err();
            assert!(matches!(err, SwapDvpVerifyError::Malformed(_)));
        }
    }

    #[test]
    fn verify_accepts_canonical_program_owned_account() {
        let dvp = sample();
        let (addr, _) = find_swap_dvp_address(
            &dvp.settlement_authority,
            &dvp.user_a,
            &dvp.user_b,
            &dvp.mint_a,
            &dvp.mint_b,
            dvp.nonce,
        );
        let parsed =
            verify_swap_dvp_bytes(&addr, &DVP_SWAP_PROGRAM_ID, &on_chain_bytes(&dvp)).unwrap();
        assert_eq!(parsed.public(), Some(&dvp));
        assert_eq!(parsed, SwapDvpAccount::Public(dvp));
    }

    #[test]
    fn verify_confidential_layout_and_sentinel() {
        let mut base = sample();
        base.amount_b = u64::MAX;
        // Layout verification treats ciphertexts as opaque wire bytes.
        let amount = crate::ciphertext::AmountCiphertexts {
            lo: [7; 64],
            hi: [8; 64],
        };
        let address = find_swap_dvp_address(
            &base.settlement_authority,
            &base.user_a,
            &base.user_b,
            &base.mint_a,
            &base.mint_b,
            base.nonce,
        )
        .0;
        let mut data = on_chain_bytes(&base);
        data.extend_from_slice(&amount.lo);
        data.extend_from_slice(&amount.hi);
        let parsed = verify_swap_dvp_bytes(&address, &DVP_SWAP_PROGRAM_ID, &data).unwrap();
        assert_eq!(parsed.public(), None);
        assert_eq!(
            parsed,
            SwapDvpAccount::Confidential(ConfidentialSwapDvp {
                base: base.clone(),
                amount_b: amount
            })
        );
        assert!(SwapDvp::try_from_bytes(&data).is_err());
        for len in [
            SWAP_DVP_ACCOUNT_LEN - 1,
            SWAP_DVP_ACCOUNT_LEN + 1,
            CONFIDENTIAL_SWAP_DVP_ACCOUNT_LEN - 1,
        ] {
            assert!(matches!(
                verify_swap_dvp_bytes(&address, &DVP_SWAP_PROGRAM_ID, &data[..len]),
                Err(SwapDvpVerifyError::UnsupportedSize { .. })
            ));
        }
        base.amount_b = 1;
        data[..SWAP_DVP_ACCOUNT_LEN].copy_from_slice(&on_chain_bytes(&base));
        assert!(matches!(
            verify_swap_dvp_bytes(&address, &DVP_SWAP_PROGRAM_ID, &data),
            Err(SwapDvpVerifyError::Malformed(_))
        ));
    }
}
