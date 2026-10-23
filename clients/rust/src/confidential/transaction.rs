use super::ConfidentialError;
use bytemuck::Pod;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{v0, v1, AddressLookupTableAccount, VersionedMessage};
use solana_packet::PACKET_DATA_SIZE;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use solana_zk_elgamal_proof_interface::{
    instruction::{ContextStateInfo, ProofInstruction},
    proof_data::ZkProofData,
    state::ProofContextState,
};
use spl_record::instruction::RecordInstruction;

pub const RECORD_PROGRAM_ID: Pubkey = Pubkey::new_from_array(spl_record::ID.to_bytes());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionFormat {
    V1,
    V0,
}

/// Supply current RPC rent and already active lookup tables. V1 ignores LUTs.
pub struct SessionConfig {
    pub payer: Pubkey,
    pub format: TransactionFormat,
    pub lookup_tables: Vec<AddressLookupTableAccount>,
    pub rent: Rent,
    pub compute_unit_limit: u32,
    /// Micro-lamports per CU. V1 converts this to a total fee, rounded up.
    pub compute_unit_price: Option<u64>,
    pub loaded_accounts_data_size_limit: u32,
}

impl SessionConfig {
    pub fn new(payer: Pubkey, format: TransactionFormat, rent: Rent) -> Self {
        Self {
            payer,
            format,
            lookup_tables: vec![],
            rent,
            compute_unit_limit: 400_000,
            compute_unit_price: None,
            loaded_accounts_data_size_limit: 4_000_000,
        }
    }

    pub fn transaction_limit(&self) -> usize {
        match self.format {
            TransactionFormat::V1 => v1::MAX_TRANSACTION_SIZE,
            TransactionFormat::V0 => PACKET_DATA_SIZE,
        }
    }

    fn compile(
        &self,
        instructions: &[Instruction],
        blockhash: Hash,
    ) -> Result<VersionedMessage, ConfidentialError> {
        let error = |e: solana_message::CompileError| ConfidentialError::Transaction(e.to_string());
        let message = match self.format {
            TransactionFormat::V1 => {
                let mut config = v1::TransactionConfig::empty()
                    .with_compute_unit_limit(self.compute_unit_limit)
                    .with_loaded_accounts_data_size_limit(self.loaded_accounts_data_size_limit);
                if let Some(price) = self.compute_unit_price {
                    const MICRO_LAMPORTS_PER_LAMPORT: u128 = 1_000_000;
                    let fee = (u128::from(price) * u128::from(self.compute_unit_limit))
                        .div_ceil(MICRO_LAMPORTS_PER_LAMPORT);
                    config = config.with_priority_fee(
                        fee.try_into().map_err(|_| ConfidentialError::Arithmetic)?,
                    );
                }
                VersionedMessage::V1(
                    v1::Message::try_compile_with_config(
                        &self.payer,
                        instructions,
                        blockhash,
                        config,
                    )
                    .map_err(error)?,
                )
            }
            TransactionFormat::V0 => {
                let mut with_budget = vec![
                    ComputeBudgetInstruction::set_compute_unit_limit(self.compute_unit_limit),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        self.loaded_accounts_data_size_limit,
                    ),
                ];
                if let Some(price) = self.compute_unit_price {
                    with_budget.push(ComputeBudgetInstruction::set_compute_unit_price(price));
                }
                with_budget.extend_from_slice(instructions);
                VersionedMessage::V0(
                    v0::Message::try_compile(
                        &self.payer,
                        &with_budget,
                        &self.lookup_tables,
                        blockhash,
                    )
                    .map_err(error)?,
                )
            }
        };
        match &message {
            VersionedMessage::V1(message) => message
                .validate()
                .map_err(|e| ConfidentialError::Transaction(e.to_string()))?,
            _ => message
                .sanitize()
                .map_err(|e| ConfidentialError::Transaction(e.to_string()))?,
        }
        Ok(message)
    }

    fn wire_size(&self, instructions: &[Instruction]) -> Result<usize, ConfidentialError> {
        let message = self.compile(instructions, Hash::default())?;
        let transaction = VersionedTransaction {
            signatures: vec![
                Signature::default();
                message.header().num_required_signatures as usize
            ],
            message,
        };
        wincode::serialize(&transaction)
            .map(|v| v.len())
            .map_err(|e| ConfidentialError::Transaction(e.to_string()))
    }

    fn check_size(&self, instructions: &[Instruction]) -> Result<usize, ConfidentialError> {
        let actual = self.wire_size(instructions)?;
        let limit = self.transaction_limit();
        if actual > limit {
            return Err(ConfidentialError::TransactionTooLarge { actual, limit });
        }
        Ok(actual)
    }
}

