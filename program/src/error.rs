use codama::CodamaErrors;
use pinocchio::error::ProgramError;
use thiserror::Error;

/// Errors that may be returned by the DvP Swap Program.
#[derive(Clone, Debug, Eq, PartialEq, Error, CodamaErrors)]
pub enum DvpSwapProgramError {
    /// (0) Signer is not a party to the DvP (must be user_a or user_b)
    #[error("Signer is not a party to the DvP")]
    SignerNotParty,

    /// (1) DvP has passed its expiry timestamp
    #[error("DvP has expired")]
    DvpExpired,

    /// (2) Signer is not the configured settlement authority
    #[error("Signer is not the settlement authority")]
    SettlementAuthorityMismatch,

    /// (3) Current time is before earliest_settlement_timestamp
    #[error("Settlement is not yet allowed")]
    SettlementTooEarly,

    /// (4) Leg balance does not match the expected amount
    #[error("DvP leg is not funded with the expected amount")]
    LegNotFunded,

    /// (5) `expiry_timestamp` is at or before `now()` at creation time
    #[error("DvP expiry must be in the future at creation")]
    ExpiryNotInFuture,

    /// (6) `earliest_settlement_timestamp` is after `expiry_timestamp`
    /// (DvP would never be settleable)
    #[error("Earliest settlement must not be after expiry")]
    EarliestAfterExpiry,

    /// (7) `user_a == user_b`: self-DvP — leg B is unfundable
    #[error("user_a and user_b must differ")]
    SelfDvp,

    /// (8) `mint_a == mint_b`: degenerate same-asset trade
    #[error("mint_a and mint_b must differ")]
    SameMint,

    /// (9) `amount_a == 0` or `amount_b == 0`
    #[error("DvP leg amounts must be non-zero")]
    ZeroAmount,

    /// (10) Mint carries a Token-2022 extension the swap program refuses
    /// to support (transfer fee, interest bearing, scaled UI amount,
    /// non-transferable). A confidential transfer fee config is also
    /// rejected, but only implicitly: it always co-occurs with a transfer
    /// fee config, which the explicit check already blocks.
    /// Checked only at CreateDvp.
    #[error("Mint carries an unsupported Token-2022 extension")]
    BlockedMintExtension,

    /// (11) `settlement_authority` equals `user_a` or `user_b`: it must be
    /// a third party, per the documented role.
    #[error("settlement_authority must not be user_a or user_b")]
    SettlementAuthorityIsParty,

    /// (12) `settlement_authority` is an executable account, which can't be
    /// credited the closed-account rent at Settle/Cancel.
    #[error("settlement_authority must not be executable")]
    SettlementAuthorityExecutable,

    /// (13) A DvP with these seeds was already created; the nonce
    /// tombstone outlives the closed trade so the PDA can't be reused.
    #[error("nonce already used for these DvP seeds")]
    NonceAlreadyUsed,

    /// (14) `expiry_timestamp` is more than `MAX_DVP_DURATION_SECS` past
    /// creation time, which would lock escrow rent for an unbounded term.
    #[error("DvP expiry is too far in the future")]
    ExpiryTooFarInFuture,

    /// (15) `ref_string` exceeds `MAX_REF_STRING_LEN` bytes.
    #[error("ref string exceeds the maximum byte length")]
    RefStringTooLong,

    /// (16) Recover targets a SwapDvp that is still open (program-owned).
    /// While the trade is live, ReclaimDvp is the per-leg recovery path.
    #[error("DvP is still open; use ReclaimDvp while the trade is live")]
    DvpStillOpen,

    /// (17) Recover's seed inputs derive a SwapDvp address with no nonce
    /// tombstone, i.e. no DvP was ever created with these exact seeds.
    #[error("no DvP was ever created with these seeds")]
    DvpNeverCreated,

    /// (18) A non-native escrow ATA holds lamports above its rent-exempt
    /// minimum at CreateDvp. Raw SOL is invisible to token accounting on
    /// non-WSOL legs and the close paths sweep the full lamport balance
    /// to the closer, so adopting the excess would misdirect it.
    #[error("escrow ATA preloaded with unexpected lamports")]
    EscrowPreloadedWithLamports,

