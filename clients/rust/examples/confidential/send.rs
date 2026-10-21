use dvp_swap_program_client::confidential::{
    PlannedTransaction, SessionConfig, TransactionSession,
};
use solana_client::{rpc_client::RpcClient, rpc_config::RpcSendTransactionConfig};
use solana_signer::Signer;
use solana_transaction_status_client_types::UiTransactionEncoding;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Use a client with the intended commitment, e.g. confirmed. Pass both payer and
/// operation authority; PlannedTransaction adds its temporary account signers.
/// Keep the session until completion so a failure does not lose its cleanup list.
pub fn send_session(
    rpc: &RpcClient,
    config: &SessionConfig,
    session: &TransactionSession,
    signers: &[&dyn Signer],
) -> Result<()> {
    for plan in &session.preparation {
        send_plan(rpc, config, plan, signers)?;
    }
    send_plan(rpc, config, &session.final_transaction, signers)
}

fn send_plan(
    rpc: &RpcClient,
    config: &SessionConfig,
    plan: &PlannedTransaction,
    signers: &[&dyn Signer],
) -> Result<()> {
    // Proofs may take time to prepare; obtain the blockhash immediately before signing.
    let transaction = plan.sign(config, rpc.get_latest_blockhash()?, signers)?;
    rpc.send_and_confirm_transaction_with_spinner_and_config(
        &transaction,
        rpc.commitment(),
        RpcSendTransactionConfig {
            encoding: Some(UiTransactionEncoding::Base64),
            preflight_commitment: Some(rpc.commitment().commitment),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// Call only after reconciling transaction status and deciding to abandon the
/// session. A send timeout does not mean the transaction failed. Close only
/// surviving accounts, one size-checked transaction at a time; this can resume
/// after partial cleanup. Include payer and operation authority as signers.
pub fn cleanup(
    rpc: &RpcClient,
    config: &SessionConfig,
    session: &TransactionSession,
    signers: &[&dyn Signer],
) -> Result<()> {
    for instruction in &session.cleanup {
        let address = instruction.accounts[0].pubkey;
        if rpc
            .get_account_with_commitment(&address, rpc.commitment())?
            .value
            .is_some()
        {
            let plan = PlannedTransaction {
                instructions: vec![instruction.clone()],
                signers: vec![],
            };
            send_plan(rpc, config, &plan, signers)?;
        }
    }
    Ok(())
}
