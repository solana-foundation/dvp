#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DvpSwapInstructionDiscriminators {
    CreateDvp = 0,
    ReclaimDvp = 1,
    SettleDvp = 2,
    CancelDvp = 3,
    RejectDvp = 4,
    RecoverDvp = 5,
    CreateConfidentialDvp = 6,
    ReclaimConfidentialDvp = 7,
    SettleConfidentialDvp = 8,
    CancelConfidentialDvp = 9,
    RejectConfidentialDvp = 10,
    RecoverConfidentialDvp = 11,
    ApplyConfidentialDvp = 12,
}

impl TryFrom<u8> for DvpSwapInstructionDiscriminators {
    type Error = ();

    fn try_from(discriminator: u8) -> Result<Self, Self::Error> {
        match discriminator {
            0 => Ok(Self::CreateDvp),
            1 => Ok(Self::ReclaimDvp),
            2 => Ok(Self::SettleDvp),
            3 => Ok(Self::CancelDvp),
            4 => Ok(Self::RejectDvp),
            5 => Ok(Self::RecoverDvp),
            6 => Ok(Self::CreateConfidentialDvp),
            7 => Ok(Self::ReclaimConfidentialDvp),
            8 => Ok(Self::SettleConfidentialDvp),
            9 => Ok(Self::CancelConfidentialDvp),
            10 => Ok(Self::RejectConfidentialDvp),
            11 => Ok(Self::RecoverConfidentialDvp),
            12 => Ok(Self::ApplyConfidentialDvp),
            _ => Err(()),
        }
    }
}
