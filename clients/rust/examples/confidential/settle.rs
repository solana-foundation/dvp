use dvp_swap_program_client::{
    confidential::{
        settle_session, BalanceEvent, ConfidentialError, EscrowKeys, MintAccount, RecipientAccount,
        SessionConfig, SettleRequest, TransactionSession, TransferSource,
    },
    instructions::SettleConfidentialDvp,
    verify::{verify_confidential_terms, ExpectedConfidentialTerms},
};
use solana_account::Account;
use solana_instruction::AccountMeta;

/// Fetch these five accounts together, after Apply confirms. Addresses are the
/// corresponding generated Settle accounts; do not reuse pre-Apply snapshots.
pub struct Snapshot<'a> {
    pub swap: &'a Account,
    pub escrow: &'a Account,
    pub recipient: &'a Account,
    pub surplus_recipient: &'a Account,
    pub mint: &'a Account,
}

/// Build proofs from checked current state, including the mint's optional auditor.
/// Resolve hook extras beforehand; hooked Settle requires V1. Send the resulting
/// preparations in order, then the final transaction with the settlement authority.
#[allow(clippy::too_many_arguments)]
pub fn settle(
    config: &SessionConfig,
    accounts: SettleConfidentialDvp,
    snapshot: Snapshot<'_>,
    keys: &EscrowKeys,
    terms: &ExpectedConfidentialTerms,
    history: &[BalanceEvent],
    leg_a_extras: &[AccountMeta],
    leg_b_extras: &[AccountMeta],
) -> Result<TransactionSession, ConfidentialError> {
    // The settlement authority re-validates every agreed term before Settle.
    let checked = verify_confidential_terms(snapshot.swap, snapshot.escrow, keys, terms)?;
    let (swap, escrow) = (checked.swap, checked.state);
    let recipient = RecipientAccount {
        address: accounts.user_a_destination_ata_b,
        owner: snapshot.recipient.owner,
        data: &snapshot.recipient.data,
    };
    let surplus_recipient = RecipientAccount {
        address: accounts.user_b_ata_b,
        owner: snapshot.surplus_recipient.owner,
        data: &snapshot.surplus_recipient.data,
    };
    settle_session(
        config,
        accounts,
        SettleRequest {
            source: TransferSource {
                state: &escrow,
                keys,
                history,
            },
            swap: &swap,
            expected_amount_b: terms.amount_b,
            recipient,
            surplus_recipient,
            // Pass recipient keys from the trade agreement to stop trusting
            // the RPC for them.
            expected_recipient: None,
            expected_surplus_recipient: None,
            mint: MintAccount {
                address: swap.base.mint_b,
                owner: snapshot.mint.owner,
                data: &snapshot.mint.data,
            },
            // Pin the auditor from application configuration (`Some(None)`
            // for none) to stop trusting the RPC for it.
            expected_auditor: None,
        },
        leg_a_extras,
        leg_b_extras,
    )
}
