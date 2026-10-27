use std::collections::{BTreeMap, BTreeSet};

use bytemuck::Pod;
use solana_address::Address;
use solana_instruction::Instruction;
use solana_zk_elgamal_proof_interface::{
    instruction::ProofInstruction, proof_data::BatchedGroupedCiphertext3HandlesValidityProofContext,
};
use spl_record::{instruction::RecordInstruction, state::RecordData};
use spl_token_2022_interface::{
    extension::{
        confidential_mint_burn::instruction::{
            BurnInstructionData, ConfidentialMintBurnInstruction, MintInstructionData,
        },
        confidential_transfer::instruction::{
            ConfidentialTransferInstruction, DepositInstructionData, TransferInstructionData,
            TransferWithFeeInstructionData, WithdrawInstructionData,
        },
    },
    instruction::TokenInstruction,
};

use super::{BalanceEvent, ConfidentialError, RECORD_PROGRAM_ID};

const MAX_RECORD_LEN: usize = 10 * 1024 * 1024;

/// A successful RPC transaction with resolved account addresses. Inner
/// instructions must be in execution order, grouped by their top-level index.
/// Supply transactions chronologically, including proof preparation transactions.
pub struct ExecutedTransaction {
    pub instructions: Vec<Instruction>,
    pub inner_instructions: BTreeMap<usize, Vec<Instruction>>,
    pub succeeded: bool,
}