/// Caller signs with the payer/authority; temporary account signers are owned by
/// this plan. Rebuild with a fresh blockhash at send time. Use base64 for v1 RPC.
pub struct PlannedTransaction {
    pub instructions: Vec<Instruction>,
    pub signers: Vec<Keypair>,
}

impl PlannedTransaction {
    pub fn wire_size(&self, config: &SessionConfig) -> Result<usize, ConfidentialError> {
        config.check_size(&self.instructions)
    }
    pub fn message(
        &self,
        config: &SessionConfig,
        blockhash: Hash,
    ) -> Result<VersionedMessage, ConfidentialError> {
        config.check_size(&self.instructions)?;
        config.compile(&self.instructions, blockhash)
    }
    pub fn sign(
        &self,
        config: &SessionConfig,
        blockhash: Hash,
        signers: &[&dyn Signer],
    ) -> Result<VersionedTransaction, ConfidentialError> {
        let message = self.message(config, blockhash)?;
        let required =
            &message.static_account_keys()[..message.header().num_required_signatures as usize];
        let mut all: Vec<&dyn Signer> = vec![];
        for signer in signers
            .iter()
            .copied()
            .chain(self.signers.iter().map(|s| s as &dyn Signer))
        {
            let address = signer.pubkey();
            if required.contains(&address) && !all.iter().any(|s| s.pubkey() == address) {
                all.push(signer);
            }
        }
        VersionedTransaction::try_new(message, &all)
            .map_err(|e| ConfidentialError::Transaction(e.to_string()))
    }
}

/// Confirm preparations in order before sending final_transaction. If abandoned,
/// close only the listed accounts that actually exist, using their authorities.
/// These cleanup instructions are separate from the successful final DvP path.
pub struct TransactionSession {
    pub preparation: Vec<PlannedTransaction>,
    pub final_transaction: PlannedTransaction,
    pub cleanup: Vec<Instruction>,
}

pub(crate) struct SessionBuilder<'a> {
    config: &'a SessionConfig,
    authority: Pubkey,
    preparation: Vec<PlannedTransaction>,
    cleanup: Vec<Instruction>,
}

impl<'a> SessionBuilder<'a> {
    pub(crate) fn new(config: &'a SessionConfig, authority: Pubkey) -> Self {
        Self {
            config,
            authority,
            preparation: vec![],
            cleanup: vec![],
        }
    }

    fn append(
        &mut self,
        instructions: Vec<Instruction>,
        signers: Vec<Keypair>,
    ) -> Result<(), ConfidentialError> {
        self.config.check_size(&instructions)?;
        if let Some(previous) = self.preparation.last_mut() {
            let mut combined = previous.instructions.clone();
            combined.extend_from_slice(&instructions);
            // A range proof uses ~200k CU. Keep the number of proof verifications
            // per preparation below the configured budget, in addition to size.
            let proof_cost: u32 = combined
                .iter()
                .filter(|ix| ix.program_id == solana_zk_elgamal_proof_interface::ID)
                .map(|ix| {
                    if ix.data.first()
                        == Some(&(ProofInstruction::VerifyBatchedRangeProofU128 as u8))
                    {
                        210_000
                    } else {
                        25_000
                    }
                })
                .sum();
            if proof_cost.saturating_add(30_000) <= self.config.compute_unit_limit
                && self.config.check_size(&combined).is_ok()
            {
                previous.instructions = combined;
                previous.signers.extend(signers);
                return Ok(());
            }
        }
        self.preparation.push(PlannedTransaction {
            instructions,
            signers,
        });
        Ok(())
    }

    /// Rent comes from the caller's RPC. Proof contexts close to the
    /// authority, so cap it at the default to keep the payer from overpaying.
    fn rent_exempt(&self, space: usize) -> Result<u64, ConfidentialError> {
        self.config
            .rent
            .try_minimum_balance(space)
            .filter(|lamports| *lamports <= Rent::default().minimum_balance(space))
            .ok_or_else(|| {
                ConfidentialError::Transaction(format!(
                    "rent for {space} bytes exceeds the default rent-exemption minimum"
                ))
            })
    }

