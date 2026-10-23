//! Wire types shared by confidential instructions and their processors.

use codama::CodamaType;
use pinocchio::error::ProgramError;

use crate::require;

// Wire sizes of the decryptable balance and each auditor ciphertext.
pub const AE_CIPHERTEXT_LEN: usize = 36;
pub const ELGAMAL_CIPHERTEXT_LEN: usize = 64;

/// Wire data forwarded to a Token-2022 confidential transfer (164 bytes).
#[derive(Clone, Debug, PartialEq, CodamaType)]
pub struct CtTransferData {
    // These match AE_CIPHERTEXT_LEN and ELGAMAL_CIPHERTEXT_LEN respectively.
    pub new_source_decryptable_available_balance: [u8; 36],
    pub auditor_ciphertext_lo: [u8; 64],
    pub auditor_ciphertext_hi: [u8; 64],
}

impl CtTransferData {
    // One decryptable balance followed by the auditor's low and high ciphertexts.
    pub const LEN: usize = AE_CIPHERTEXT_LEN + 2 * ELGAMAL_CIPHERTEXT_LEN;
}

impl TryFrom<&[u8]> for CtTransferData {
    type Error = ProgramError;

    /// Decodes exactly one transfer payload; rejects truncated or trailing bytes.
    #[inline(always)]
    fn try_from(data: &[u8]) -> Result<Self, Self::Error> {
        require!(
            data.len() == Self::LEN,
            ProgramError::InvalidInstructionData
        );
        let (decryptable_balance, auditor_ciphertexts) = data.split_at(AE_CIPHERTEXT_LEN);
        let (auditor_ciphertext_lo, auditor_ciphertext_hi) =
            auditor_ciphertexts.split_at(ELGAMAL_CIPHERTEXT_LEN);
        Ok(Self {
            new_source_decryptable_available_balance: decryptable_balance.try_into().unwrap(),
            auditor_ciphertext_lo: auditor_ciphertext_lo.try_into().unwrap(),
            auditor_ciphertext_hi: auditor_ciphertext_hi.try_into().unwrap(),
        })
    }
}

/// Tag 0: no CT transfer; tag 1: drain and zero-check; tag 2: partial refund.
#[derive(Clone, Debug, PartialEq, CodamaType)]
#[codama(name = "leg_b_refund")]
pub enum LegBRefund {
    None,
    Full(CtTransferData),
    Partial(CtTransferData),
}

impl TryFrom<&[u8]> for LegBRefund {
    type Error = ProgramError;

    /// Decodes only the refund field: its tag and optional transfer payload.
    /// Rejects unknown tags, truncated payloads and trailing bytes.
    #[inline(always)]
    fn try_from(refund_bytes: &[u8]) -> Result<Self, Self::Error> {
        let (tag, payload) = refund_bytes
            .split_first()
            .ok_or(ProgramError::InvalidInstructionData)?;
        match tag {
            0 if payload.is_empty() => Ok(Self::None),
            1 => Ok(Self::Full(CtTransferData::try_from(payload)?)),
            2 => Ok(Self::Partial(CtTransferData::try_from(payload)?)),
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}