#[cfg(feature = "fetch")]
impl ExecutedTransaction {
    /// Decode getTransaction's Base64 response with inner instructions and
    /// loadedAddresses. Request maxSupportedTransactionVersion=1. This does not
    /// fetch history or infer ordering between transactions in the same slot.
    pub fn from_rpc(
        value: &solana_transaction_status_client_types::EncodedTransactionWithStatusMeta,
    ) -> Result<Self, ConfidentialError> {
        use solana_instruction::AccountMeta;
        use solana_transaction_status_client_types::{
            option_serializer::OptionSerializer, UiInstruction,
        };
        let meta = value
            .meta
            .as_ref()
            .ok_or(ConfidentialError::IncompleteHistory)?;
        if meta.err.is_some() {
            return Ok(Self {
                instructions: vec![],
                inner_instructions: BTreeMap::new(),
                succeeded: false,
            });
        }
        let transaction = value
            .transaction
            .decode()
            .ok_or(ConfidentialError::IncompleteHistory)?;
        let message = transaction.message;
        let mut keys = message.static_account_keys().to_vec();
        if let Some(lookups) = message.address_table_lookups().filter(|v| !v.is_empty()) {
            let OptionSerializer::Some(loaded) = &meta.loaded_addresses else {
                return Err(ConfidentialError::IncompleteHistory);
            };
            if loaded.writable.len()
                != lookups
                    .iter()
                    .map(|v| v.writable_indexes.len())
                    .sum::<usize>()
                || loaded.readonly.len()
                    != lookups
                        .iter()
                        .map(|v| v.readonly_indexes.len())
                        .sum::<usize>()
            {
                return Err(ConfidentialError::IncompleteHistory);
            }
            for key in loaded.writable.iter().chain(&loaded.readonly) {
                keys.push(
                    key.parse()
                        .map_err(|_| ConfidentialError::IncompleteHistory)?,
                );
            }
        }
        let decode = |program_index: u8,
                      accounts: &[u8],
                      data: Vec<u8>|
         -> Result<Instruction, ConfidentialError> {
            Ok(Instruction {
                program_id: *keys
                    .get(program_index as usize)
                    .ok_or(ConfidentialError::IncompleteHistory)?,
                accounts: accounts
                    .iter()
                    .map(|index| {
                        keys.get(*index as usize)
                            .copied()
                            .map(|key| AccountMeta::new_readonly(key, false))
                            .ok_or(ConfidentialError::IncompleteHistory)
                    })
                    .collect::<Result<_, _>>()?,
                data,
            })
        };
        let instructions = message
            .instructions()
            .iter()
            .map(|ix| decode(ix.program_id_index, &ix.accounts, ix.data.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let OptionSerializer::Some(inner) = &meta.inner_instructions else {
            return Err(ConfidentialError::IncompleteHistory);
        };
        let mut inner_instructions = BTreeMap::new();
        for group in inner {
            let index = group.index as usize;
            if index >= instructions.len() || inner_instructions.contains_key(&index) {
                return Err(ConfidentialError::IncompleteHistory);
            }
            let decoded = group
                .instructions
                .iter()
                .map(|ix| {
                    let UiInstruction::Compiled(ix) = ix else {
                        return Err(ConfidentialError::IncompleteHistory);
                    };
                    let data = bs58::decode(&ix.data)
                        .into_vec()
                        .map_err(|_| ConfidentialError::IncompleteHistory)?;
                    decode(ix.program_id_index, &ix.accounts, data)
                })
                .collect::<Result<_, _>>()?;
            inner_instructions.insert(index, decoded);
        }
        Ok(Self {
            instructions,
            inner_instructions,
            succeeded: true,
        })
    }
}

/// Extracts escrow balance operations from Token-2022 instructions and CPIs.
/// Retains verified contexts and SPL Record writes until their accounts close.
pub struct BalanceHistory {
    escrow: Address,
    contexts: BTreeMap<Address, Option<BatchedGroupedCiphertext3HandlesValidityProofContext>>,
    closed_records: BTreeSet<Address>,
    records: BTreeMap<Address, Vec<u8>>,
    events: Vec<BalanceEvent>,
}

impl BalanceHistory {
    pub fn new(escrow: Address) -> Self {
        Self {
            escrow,
            contexts: BTreeMap::new(),
            closed_records: BTreeSet::new(),
            records: BTreeMap::new(),
            events: vec![],
        }
    }

    pub fn events(&self) -> &[BalanceEvent] {
        &self.events
    }

    /// Failed transactions have no effect. On a decoding error this collector
    /// must be discarded; its history is incomplete.
    pub fn push(&mut self, transaction: &ExecutedTransaction) -> Result<(), ConfidentialError> {
        self.push_with_proof_accounts(transaction, |_| None)
    }

    /// Resolve proof-account bytes as they existed when the supplied ZK verify
    /// instruction executed. RPC transaction metadata does not include these
    /// historical bytes for arbitrary programs. SPL Record writes are replayed
    /// internally; the callback is only used for accounts absent from that replay.
    /// The callback is also used for inline proof offsets, so it must be repeatable.
    /// Supplied data is untrusted: recover_escrow_balance checks every resulting
    /// balance and the pending counter against the current snapshot.
    pub fn push_with_proof_accounts(
        &mut self,
        transaction: &ExecutedTransaction,
        mut resolve: impl FnMut(&Instruction) -> Option<Vec<u8>>,
    ) -> Result<(), ConfidentialError> {
        if !transaction.succeeded {
            return Ok(());
        }
        for (index, instruction) in transaction.instructions.iter().enumerate() {
            self.instruction(instruction, index, &transaction.instructions, &mut resolve)?;
            if let Some(inner) = transaction.inner_instructions.get(&index) {
                for instruction in inner {
                    self.instruction(instruction, index, &transaction.instructions, &mut resolve)?;
                }
            }
        }
        for address in std::mem::take(&mut self.closed_records) {
            self.records.remove(&address);
        }
        Ok(())
    }

    fn instruction(
        &mut self,
        ix: &Instruction,
        index: usize,
        top: &[Instruction],
        resolve: &mut dyn FnMut(&Instruction) -> Option<Vec<u8>>,
    ) -> Result<(), ConfidentialError> {
        if ix.program_id == RECORD_PROGRAM_ID {
            let address = account(ix, 0)?;
            match decode_record(&ix.data)? {
                RecordInstruction::Initialize => {
                    self.records
                        .insert(address, vec![0; RecordData::WRITABLE_START_INDEX]);
                }
                RecordInstruction::Write { offset, data } => {
                    let start = usize::try_from(offset)
                        .ok()
                        .and_then(|v| v.checked_add(RecordData::WRITABLE_START_INDEX))
                        .ok_or(ConfidentialError::IncompleteHistory)?;
                    let end = start
                        .checked_add(data.len())
                        .ok_or(ConfidentialError::IncompleteHistory)?;
                    // Solana accounts cannot exceed 10 MiB. Bound untrusted offsets
                    // before resizing the reconstructed record.
                    if end > MAX_RECORD_LEN {
                        return Err(ConfidentialError::IncompleteHistory);
                    }
                    // A transaction may write an unrelated pre-existing Record.
                    // Without initialization its bytes remain unknown; resolve
                    // them only if a later balance proof actually needs them.
                    let Some(record) = self.records.get_mut(&address) else {
                        return Ok(());
                    };
                    record.resize(record.len().max(end), 0);
                    record[start..end].copy_from_slice(data);
                }
                RecordInstruction::CloseAccount => {
                    // Closing drains lamports; data remains readable until the
                    // transaction ends, including a later ZK verification.
                    self.closed_records.insert(address);
                }
                _ => {}
            }
        } else if ix.program_id == solana_zk_elgamal_proof_interface::ID {
            if ix.data.first()
                == Some(&(ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity as u8))
            {
                // An unrelated verification may use unavailable external
                // storage. Fail only if a balance operation needs that context.
                let context = self.validity(ix, resolve).ok();
                let context_index = usize::from(ix.data.len() == 1 + core::mem::size_of::<u32>());
                // The proof program creates a context only when its authority
                // follows it; otherwise the verification leaves contexts as is.
                if let (Some(meta), Some(_authority)) = (
                    ix.accounts.get(context_index),
                    ix.accounts.get(context_index + 1),
                ) {
                    self.contexts.insert(meta.pubkey, context);
                }
            } else if ix.data.first() == Some(&(ProofInstruction::CloseContextState as u8)) {
                self.contexts.remove(&account(ix, 0)?);
            }
        } else if ix.program_id == spl_token_2022_interface::ID {
            match TokenInstruction::unpack(&ix.data) {
                Ok(TokenInstruction::ConfidentialTransferExtension) => {
                    self.token(ix, index, top, resolve)?
                }
                Ok(TokenInstruction::ConfidentialMintBurnExtension) => {
                    self.mint_burn(ix, index, top, resolve)?
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validity(
        &self,
        ix: &Instruction,
        resolve: &mut dyn FnMut(&Instruction) -> Option<Vec<u8>>,
    ) -> Result<BatchedGroupedCiphertext3HandlesValidityProofContext, ConfidentialError> {
        if ix.program_id != solana_zk_elgamal_proof_interface::ID
            || ix.data.first()
                != Some(&(ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity as u8))
        {
            return Err(ConfidentialError::IncompleteHistory);
        }
        let external;
        let bytes = if ix.data.len() == 1 + core::mem::size_of::<u32>() {
            let offset = u32::from_le_bytes(ix.data[1..].try_into().unwrap()) as usize;
            let data = match self.records.get(&account(ix, 0)?) {
                Some(data) => data,
                None => {
                    external = resolve(ix).ok_or(ConfidentialError::IncompleteHistory)?;
                    &external
                }
            };
            data.get(offset..)
                .ok_or(ConfidentialError::IncompleteHistory)?
        } else {
            ix.data
                .get(1..)
                .ok_or(ConfidentialError::IncompleteHistory)?
        };
        let len = core::mem::size_of::<BatchedGroupedCiphertext3HandlesValidityProofContext>();
        pod(bytes
            .get(..len)
            .ok_or(ConfidentialError::IncompleteHistory)?)
    }

    #[allow(clippy::too_many_arguments)]
    fn transfer_context(
        &self,
        ix: &Instruction,
        index: usize,
        top: &[Instruction],
        offsets: [i8; 3],
        fixed_accounts: usize,
        resolve: &mut dyn FnMut(&Instruction) -> Option<Vec<u8>>,
    ) -> Result<BatchedGroupedCiphertext3HandlesValidityProofContext, ConfidentialError> {
        let [equality, validity, range] = offsets;
        if validity != 0 {
            let proof_index = index
                .checked_add_signed(validity as isize)
                .ok_or(ConfidentialError::IncompleteHistory)?;
            self.validity(
                top.get(proof_index)
                    .ok_or(ConfidentialError::IncompleteHistory)?,
                resolve,
            )
        } else {
            let validity_index = fixed_accounts
                + usize::from(equality != 0 || range != 0)
                + usize::from(equality == 0);
            self.contexts
                .get(&account(ix, validity_index)?)
                .copied()
                .flatten()
                .ok_or(ConfidentialError::IncompleteHistory)
        }
    }

    fn push_amount(
        &mut self,
        context: &BatchedGroupedCiphertext3HandlesValidityProofContext,
        handle: usize,
        incoming: bool,
    ) -> Result<(), ConfidentialError> {
        let limb = |group: solana_zk_sdk_pod::encryption::grouped_elgamal::PodGroupedElGamalCiphertext3Handles| {
            group.try_extract_ciphertext(handle).map_err(|_| ConfidentialError::IncompleteHistory)?
                .try_into().map_err(|_| ConfidentialError::IncompleteHistory)
        };
        let lo = limb(context.grouped_ciphertext_lo)?;
        let hi = limb(context.grouped_ciphertext_hi)?;
        self.events.push(if incoming {
            BalanceEvent::Credit { lo, hi }
        } else {
            BalanceEvent::Debit { lo, hi }
        });
        Ok(())
    }

    fn mint_burn(
        &mut self,
        ix: &Instruction,
        index: usize,
        top: &[Instruction],
        resolve: &mut dyn FnMut(&Instruction) -> Option<Vec<u8>>,
    ) -> Result<(), ConfidentialError> {
        let kind = ConfidentialMintBurnInstruction::try_from(
            *ix.data.get(1).ok_or(ConfidentialError::IncompleteHistory)?,
        )
        .map_err(|_| ConfidentialError::IncompleteHistory)?;
        if account(ix, 0)? != self.escrow {
            return Ok(());
        }
        let (offsets, incoming) = match kind {
            ConfidentialMintBurnInstruction::Mint => {
                let data: MintInstructionData = pod(&ix.data[2..])?;
                (
                    [
                        data.equality_proof_instruction_offset,
                        data.ciphertext_validity_proof_instruction_offset,
                        data.range_proof_instruction_offset,
                    ],
                    true,
                )
            }
            ConfidentialMintBurnInstruction::Burn => {
                let data: BurnInstructionData = pod(&ix.data[2..])?;
                (
                    [
                        data.equality_proof_instruction_offset,
                        data.ciphertext_validity_proof_instruction_offset,
                        data.range_proof_instruction_offset,
                    ],
                    false,
                )
            }
            _ => return Ok(()),
        };
        let context = self.transfer_context(ix, index, top, offsets, 2, resolve)?;
        // Mint destination and burn source are both handle 0; handle 1 is supply.
        self.push_amount(&context, 0, incoming)
    }

    fn token(
        &mut self,
        ix: &Instruction,
        index: usize,
        top: &[Instruction],
        resolve: &mut dyn FnMut(&Instruction) -> Option<Vec<u8>>,
    ) -> Result<(), ConfidentialError> {
        let kind = ConfidentialTransferInstruction::try_from(
            *ix.data.get(1).ok_or(ConfidentialError::IncompleteHistory)?,
        )
        .map_err(|_| ConfidentialError::IncompleteHistory)?;
        if matches!(
            kind,
            ConfidentialTransferInstruction::Transfer
                | ConfidentialTransferInstruction::TransferWithFee
        ) {
            let source = account(ix, 0)?;
            let destination = account(ix, 2)?;
            if source != self.escrow && destination != self.escrow {
                return Ok(());
            }
            // DvP mints have no TransferFeeConfig. Token-2022 then executes
            // TransferWithFee as Transfer: same accounts, fee proofs ignored.
            let offsets = if matches!(kind, ConfidentialTransferInstruction::Transfer) {
                let data: TransferInstructionData = pod(&ix.data[2..])?;
                [
                    data.equality_proof_instruction_offset,
                    data.ciphertext_validity_proof_instruction_offset,
                    data.range_proof_instruction_offset,
                ]
            } else {
                let data: TransferWithFeeInstructionData = pod(&ix.data[2..])?;
                [
                    data.equality_proof_instruction_offset,
                    data.transfer_amount_ciphertext_validity_proof_instruction_offset,
                    data.range_proof_instruction_offset,
                ]
            };
            let context = self.transfer_context(ix, index, top, offsets, 3, resolve)?;
            for (address, handle) in [(source, 0), (destination, 1)] {
                if address != self.escrow {
                    continue;
                }
                self.push_amount(&context, handle, handle == 1)?;
            }
        } else if account(ix, 0)? == self.escrow {
            match kind {
                ConfidentialTransferInstruction::ConfigureAccount
                | ConfidentialTransferInstruction::ConfigureAccountWithRegistry => {
                    self.events.clear()
                }
                ConfidentialTransferInstruction::Deposit => {
                    self.events.push(BalanceEvent::Deposit(u64::from(
                        pod::<DepositInstructionData>(&ix.data[2..])?.amount,
                    )))
                }
                ConfidentialTransferInstruction::Withdraw => {
                    self.events.push(BalanceEvent::Withdraw(u64::from(
                        pod::<WithdrawInstructionData>(&ix.data[2..])?.amount,
                    )))
                }
                ConfidentialTransferInstruction::ApplyPendingBalance => {
                    self.events.push(BalanceEvent::Apply)
                }
                ConfidentialTransferInstruction::EmptyAccount => {
                    self.events.push(BalanceEvent::Empty)
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn account(ix: &Instruction, index: usize) -> Result<Address, ConfidentialError> {
    ix.accounts
        .get(index)
        .map(|v| v.pubkey)
        .ok_or(ConfidentialError::IncompleteHistory)
}

fn pod<T: Pod>(bytes: &[u8]) -> Result<T, ConfidentialError> {
    bytemuck::try_pod_read_unaligned(bytes).map_err(|_| ConfidentialError::IncompleteHistory)
}

fn decode_record(data: &[u8]) -> Result<RecordInstruction<'_>, ConfidentialError> {
    // SPL Record's decoder indexes Write's length and payload without bounds
    // checks. Validate those variable bytes before calling the SDK decoder.
    let empty_write = RecordInstruction::Write {
        offset: 0,
        data: &[],
    }
    .pack();
    if data.first() == empty_write.first() {
        let length_start = empty_write.len() - core::mem::size_of::<u32>();
        let length = data
            .get(length_start..empty_write.len())
            .ok_or(ConfidentialError::IncompleteHistory)?;
        let length = u32::from_le_bytes(length.try_into().unwrap()) as usize;
        if data
            .get(empty_write.len()..)
            .is_none_or(|payload| payload.len() < length)
        {
            return Err(ConfidentialError::IncompleteHistory);
        }
    }
    RecordInstruction::unpack(data).map_err(|_| ConfidentialError::IncompleteHistory)
}
