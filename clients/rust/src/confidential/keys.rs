use super::{AmountCiphertexts, ConfidentialError, AMOUNT_LO_BITS, MAX_TRANSFER_AMOUNT};
use curve25519_dalek::scalar::Scalar;
use hkdf::Hkdf;
use sha2::Sha512;
use solana_pubkey::Pubkey;
use solana_zk_sdk::encryption::{
    auth_encryption::AeKey, derivation::derive_confidential_keys_from_ikm, elgamal::ElGamalKeypair,
    pedersen::PedersenOpening,
};

// Protocol domain separators; changing these bytes changes the derived keys/openings.
const SHARED_SEED_DOMAIN: &[u8] = b"dvp/confidential-amount-b/seed/v1";
const AMOUNT_OPENINGS_SALT: &[u8] = b"dvp/confidential-amount-b/v1";

/// The callback computes HMAC-SHA256 with a dedicated master key of at least
/// 32 bytes. The master key stays in the caller's KMS/HSM.
pub fn derive_shared_seed<E>(
    swap: &Pubkey,
    mac: impl FnOnce(&[u8]) -> Result<[u8; 32], E>,
) -> Result<[u8; 32], E> {
    let mut message = SHARED_SEED_DOMAIN.to_vec();
    message.extend_from_slice(swap.as_ref());
    mac(&message)
}

/// The wrapper derives neither Debug nor serde, but its fields are public and
/// individually serializable (the SDK `ElGamalKeypair` derives `Serialize`).
/// Never serialize, log or report them.
pub struct EscrowKeys {
    pub elgamal: ElGamalKeypair,
    pub ae: AeKey,
    pub opening_lo: PedersenOpening,
    pub opening_hi: PedersenOpening,
}

impl EscrowKeys {
    pub fn from_seed(seed: &[u8; 32]) -> Result<Self, ConfidentialError> {
        let (elgamal, ae) = derive_confidential_keys_from_ikm(seed)
            .map_err(|e| ConfidentialError::Proof(e.to_string()))?;
        let hk = Hkdf::<Sha512>::new(Some(AMOUNT_OPENINGS_SALT), seed);
        let opening = |info: &[u8]| {
            let mut wide = [0; 64];
            hk.expand(info, &mut wide)
                .expect("64 bytes fit HKDF-SHA512 output");
            PedersenOpening::new(Scalar::from_bytes_mod_order_wide(&wide))
        };
        Ok(Self {
            elgamal,
            ae,
            opening_lo: opening(b"opening-lo"),
            opening_hi: opening(b"opening-hi"),
        })
    }

    pub fn encrypt_amount(&self, amount: u64) -> Result<AmountCiphertexts, ConfidentialError> {
        if amount == 0 || amount > MAX_TRANSFER_AMOUNT {
            return Err(ConfidentialError::InvalidAmount);
        }
        Ok(AmountCiphertexts {
            lo: self
                .elgamal
                .pubkey()
                .encrypt_with(amount & ((1 << AMOUNT_LO_BITS) - 1), &self.opening_lo)
                .to_bytes(),
            hi: self
                .elgamal
                .pubkey()
                .encrypt_with(amount >> AMOUNT_LO_BITS, &self.opening_hi)
                .to_bytes(),
        })
    }
}