    /// (19) A settlement destination equals the SwapDvp PDA, so its
    /// canonical ATA is the escrow itself and delivery would be a
    /// self-transfer, which SPL Token treats as a successful no-op.
    #[error("settlement destination must not be the swap_dvp PDA")]
    SettlementDestinationIsSwapDvp,

    /// (20) The swap_dvp account holds lamports above its rent reserve
    /// at CreateDvp. The terminal close paths sweep the PDA's full
    /// balance to the closer, so adopting a preload would misdirect it.
    #[error("swap_dvp preloaded with lamports above its rent reserve")]
    SwapDvpPreloadedWithLamports,

    /// (21) A settlement recipient or refund ATA is canonical by address
    /// but its token-account owner or mint no longer matches the expected
    /// wallet/mint. A legacy SPL Token account can be reassigned via
    /// SetAuthority without changing its ATA pubkey, so address alone does
    /// not authenticate the recipient.
    #[error("recipient ATA owner or mint does not match")]
    RecipientAtaMismatch,

    /// (22) `user_a` or `user_b` is not a wallet-style identity. Every
    /// unwind path (Reject/Reclaim/Recover) authorizes by requiring the
    /// party to sign, which only a system-owned, non-executable account
    /// can ever do (a keypair directly, or a smart-wallet PDA via CPI).
    /// A party owned by another program (e.g. an SPL Token multisig) or
    /// an executable can never sign, so its late deposits would be
    /// unrecoverable; CreateDvp rejects it up front.
    #[error("user_a and user_b must be system-owned, non-executable accounts")]
    PartyNotSignerCapable,

    /// (23) A leg's mint authority differs from the value captured at
    /// CreateDvp. Checked only at SettleDvp.
    #[error("mint authority changed since DvP creation")]
    MintAuthorityChanged,
    /// (24) Instruction does not match the swap mode.
    #[error("Instruction does not match the swap mode")]
    SwapModeMismatch,

    /// (25) Mint does not support confidential transfers.
    #[error("Mint does not support confidential transfers")]
    MintNotConfidential,

    /// (26) Escrow public balance must be empty at creation.
    #[error("Escrow public balance must be empty at creation")]
    EscrowPublicBalanceNotEmpty,

    /// (27) Escrow does not have confidential transfer state.
    #[error("Escrow does not have confidential transfer state")]
    EscrowNotConfidential,

    /// (28) Proof context owner, length or type is invalid.
    #[error("Proof context owner, length or type is invalid")]
    InvalidProofContext,

    /// (29) Proof context authority does not match the signer.
    #[error("Proof context authority does not match the signer")]
    ProofContextAuthorityMismatch,

    /// (30) Confidential payment does not match the agreed amount.
    #[error("Confidential payment does not match the agreed amount")]
    #[codama(name = "confidential_amount_b_mismatch")]
    ConfidentialAmountBMismatch,

    /// (31) Recipient does not have confidential transfer state.
    #[error("Recipient does not have confidential transfer state")]
    RecipientNotConfidential,

    /// (32) Recipient is not approved for confidential transfers.
    #[error("Recipient is not approved for confidential transfers")]
    RecipientNotApproved,

    /// (33) Recipient does not accept confidential credits.
    #[error("Recipient does not accept confidential credits")]
    RecipientConfidentialCreditsDisabled,

    /// (34) Recipient pending credit counter is full.
    #[error("Recipient pending credit counter is full")]
    RecipientPendingCounterFull,

    /// (35) Escrow available balance does not match the zero proof.
    #[error("Escrow available balance does not match the zero proof")]
    EscrowBalanceNotZero,

    /// (36) A confidential refund is required for leg B.
    #[error("A confidential refund is required for leg B")]
    #[codama(name = "leg_b_refund_required")]
    LegBRefundRequired,
}

impl From<DvpSwapProgramError> for ProgramError {
    fn from(e: DvpSwapProgramError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
