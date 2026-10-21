extern crate alloc;

use alloc::string::String;
use codama::CodamaInstructions;
use pinocchio::Address as Pubkey;

use crate::processor::shared::confidential::{CtTransferData, LegBRefund};

/// Instructions for the DvP Swap Program.
///
/// Lifecycle: Create → fund each leg via raw SPL Transfer to the leg's
/// escrow ATA → Settle | (Reclaim per leg) | Cancel | Reject. Funding
/// is intentionally not a program instruction so that custodian
/// integrations can use a plain SPL Transfer. Recover is the post-close
/// path for deposits that land after the trade is closed.
/// Discriminator order matches `discriminator::DvpSwapInstructionDiscriminators`.
///
/// Token-2022: every transfer instruction accepts a separate token
/// program per leg (`token_program_a` / `token_program_b`), enabling
/// cross-program swaps (e.g. legacy-SPL ↔ Token-2022). Each token
/// program account must match the owner of its leg's mint. Mints
/// carrying amount-mutating Token-2022 extensions (TransferFee,
/// InterestBearing, ScaledUiAmount) are
/// rejected at CreateDvp; later instructions do not re-check so that
/// funds remain recoverable if a mint's extension parameters change
/// post-Create.
///
/// TransferHook is supported: instructions that issue a `TransferChecked`
/// CPI (Settle/Cancel/Reject/Reclaim) treat any accounts beyond their
/// fixed prefix as transfer-hook extras forwarded to the token program.
/// Settle/Cancel/Reject split the trailing accounts between the two legs
/// via the `leg_a_extras_count: u8` data field; Reclaim has a single
/// leg, so all trailing accounts feed its one CPI. The client is
/// responsible for resolving the hook's `ExtraAccountMetaList`
/// off-chain and supplying the resulting accounts in the order the hook
/// expects.
#[repr(C, u8)]
#[derive(Clone, Debug, PartialEq, CodamaInstructions)]
// IDL-only type: processors hand-parse instruction data and never
// construct this enum, so the CreateDvp variant's size is irrelevant.
#[allow(clippy::large_enum_variant)]
pub enum DvpSwapProgramInstruction {
    /// Permissionless. Creates the SwapDvp PDA and both escrow ATAs.
    /// No funding happens here; each leg is deposited by sending tokens
    /// via a raw SPL Transfer to the leg's escrow ATA.
    #[codama(account(
        name = "payer",
        docs = "Funds account/ATA creation rent",
        signer,
        writable
    ))]
    #[codama(account(name = "swap_dvp", docs = "SwapDvp PDA to be created", writable))]
    #[codama(account(
        name = "nonce_tombstone",
        docs = "Per-DvP nonce tombstone PDA, created here and never closed; rejects nonce reuse",
        writable
    ))]
    #[codama(account(
        name = "settlement_authority",
        docs = "Third-party authority allowed to settle/cancel; must not be executable so it can receive closed-account rent"
    ))]
    #[codama(account(
        name = "user_a",
        docs = "Seller (delivers mint_a). Must be a system-owned, non-executable account so it can authorize Reject/Reclaim/Recover"
    ))]
    #[codama(account(
        name = "user_b",
        docs = "Buyer (delivers mint_b). Same signer-capability requirement as user_a"
    ))]
    #[codama(account(name = "mint_a", docs = "Mint of the asset leg (seller delivers)"))]
    #[codama(account(name = "mint_b", docs = "Mint of the cash leg (buyer delivers)"))]
    #[codama(account(
        name = "dvp_ata_a",
        docs = "swap_dvp's ATA for mint_a (created here)",
        writable
    ))]
    #[codama(account(
        name = "dvp_ata_b",
        docs = "swap_dvp's ATA for mint_b (created here)",
        writable
    ))]
    #[codama(account(name = "system_program", docs = "System program"))]
    #[codama(account(
        name = "token_program_a",
        docs = "SPL Token or Token-2022 program; must own mint_a"
    ))]
    #[codama(account(
        name = "token_program_b",
        docs = "SPL Token or Token-2022 program; must own mint_b"
    ))]
    #[codama(account(
        name = "associated_token_program",
        docs = "Associated Token Account program"
    ))]
    CreateDvp {
        /// Asset leg size.
        amount_a: u64,
        /// Cash leg size.
        amount_b: u64,
        /// Settlement is rejected after this Unix timestamp.
        expiry_timestamp: i64,
        /// Disambiguates DvPs sharing all other seeds.
        nonce: u64,
        /// Opaque client reference (e.g. an off-chain order ID), at most
        /// `MAX_REF_STRING_LEN` (64) bytes. Stored zero-padded on the
        /// SwapDvp (`None` stores all zeros); the program never reads it.
        ref_string: Option<String>,
        /// Wallet receiving user_a's settlement proceeds — the cash leg
        /// (`mint_b`) — at its canonical ATA (e.g. a custodian deposit
        /// wallet). Defaults to `user_a`. Settle-only: refunds always go
        /// to the depositor.
        user_a_settlement_destination: Option<Pubkey>,
        /// Wallet receiving user_b's settlement proceeds — the asset leg
        /// (`mint_a`). Defaults to `user_b`; same rules as above.
        user_b_settlement_destination: Option<Pubkey>,
        /// If set, settlement is also rejected before this timestamp.
        earliest_settlement_timestamp: Option<i64>,
    } = 0,

    /// Permissioned: signer must be `dvp.user_a` or `dvp.user_b`. Drains
    /// the signer's leg back to the signer. The DvP stays open; the
    /// leg can be re-funded.
    #[codama(account(
        name = "signer",
        docs = "Depositor; must equal dvp.user_a or dvp.user_b",
        signer
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "SwapDvp PDA (signs the transfer as authority)"
    ))]
    #[codama(account(
        name = "mint",
        docs = "Mint of the leg being reclaimed; must equal dvp.mint_a or dvp.mint_b"
    ))]
    #[codama(account(
        name = "dvp_source_ata",
        docs = "DvP's escrow ATA for the leg's mint",
        writable
    ))]
    #[codama(account(
        name = "signer_dest_ata",
        docs = "Signer's canonical ATA for the leg's mint",
        writable
    ))]
    #[codama(account(
        name = "token_program",
        docs = "SPL Token or Token-2022 program; must own mint"
    ))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used if signer_dest_ata requires a memo"
    ))]
    ReclaimDvp {} = 1,

    /// Permissioned: signer must be `dvp.settlement_authority`. Atomic
    /// DvP settlement — cash to seller, asset to buyer — followed by
    /// refunding any over-deposit surplus to each leg's depositor and
    /// closing both escrows and the SwapDvp PDA. Closed-account rent
    /// goes to the settlement authority.
    #[codama(account(
        name = "settlement_authority",
        docs = "Must equal dvp.settlement_authority; receives closed-account rent",
        signer,
        writable
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "SwapDvp PDA (signs CPIs, then closed)",
        writable
    ))]
    #[codama(account(name = "mint_a", docs = "Must equal dvp.mint_a"))]
    #[codama(account(name = "mint_b", docs = "Must equal dvp.mint_b"))]
    #[codama(account(
        name = "dvp_ata_a",
        docs = "Asset escrow (drained, then closed)",
        writable
    ))]
    #[codama(account(
        name = "dvp_ata_b",
        docs = "Cash escrow (drained, then closed)",
        writable
    ))]
    #[codama(account(
        name = "user_a_destination_ata_b",
        docs = "user_a_settlement_destination's ATA for mint_b; receives the cash leg (destination defaults to user_a)",
        writable
    ))]
    #[codama(account(
        name = "user_b_destination_ata_a",
        docs = "user_b_settlement_destination's ATA for mint_a; receives the asset leg (destination defaults to user_b)",
        writable
    ))]
    #[codama(account(
        name = "user_a_ata_a",
        docs = "user_a's ATA for mint_a; receives any asset-leg surplus refund. Required and must be pre-initialized: anyone can dust the escrow, forcing a surplus refund, and a missing ATA reverts the whole Settle",
        writable
    ))]
    #[codama(account(
        name = "user_b_ata_b",
        docs = "user_b's ATA for mint_b; receives any cash-leg surplus refund. Required and must be pre-initialized, same as user_a_ata_a",
        writable
    ))]
    #[codama(account(
        name = "token_program_a",
        docs = "SPL Token or Token-2022 program; must own mint_a"
    ))]
    #[codama(account(
        name = "token_program_b",
        docs = "SPL Token or Token-2022 program; must own mint_b"
    ))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used for destinations that require a memo"
    ))]
    SettleDvp {
        /// Splits the trailing remaining accounts between the two
        /// legs' `TransferChecked` CPIs. The first
        /// `leg_a_extras_count` remaining accounts go to leg A; the
        /// rest go to leg B. Either value can be 0 independently
        /// (e.g. only leg B's mint carries a transfer hook).
        leg_a_extras_count: u8,
    } = 2,

    /// Permissioned: signer must be `dvp.settlement_authority`. Refunds
    /// any funded legs to their depositors and closes the trade. No
    /// expiry check — Cancel works post-expiry too. Closed-account rent
    /// goes to the settlement authority.
    #[codama(account(
        name = "settlement_authority",
        docs = "Must equal dvp.settlement_authority; receives closed-account rent",
        signer,
        writable
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "SwapDvp PDA (signs CPIs, then closed)",
        writable
    ))]
    #[codama(account(name = "mint_a", docs = "Must equal dvp.mint_a"))]
    #[codama(account(name = "mint_b", docs = "Must equal dvp.mint_b"))]
    #[codama(account(
        name = "dvp_ata_a",
        docs = "Asset escrow (drained if funded, then closed)",
        writable
    ))]
    #[codama(account(
        name = "dvp_ata_b",
        docs = "Cash escrow (drained if funded, then closed)",
        writable
    ))]
    #[codama(account(
        name = "user_a_ata_a",
        docs = "user_a's ATA for mint_a; refund destination",
        writable
    ))]
    #[codama(account(
        name = "user_b_ata_b",
        docs = "user_b's ATA for mint_b; refund destination",
        writable
    ))]
    #[codama(account(
        name = "token_program_a",
        docs = "SPL Token or Token-2022 program; must own mint_a"
    ))]
    #[codama(account(
        name = "token_program_b",
        docs = "SPL Token or Token-2022 program; must own mint_b"
    ))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used for destinations that require a memo"
    ))]
    CancelDvp {
        /// Splits the trailing remaining accounts between the two
        /// legs' refund `TransferChecked` CPIs. The first
        /// `leg_a_extras_count` remaining accounts go to leg A; the
        /// rest go to leg B. Accounts for an unfunded leg are ignored
        /// since that leg's transfer is skipped.
        leg_a_extras_count: u8,
    } = 3,

    /// Permissioned: signer must be `dvp.user_a` or `dvp.user_b`. Either
    /// counterparty can pull the plug; refunds any funded legs to their
    /// depositors and closes the trade. Closed-account rent goes to the
    /// signer, not `dvp.settlement_authority` — Reject is the safety
    /// valve and must work even if the settlement authority is
    /// unreachable (e.g. a sysvar or executable address).
    #[codama(account(
        name = "signer",
        docs = "Must equal dvp.user_a or dvp.user_b; receives closed-account rent",
        signer,
        writable
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "SwapDvp PDA (signs CPIs, then closed)",
        writable
    ))]
    #[codama(account(name = "mint_a", docs = "Must equal dvp.mint_a"))]
    #[codama(account(name = "mint_b", docs = "Must equal dvp.mint_b"))]
    #[codama(account(
        name = "dvp_ata_a",
        docs = "Asset escrow (drained if funded, then closed)",
        writable
    ))]
    #[codama(account(
        name = "dvp_ata_b",
        docs = "Cash escrow (drained if funded, then closed)",
        writable
    ))]
    #[codama(account(
        name = "user_a_ata_a",
        docs = "user_a's ATA for mint_a; refund destination",
        writable
    ))]
    #[codama(account(
        name = "user_b_ata_b",
        docs = "user_b's ATA for mint_b; refund destination",
        writable
    ))]
    #[codama(account(
        name = "token_program_a",
        docs = "SPL Token or Token-2022 program; must own mint_a"
    ))]
    #[codama(account(
        name = "token_program_b",
        docs = "SPL Token or Token-2022 program; must own mint_b"
    ))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used for destinations that require a memo"
    ))]
    RejectDvp {
        /// Splits the trailing remaining accounts between the two
        /// legs' refund `TransferChecked` CPIs. The first
        /// `leg_a_extras_count` remaining accounts go to leg A; the
        /// rest go to leg B. Accounts for an unfunded leg are ignored
        /// since that leg's transfer is skipped.
        leg_a_extras_count: u8,
    } = 4,

    /// Permissioned: signer must be the recovered leg's depositor.
    /// Recovers a deposit that landed in a recreated escrow ATA after
    /// the SwapDvp was closed. The caller supplies the original seed
    /// inputs; the program re-derives the PDA, requires its nonce
    /// tombstone (proof the DvP existed), drains the escrow back to
    /// the signer, and closes it. Signer selects the leg as in
    /// Reclaim: user_a recovers mint_a, user_b recovers mint_b.
    #[codama(account(
        name = "signer",
        docs = "Depositor of the leg being recovered; must equal the leg's user; receives the closed escrow's rent",
        signer,
        writable
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "Closed SwapDvp address, re-derived from the seed args; must be system-owned and empty (signs the CPIs)"
    ))]
    #[codama(account(
        name = "nonce_tombstone",
        docs = "Nonce tombstone PDA for swap_dvp; must be program-owned"
    ))]
    #[codama(account(
        name = "mint",
        docs = "Mint of the leg being recovered; mint_a if signed by user_a, mint_b if by user_b"
    ))]
    #[codama(account(
        name = "dvp_escrow_ata",
        docs = "Recreated escrow ATA for the closed DvP (drained, then closed)",
        writable
    ))]
    #[codama(account(
        name = "signer_dest_ata",
        docs = "Signer's canonical ATA for the leg's mint",
        writable
    ))]
    #[codama(account(
        name = "token_program",
        docs = "SPL Token or Token-2022 program; must own dvp_escrow_ata (the program the escrow was created under, regardless of the mint's current owner)"
    ))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used if signer_dest_ata requires a memo"
    ))]
    RecoverDvp {
        /// Original `settlement_authority` seed input.
        settlement_authority: Pubkey,
        /// Original `user_a` seed input.
        user_a: Pubkey,
        /// Original `user_b` seed input.
        user_b: Pubkey,
        /// Original `mint_a` seed input.
        mint_a: Pubkey,
        /// Original `mint_b` seed input.
        mint_b: Pubkey,
        /// Original `nonce` seed input.
        nonce: u64,
    } = 5,

    /// Creates a DvP with a confidential leg B.
    #[codama(account(
        name = "payer",
        docs = "Funds account/ATA creation rent",
        signer,
        writable
    ))]
    #[codama(account(name = "swap_dvp", docs = "SwapDvp PDA to be created", writable))]
    #[codama(account(
        name = "nonce_tombstone",
        docs = "Per-DvP nonce tombstone PDA, created here and never closed; rejects nonce reuse",
        writable
    ))]
    #[codama(account(
        name = "settlement_authority",
        docs = "Third-party authority allowed to settle/cancel; must not be executable so it can receive closed-account rent"
    ))]
    #[codama(account(
        name = "user_a",
        docs = "Seller (delivers mint_a). Must be a system-owned, non-executable account so it can authorize Reject/Reclaim/Recover"
    ))]
    #[codama(account(
        name = "user_b",
        docs = "Buyer (delivers mint_b). Same signer-capability requirement as user_a"
    ))]
    #[codama(account(name = "mint_a", docs = "Mint of the asset leg (seller delivers)"))]
    #[codama(account(
        name = "mint_b",
        docs = "Token-2022 mint for leg B with ConfidentialTransferMint"
    ))]
    #[codama(account(
        name = "dvp_ata_a",
        docs = "swap_dvp's ATA for mint_a (created here)",
        writable
    ))]
    #[codama(account(
        name = "dvp_ata_b",
        docs = "swap_dvp's ATA for mint_b; created and configured for confidential transfers",
        writable
    ))]
    #[codama(account(name = "system_program", docs = "System program"))]
    #[codama(account(
        name = "token_program_a",
        docs = "SPL Token or Token-2022 program; must own mint_a"
    ))]
    #[codama(account(name = "token_program_b", docs = "Token-2022 program; must own mint_b"))]
    #[codama(account(
        name = "associated_token_program",
        docs = "Associated Token Account program"
    ))]
    #[codama(account(
        name = "instructions_sysvar",
        docs = "Instructions sysvar used to locate the inline PubkeyValidity proof"
    ))]
    CreateConfidentialDvp {
        amount_a: u64,
        expiry_timestamp: i64,
        nonce: u64,
        amount_b_ciphertext_lo: [u8; 64],
        amount_b_ciphertext_hi: [u8; 64],
        decryptable_zero_balance: [u8; 36],
        pubkey_validity_proof_offset: i8,
        ref_string: Option<String>,
        user_a_settlement_destination: Option<Pubkey>,
        user_b_settlement_destination: Option<Pubkey>,
        earliest_settlement_timestamp: Option<i64>,
    } = 6,

    /// Refunds the signer's leg while the confidential swap stays open.
    #[codama(account(
        name = "signer",
        docs = "Depositor; must equal dvp.user_a or dvp.user_b; receives closed proof-context rent",
        signer,
        writable
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "SwapDvp PDA (signs the transfer as authority)"
    ))]
    #[codama(account(
        name = "mint",
        docs = "Mint of the leg being reclaimed; must equal dvp.mint_a or dvp.mint_b"
    ))]
    #[codama(account(
        name = "dvp_source_ata",
        docs = "DvP's escrow ATA for the leg's mint",
        writable
    ))]
    #[codama(account(
        name = "signer_dest_ata",
        docs = "Signer's canonical ATA for the leg's mint",
        writable
    ))]
    #[codama(account(
        name = "token_program",
        docs = "SPL Token or Token-2022 program; must own mint"
    ))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used if signer_dest_ata requires a memo"
    ))]
    #[codama(account(
        name = "zk_elgamal_proof_program",
        docs = "ZK ElGamal Proof program; closes proof contexts and returns rent to the signer"
    ))]
    #[codama(account(
        name = "equality_context",
        docs = "CiphertextCommitmentEquality context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "validity_context",
        docs = "BatchedGroupedCiphertext3HandlesValidity context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "range_context",
        docs = "BatchedRangeProofU128 context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "zero_context",
        docs = "ZeroCiphertext context for Full; program-id placeholder for None or Partial",
        writable,
        optional
    ))]
    ReclaimConfidentialDvp { leg_b_refund: LegBRefund } = 7,

    /// Settles the confidential payment and refunds any surplus.
    #[codama(account(
        name = "settlement_authority",
        docs = "Must equal dvp.settlement_authority; receives closed-account rent",
        signer,
        writable
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "SwapDvp PDA (signs CPIs, then closed)",
        writable
    ))]
    #[codama(account(name = "mint_a", docs = "Must equal dvp.mint_a"))]
    #[codama(account(name = "mint_b", docs = "Must equal dvp.mint_b"))]
    #[codama(account(
        name = "dvp_ata_a",
        docs = "Asset escrow (drained, then closed)",
        writable
    ))]
    #[codama(account(
        name = "dvp_ata_b",
        docs = "Confidential leg B escrow; left open if balances remain after the operation",
        writable
    ))]
    #[codama(account(
        name = "user_a_destination_ata_b",
        docs = "user_a_settlement_destination's ATA for mint_b; receives the leg B payment (destination defaults to user_a)",
        writable
    ))]
    #[codama(account(
        name = "user_b_destination_ata_a",
        docs = "user_b_settlement_destination's ATA for mint_a; receives the asset leg (destination defaults to user_b)",
        writable
    ))]
    #[codama(account(
        name = "user_a_ata_a",
        docs = "user_a's ATA for mint_a; receives any asset-leg surplus refund. Required and must be pre-initialized: anyone can dust the escrow, forcing a surplus refund, and a missing ATA reverts the whole Settle",
        writable
    ))]
    #[codama(account(
        name = "user_b_ata_b",
        docs = "user_b's ATA for mint_b; receives any leg B surplus refund. Required and must be pre-initialized, same as user_a_ata_a",
        writable
    ))]
    #[codama(account(
        name = "token_program_a",
        docs = "SPL Token or Token-2022 program; must own mint_a"
    ))]
    #[codama(account(name = "token_program_b", docs = "Token-2022 program; must own mint_b"))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used for destinations that require a memo"
    ))]
    #[codama(account(
        name = "zk_elgamal_proof_program",
        docs = "ZK ElGamal Proof program; closes proof contexts and returns rent to the signer"
    ))]
    #[codama(account(
        name = "payment_equality_context",
        docs = "CiphertextCommitmentEquality context for the leg B payment",
        writable
    ))]
    #[codama(account(
        name = "payment_validity_context",
        docs = "BatchedGroupedCiphertext3HandlesValidity context for the leg B payment",
        writable
    ))]
    #[codama(account(
        name = "payment_range_context",
        docs = "BatchedRangeProofU128 context for the leg B payment",
        writable
    ))]
    #[codama(account(
        name = "eq_lo_context",
        docs = "CiphertextCiphertextEquality context binding the low payment limb to the stored amount",
        writable
    ))]
    #[codama(account(
        name = "eq_hi_context",
        docs = "CiphertextCiphertextEquality context binding the high payment limb to the stored amount",
        writable
    ))]
    #[codama(account(
        name = "zero_context",
        docs = "ZeroCiphertext context proving the leg B escrow is empty after payment and surplus refund",
        writable
    ))]
    #[codama(account(
        name = "surplus_equality_context",
        docs = "CiphertextCommitmentEquality context for surplus_b; program-id placeholder when absent",
        writable,
        optional
    ))]
    #[codama(account(
        name = "surplus_validity_context",
        docs = "BatchedGroupedCiphertext3HandlesValidity context for surplus_b; program-id placeholder when absent",
        writable,
        optional
    ))]
    #[codama(account(
        name = "surplus_range_context",
        docs = "BatchedRangeProofU128 context for surplus_b; program-id placeholder when absent",
        writable,
        optional
    ))]
    SettleConfidentialDvp {
        leg_a_extras_count: u8,
        payment: CtTransferData,
        surplus_b: Option<CtTransferData>,
    } = 8,

    /// Refunds both legs and closes the confidential swap.
    #[codama(account(
        name = "settlement_authority",
        docs = "Must equal dvp.settlement_authority; receives closed-account rent",
        signer,
        writable
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "SwapDvp PDA (signs CPIs, then closed)",
        writable
    ))]
    #[codama(account(name = "mint_a", docs = "Must equal dvp.mint_a"))]
    #[codama(account(name = "mint_b", docs = "Must equal dvp.mint_b"))]
    #[codama(account(
        name = "dvp_ata_a",
        docs = "Asset escrow (drained if funded, then closed)",
        writable
    ))]
    #[codama(account(
        name = "dvp_ata_b",
        docs = "Confidential leg B escrow; left open if balances remain after the operation",
        writable
    ))]
    #[codama(account(
        name = "user_a_ata_a",
        docs = "user_a's ATA for mint_a; refund destination",
        writable
    ))]
    #[codama(account(
        name = "user_b_ata_b",
        docs = "user_b's ATA for mint_b; refund destination",
        writable
    ))]
    #[codama(account(
        name = "token_program_a",
        docs = "SPL Token or Token-2022 program; must own mint_a"
    ))]
    #[codama(account(name = "token_program_b", docs = "Token-2022 program; must own mint_b"))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used for destinations that require a memo"
    ))]
    #[codama(account(
        name = "zk_elgamal_proof_program",
        docs = "ZK ElGamal Proof program; closes proof contexts and returns rent to the signer"
    ))]
    #[codama(account(
        name = "equality_context",
        docs = "CiphertextCommitmentEquality context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "validity_context",
        docs = "BatchedGroupedCiphertext3HandlesValidity context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "range_context",
        docs = "BatchedRangeProofU128 context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "zero_context",
        docs = "ZeroCiphertext context for Full; program-id placeholder for None or Partial",
        writable,
        optional
    ))]
    CancelConfidentialDvp {
        leg_a_extras_count: u8,
        leg_b_refund: LegBRefund,
    } = 9,

    /// Refunds both legs and closes the confidential swap.
    #[codama(account(
        name = "signer",
        docs = "Must equal dvp.user_a or dvp.user_b; receives closed-account rent",
        signer,
        writable
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "SwapDvp PDA (signs CPIs, then closed)",
        writable
    ))]
    #[codama(account(name = "mint_a", docs = "Must equal dvp.mint_a"))]
    #[codama(account(name = "mint_b", docs = "Must equal dvp.mint_b"))]
    #[codama(account(
        name = "dvp_ata_a",
        docs = "Asset escrow (drained if funded, then closed)",
        writable
    ))]
    #[codama(account(
        name = "dvp_ata_b",
        docs = "Confidential leg B escrow; left open if balances remain after the operation",
        writable
    ))]
    #[codama(account(
        name = "user_a_ata_a",
        docs = "user_a's ATA for mint_a; refund destination",
        writable
    ))]
    #[codama(account(
        name = "user_b_ata_b",
        docs = "user_b's ATA for mint_b; refund destination",
        writable
    ))]
    #[codama(account(
        name = "token_program_a",
        docs = "SPL Token or Token-2022 program; must own mint_a"
    ))]
    #[codama(account(name = "token_program_b", docs = "Token-2022 program; must own mint_b"))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used for destinations that require a memo"
    ))]
    #[codama(account(
        name = "zk_elgamal_proof_program",
        docs = "ZK ElGamal Proof program; closes proof contexts and returns rent to the signer"
    ))]
    #[codama(account(
        name = "equality_context",
        docs = "CiphertextCommitmentEquality context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "validity_context",
        docs = "BatchedGroupedCiphertext3HandlesValidity context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "range_context",
        docs = "BatchedRangeProofU128 context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "zero_context",
        docs = "ZeroCiphertext context for Full; program-id placeholder for None or Partial",
        writable,
        optional
    ))]
    RejectConfidentialDvp {
        leg_a_extras_count: u8,
        leg_b_refund: LegBRefund,
    } = 10,

    /// Recovers a confidential escrow after the swap closes.
    #[codama(account(
        name = "signer",
        docs = "Must equal user_b from the original seeds; receives closed escrow and proof-context rent",
        signer,
        writable
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "Closed SwapDvp address, re-derived from the seed args; must be system-owned and empty (signs the CPIs)"
    ))]
    #[codama(account(
        name = "nonce_tombstone",
        docs = "Nonce tombstone PDA for swap_dvp; must be program-owned"
    ))]
    #[codama(account(
        name = "mint",
        docs = "Leg B mint; must equal mint_b from the original seeds"
    ))]
    #[codama(account(
        name = "dvp_escrow_ata",
        docs = "Confidential leg B escrow of the closed swap; closed only when all balances are zero",
        writable
    ))]
    #[codama(account(
        name = "signer_dest_ata",
        docs = "Signer's canonical ATA for the leg's mint",
        writable
    ))]
    #[codama(account(
        name = "token_program",
        docs = "Token-2022 program that owns the leg B escrow"
    ))]
    #[codama(account(
        name = "memo_program",
        docs = "SPL Memo program; only used if signer_dest_ata requires a memo"
    ))]
    #[codama(account(
        name = "zk_elgamal_proof_program",
        docs = "ZK ElGamal Proof program; closes proof contexts and returns rent to the signer"
    ))]
    #[codama(account(
        name = "equality_context",
        docs = "CiphertextCommitmentEquality context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "validity_context",
        docs = "BatchedGroupedCiphertext3HandlesValidity context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "range_context",
        docs = "BatchedRangeProofU128 context for Full or Partial; program-id placeholder for None",
        writable,
        optional
    ))]
    #[codama(account(
        name = "zero_context",
        docs = "ZeroCiphertext context for Full; program-id placeholder for None or Partial",
        writable,
        optional
    ))]
    RecoverConfidentialDvp {
        settlement_authority: Pubkey,
        user_a: Pubkey,
        user_b: Pubkey,
        mint_a: Pubkey,
        mint_b: Pubkey,
        nonce: u64,
        leg_b_refund: LegBRefund,
    } = 11,

    /// Applies pending confidential credits, before or after close.
    #[codama(account(
        name = "signer",
        docs = "user_a, user_b or settlement_authority from the swap state or original seeds",
        signer
    ))]
    #[codama(account(
        name = "swap_dvp",
        docs = "SwapDvp PDA derived from the seed arguments; may be open or closed"
    ))]
    #[codama(account(
        name = "nonce_tombstone",
        docs = "Nonce tombstone PDA; proves the swap existed when swap_dvp is closed"
    ))]
    #[codama(account(
        name = "dvp_ata_b",
        docs = "Confidential leg B escrow; pending credits are applied to its available balance",
        writable
    ))]
    #[codama(account(
        name = "token_program",
        docs = "Token-2022 program that owns the leg B escrow"
    ))]
    ApplyConfidentialDvp {
        expected_pending_balance_credit_counter: u64,
        new_decryptable_available_balance: [u8; 36],
        settlement_authority: Pubkey,
        user_a: Pubkey,
        user_b: Pubkey,
        mint_a: Pubkey,
        mint_b: Pubkey,
        nonce: u64,
    } = 12,
}
