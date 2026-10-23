use dvp_swap_program_client::confidential::{
    read_escrow_account, refund_session, BalanceEvent, ConfidentialError, EscrowKeys, MintAccount,
    RecipientAccount, RefundAmount, RefundInstruction, RefundRequest, SessionConfig,
    TransactionSession, TransferSource,
};
use solana_account::Account;
use solana_instruction::AccountMeta;

/// Reclaim after expiry, Cancel by the settlement authority, Reject by a party,
/// or Recover after closure. Use generated accounts (and original seed args for
/// Recover); each operation still enforces its on-chain authorization rules.
/// Full refunds spend available only. If late credits remain, Apply and Recover
/// again from a fresh snapshot. None withdraws public tokens only after CT is empty.
#[allow(clippy::too_many_arguments)]
pub fn refund(
    config: &SessionConfig,
    instruction: RefundInstruction,
    escrow: &Account,
    destination: &Account,
    mint_account: &Account,
    keys: &EscrowKeys,
    history: &[BalanceEvent],
    amount: RefundAmount,
    leg_a_extras: &[AccountMeta],
    leg_b_extras: &[AccountMeta],
) -> Result<TransactionSession, ConfidentialError> {
    let (swap, mint, source, dest) = match &instruction {
        RefundInstruction::Reclaim(a) => (a.swap_dvp, a.mint, a.dvp_source_ata, a.signer_dest_ata),
        RefundInstruction::Cancel(a) => (a.swap_dvp, a.mint_b, a.dvp_ata_b, a.user_b_ata_b),
        RefundInstruction::Reject(a) => (a.swap_dvp, a.mint_b, a.dvp_ata_b, a.user_b_ata_b),
        RefundInstruction::Recover(a, _) => {
            (a.swap_dvp, a.mint, a.dvp_escrow_ata, a.signer_dest_ata)
        }
    };
    let (state, _) = read_escrow_account(&source, &escrow.owner, &escrow.data, &swap, &mint)?;
    refund_session(
        config,
        instruction,
        Some(RefundRequest {
            source: TransferSource {
                state: &state,
                keys,
                history,
            },
            recipient: RecipientAccount {
                address: dest,
                owner: destination.owner,
                data: &destination.data,
            },
            // The refund goes to the signer's own account; pass its key to
            // stop trusting the RPC for it.
            expected_recipient: None,
            amount,
            mint: MintAccount {
                address: mint,
                owner: mint_account.owner,
                data: &mint_account.data,
            },
            // Pin the auditor from application configuration (`Some(None)`
            // for none) to stop trusting the RPC for it.
            expected_auditor: None,
        }),
        leg_a_extras,
        leg_b_extras,
    )
}