    pub(crate) fn proof<T: Pod + ZkProofData<U>, U: Pod>(
        &mut self,
        kind: ProofInstruction,
        proof: &T,
    ) -> Result<Pubkey, ConfidentialError> {
        let context = Keypair::new();
        let address = context.pubkey();
        let space = core::mem::size_of::<ProofContextState<U>>();
        let create = solana_system_interface::instruction::create_account(
            &self.config.payer,
            &address,
            self.rent_exempt(space)?,
            space as u64,
            &solana_zk_elgamal_proof_interface::ID,
        );
        let authority = self.authority;
        let context_info = ContextStateInfo {
            context_state_account: &address,
            context_state_authority: &authority,
        };
        let inline = kind.encode_verify_proof(Some(context_info), proof);
        let pair = vec![create.clone(), inline];
        let use_record = self.config.format == TransactionFormat::V0
            && self.config.wire_size(&pair)? > self.config.transaction_limit();
        if use_record {
            let record = Keypair::new();
            let record_address = record.pubkey();
            let bytes = bytemuck::bytes_of(proof);
            let space = spl_record::state::RecordData::WRITABLE_START_INDEX + bytes.len();
            let create_record = solana_system_interface::instruction::create_account(
                &self.config.payer,
                &record_address,
                self.rent_exempt(space)?,
                space as u64,
                &RECORD_PROGRAM_ID,
            );
            let initialize = record_instruction(
                record_address,
                self.config.payer,
                RecordInstruction::Initialize,
            );
            self.append(vec![create_record, initialize], vec![record])?;
            let mut offset = 0;
            while offset < bytes.len() {
                let mut end = bytes.len();
                loop {
                    let write = record_instruction(
                        record_address,
                        self.config.payer,
                        RecordInstruction::Write {
                            offset: offset as u64,
                            data: &bytes[offset..end],
                        },
                    );
                    let size = self.config.wire_size(std::slice::from_ref(&write))?;
                    if size <= self.config.transaction_limit() {
                        self.append(vec![write], vec![])?;
                        break;
                    }
                    let excess = size - self.config.transaction_limit();
                    end = end.checked_sub(excess).filter(|end| *end > offset).ok_or(
                        ConfidentialError::TransactionTooLarge {
                            actual: size,
                            limit: self.config.transaction_limit(),
                        },
                    )?;
                }
                offset = end;
            }
            let close = record_close(record_address, self.config.payer);
            self.cleanup.push(close.clone());
            let verify = kind.encode_verify_proof_from_account(
                Some(context_info),
                &record_address,
                spl_record::state::RecordData::WRITABLE_START_INDEX as u32,
            );
            self.append(vec![create, verify, close], vec![context])?;
        } else {
            self.append(pair, vec![context])?;
        }
        self.cleanup.push(
            solana_zk_elgamal_proof_interface::instruction::close_context_state(
                context_info,
                &self.authority,
            ),
        );
        Ok(address)
    }

    pub(crate) fn finish(
        self,
        instructions: Vec<Instruction>,
    ) -> Result<TransactionSession, ConfidentialError> {
        self.config.check_size(&instructions)?;
        Ok(TransactionSession {
            preparation: self.preparation,
            final_transaction: PlannedTransaction {
                instructions,
                signers: vec![],
            },
            cleanup: self.cleanup,
        })
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) fn close_proof_contexts(&self) -> Vec<Instruction> {
        self.cleanup
            .iter()
            .filter(|ix| ix.program_id == solana_zk_elgamal_proof_interface::ID)
            .cloned()
            .collect()
    }
}

fn record_instruction(
    record: Pubkey,
    authority: Pubkey,
    instruction: RecordInstruction<'_>,
) -> Instruction {
    let signer = !matches!(instruction, RecordInstruction::Initialize);
    Instruction {
        program_id: RECORD_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(record, false),
            AccountMeta::new_readonly(authority, signer),
        ],
        data: instruction.pack(),
    }
}

fn record_close(record: Pubkey, authority: Pubkey) -> Instruction {
    let mut ix = record_instruction(record, authority, RecordInstruction::CloseAccount);
    ix.accounts.push(AccountMeta::new(authority, false));
    ix
}
