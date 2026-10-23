//! Wire types for confidential swap accounts, without cryptographic dependencies.

/// A compressed commitment and decrypt handle, each 32 bytes.
pub const ELGAMAL_CIPHERTEXT_LEN: usize = 32 + 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmountCiphertexts {
    pub lo: [u8; ELGAMAL_CIPHERTEXT_LEN],
    pub hi: [u8; ELGAMAL_CIPHERTEXT_LEN],
}
