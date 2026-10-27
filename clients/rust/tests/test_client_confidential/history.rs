use crate::{
    confidential_utils::{create_wallet_account, send_v1, state, Keys},
    utils::{execute, trace, ClientFixture, TestContext, TOKEN_2022_PROGRAM_ID as TOKEN},
};
use dvp_swap_program_client::confidential::test_utils::{
    checked_available_balance, transfer_session, TransferAccounts, TransferRequest, AMOUNT_LO_BITS,
    RECORD_PROGRAM_ID,
};
use dvp_swap_program_client::confidential::*;
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_loader_v3_interface::{instruction as loader, state::UpgradeableLoaderState};
use solana_signer::Signer;
use solana_zk_elgamal_proof_interface::{
    instruction::{ContextStateInfo, ProofInstruction},
    proof_data::ZkProofData,
    state::ProofContextState,
};
use spl_record::{instruction::RecordInstruction, state::RecordData};
use spl_token_2022_interface::{
    extension::{
        confidential_mint_burn::instruction as mint_burn, confidential_transfer::instruction as ct,
        ExtensionType,
    },
    instruction as token,
    state::Mint,
};
use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;

// Arbitrary nonzero low and high limbs exercise both ciphertexts.
const TRANSFER_AMOUNT: u64 = (1 << AMOUNT_LO_BITS) + 43;

#[test]
fn history_keeps_closed_record_until_transaction_end_and_supports_recreation() {
    let (mut context, f) = funded_proof_wallet(TRANSFER_AMOUNT * 2);
    let record = Keypair::new();
    let mut replay = BalanceHistory::new(f.dvp.accounts.dvp_ata_b);

    // Close precedes verification in the same transaction; replay must retain the bytes.
    let (first, _) = transfer_with_stored_proof(&mut context, &f, &record, ProofStorage::Record);
    for tx in &first {
        replay.push(tx).unwrap();
    }

    // Recreate the same address with a new proof. No external resolver may fill in gaps.
    let (second, _) = transfer_with_stored_proof(&mut context, &f, &record, ProofStorage::Record);
    for tx in &second {
        replay.push(tx).unwrap();
    }

    let recovered =
        recover_escrow_balance(&f.source(&context), 0, &f.keys, replay.events()).unwrap();
    assert_eq!(recovered.pending, TRANSFER_AMOUNT * 2);
    assert_eq!(recovered.pending_credit_counter, 2);
}

#[test]
fn history_uses_historical_proof_bytes_from_non_record_accounts() {
    let (mut context, f) = funded_proof_wallet(TRANSFER_AMOUNT);
    let buffer = Keypair::new();
    let (transactions, bytes) =
        transfer_with_stored_proof(&mut context, &f, &buffer, ProofStorage::LoaderBuffer);

    // The buffer is already closed; only the captured historical bytes remain.
    let replay = replay_external_proof(&transactions, &f, buffer.pubkey(), &bytes);
    let recovered =
        recover_escrow_balance(&f.source(&context), 0, &f.keys, replay.events()).unwrap();
    assert_eq!(recovered.pending, TRANSFER_AMOUNT);
    assert_eq!(recovered.pending_credit_counter, 1);
}

#[test]
fn history_recovers_confidential_mint_and_burn_after_false_ae() {
    let mut context = TestContext::new();
    let config = SessionConfig::new(
        context.payer.pubkey(),
        TransactionFormat::V1,
        context.svm.get_sysvar(),
    );

    // Create a mint with confidential supply and a recipient account.
    let supply = Keys::new();
    let mint = create_confidential_mint(&mut context, &config, &supply);
    let wallet = Keypair::new();
    let keys = EscrowKeys::from_seed(&[19; 32]).unwrap();
    let wallet_keys = Keys {
        elgamal: keys.elgamal.clone(),
        ae: keys.ae.clone(),
    };
    let account = create_wallet_account(&mut context, &wallet, &mint, &wallet_keys);
    // Arbitrary nonzero limbs exercise both halves of Mint and Burn amounts.
    const MINT_AMOUNT: u64 = (3 << AMOUNT_LO_BITS) + 42;
    const BURN_AMOUNT: u64 = (1 << AMOUNT_LO_BITS) + 1;

    // Mint through real proofs and reconstruct the pending credit.
    let proof = spl_token_confidential_transfer_proof_generation::mint::mint_split_proof_data(
        &Default::default(),
        MINT_AMOUNT,
        0,
        &supply.elgamal,
        keys.elgamal.pubkey(),
        None,
    )
    .unwrap();
    let mut transactions = vec![];
    let equality_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyCiphertextCommitmentEquality,
        &proof.equality_proof_data,
        &mut transactions,
    );
    let validity_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity,
        &proof
            .ciphertext_validity_proof_data_with_ciphertext
            .proof_data,
        &mut transactions,
    );
    let range_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyBatchedRangeProofU128,
        &proof.range_proof_data,
        &mut transactions,
    );
    let ciphertext = &proof.ciphertext_validity_proof_data_with_ciphertext;
    transactions.push(run(
        &mut context,
        &config,
        vec![mint_burn::inner_confidential_mint(
            &TOKEN,
            &account,
            &mint,
            &ciphertext.ciphertext_lo,
            &ciphertext.ciphertext_hi,
            &config.payer,
            &[],
            ProofLocation::ContextStateAccount(&equality_context),
            ProofLocation::ContextStateAccount(&validity_context),
            ProofLocation::ContextStateAccount(&range_context),
            &supply.balance(MINT_AMOUNT),
        )
        .unwrap()],
        &[],
    ));
    // Decode Mint as a credit and check it against the real pending balance.
    let mut replay = BalanceHistory::new(account);
    for tx in &transactions {
        replay.push(tx).unwrap();
    }
    assert_eq!(
        recover_escrow_balance(&state(&context, &account), 0, &keys, replay.events())
            .unwrap()
            .pending,
        MINT_AMOUNT
    );

    // A false AE supplied to Apply must not prevent history-based recovery.
    let tx = run(
        &mut context,
        &config,
        vec![ct::apply_pending_balance(
            &TOKEN,
            &account,
            1,
            &wallet_keys.balance(1),
            &wallet.pubkey(),
            &[],
        )
        .unwrap()],
        &[&wallet],
    );
    replay.push(&tx).unwrap();
    assert!(matches!(
        checked_available_balance(&state(&context, &account), &keys),
        Err(ConfidentialError::BalanceMismatch)
    ));
    assert_eq!(
        recover_escrow_balance(&state(&context, &account), 0, &keys, replay.events())
            .unwrap()
            .available,
        MINT_AMOUNT
    );

    // Burn part of the balance and recover the remainder despite another false AE.
    let current = state(&context, &account);
    let burn = spl_token_confidential_transfer_proof_generation::burn::burn_split_proof_data(
        &current.available_balance.try_into().unwrap(),
        &keys.ae.encrypt(MINT_AMOUNT),
        BURN_AMOUNT,
        &keys.elgamal,
        &keys.ae,
        supply.elgamal.pubkey(),
        None,
    )
    .unwrap();
    let mut transactions = vec![];
    let equality_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyCiphertextCommitmentEquality,
        &burn.equality_proof_data,
        &mut transactions,
    );
    let validity_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity,
        &burn
            .ciphertext_validity_proof_data_with_ciphertext
            .proof_data,
        &mut transactions,
    );
    let range_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyBatchedRangeProofU128,
        &burn.range_proof_data,
        &mut transactions,
    );
    let ciphertext = &burn.ciphertext_validity_proof_data_with_ciphertext;
    transactions.push(run(
        &mut context,
        &config,
        vec![mint_burn::inner_confidential_burn(
            &TOKEN,
            &account,
            &mint,
            &wallet_keys.balance(1),
            &ciphertext.ciphertext_lo,
            &ciphertext.ciphertext_hi,
            &wallet.pubkey(),
            &[],
            ProofLocation::ContextStateAccount(&equality_context),
            ProofLocation::ContextStateAccount(&validity_context),
            ProofLocation::ContextStateAccount(&range_context),
        )
        .unwrap()],
        &[&wallet],
    ));
    // Decode Burn as a debit; stale AE must not hide the remaining balance.
    for tx in &transactions {
        replay.push(tx).unwrap();
    }
    assert!(matches!(
        checked_available_balance(&state(&context, &account), &keys),
        Err(ConfidentialError::BalanceMismatch)
    ));
    assert_eq!(
        recover_escrow_balance(&state(&context, &account), 0, &keys, replay.events())
            .unwrap()
            .available,
        MINT_AMOUNT - BURN_AMOUNT
    );
}

#[test]
fn history_rejects_missing_or_corrupt_external_proof_bytes() {
    let (mut context, f) = funded_proof_wallet(TRANSFER_AMOUNT);
    let buffer = Keypair::new();
    let (transactions, bytes) =
        transfer_with_stored_proof(&mut context, &f, &buffer, ProofStorage::LoaderBuffer);

    // Without historical bytes, the transfer's proof context cannot be recovered.
    let mut missing = BalanceHistory::new(f.dvp.accounts.dvp_ata_b);
    assert!(matches!(
        transactions.iter().try_for_each(|tx| missing.push(tx)),
        Err(ConfidentialError::IncompleteHistory)
    ));

    // Zeroed context bytes decode, but cannot reproduce the on-chain balance.
    let wrong = replay_external_proof(&transactions, &f, buffer.pubkey(), &vec![0; bytes.len()]);
    assert!(matches!(
        recover_escrow_balance(&f.source(&context), 0, &f.keys, wrong.events()),
        Err(ConfidentialError::IncompleteHistory)
    ));
}

// Proof storage helpers: each transfer writes, verifies, and closes one real account.
#[derive(Clone, Copy)]
enum ProofStorage {
    Record,
    LoaderBuffer,
}

fn funded_proof_wallet(amount: u64) -> (TestContext, ClientFixture) {
    let mut context = TestContext::new();
    let f = ClientFixture::new(&mut context, TransactionFormat::V1, 0);
    send_v1(
        &mut context,
        &[
            token::mint_to(
                &TOKEN,
                &f.dvp.accounts.mint_b,
                &f.refund,
                &f.config.payer,
                &[],
                amount,
            )
            .unwrap(),
            ct::deposit(
                &TOKEN,
                &f.refund,
                &f.dvp.accounts.mint_b,
                amount,
                6,
                &f.dvp.user_b.pubkey(),
                &[],
            )
            .unwrap(),
            ct::apply_pending_balance(
                &TOKEN,
                &f.refund,
                1,
                &f.buyer_keys.balance(amount),
                &f.dvp.user_b.pubkey(),
                &[],
            )
            .unwrap(),
        ],
        &[&f.dvp.user_b],
    )
    .unwrap();
    (context, f)
}

fn transfer_with_stored_proof(
    context: &mut TestContext,
    f: &ClientFixture,
    storage_account: &Keypair,
    storage: ProofStorage,
) -> (Vec<ExecutedTransaction>, Vec<u8>) {
    let wallet_keys = EscrowKeys {
        elgamal: f.buyer_keys.elgamal.clone(),
        ae: f.buyer_keys.ae.clone(),
        opening_lo: f.keys.opening_lo.clone(),
        opening_hi: f.keys.opening_hi.clone(),
    };
    let source = state(context, &f.refund);
    let recipient = f.source(context);
    let mut session = transfer_session(
        &f.config,
        TransferAccounts {
            authority: f.dvp.user_b.pubkey(),
            source: f.refund,
            mint: f.dvp.accounts.mint_b,
            destination: f.dvp.accounts.dvp_ata_b,
        },
        TransferRequest {
            source: TransferSource {
                state: &source,
                keys: &wallet_keys,
                history: &[],
            },
            recipient: &recipient,
            amount: TRANSFER_AMOUNT,
            auditor: None,
        },
        &[],
    )
    .unwrap();

    // Find the validity proof once, then replace its inline bytes with an account reference.
    let kind = ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity;
    let (plan, index) = session
        .preparation
        .iter_mut()
        .find_map(|plan| {
            let index = plan.instructions.iter().position(|ix| {
                ix.program_id == solana_zk_elgamal_proof_interface::ID
                    && ix.data.first() == Some(&(kind as u8))
            })?;
            Some((plan, index))
        })
        .unwrap();
    let validity = &mut plan.instructions[index];
    let address = storage_account.pubkey();
    let bytes = &validity.data[1..];
    let (offset, instructions) = match storage {
        ProofStorage::Record => {
            let offset = RecordData::WRITABLE_START_INDEX;
            let space = offset + bytes.len();
            (
                offset,
                vec![
                    solana_system_interface::instruction::create_account(
                        &f.config.payer,
                        &address,
                        f.config.rent.minimum_balance(space),
                        space as u64,
                        &RECORD_PROGRAM_ID,
                    ),
                    record_instruction(address, f.config.payer, RecordInstruction::Initialize),
                    record_instruction(
                        address,
                        f.config.payer,
                        RecordInstruction::Write {
                            offset: 0,
                            data: bytes,
                        },
                    ),
                ],
            )
        }
        ProofStorage::LoaderBuffer => {
            // The loader provides real proof storage that BalanceHistory cannot replay itself.
            let offset = UpgradeableLoaderState::size_of_buffer_metadata();
            let mut instructions = loader::create_buffer(
                &f.config.payer,
                &address,
                &f.config.payer,
                f.config.rent.minimum_balance(offset + bytes.len()),
                bytes.len(),
            )
            .unwrap();
            instructions.push(loader::write(&address, &f.config.payer, 0, bytes.to_vec()));
            (offset, instructions)
        }
    };
    let preparation = run(context, &f.config, instructions, &[storage_account]);
    let historical_bytes = context.get_account(&address).unwrap().data;
    *validity = kind.encode_verify_proof_from_account(
        Some(ContextStateInfo {
            context_state_account: &validity.accounts[0].pubkey,
            context_state_authority: &validity.accounts[1].pubkey,
        }),
        &address,
        offset as u32,
    );

    if matches!(storage, ProofStorage::Record) {
        // SPL Record Close only drains lamports; verification can still read the data.
        plan.instructions.insert(
            index,
            record_instruction(address, f.config.payer, RecordInstruction::CloseAccount),
        );
    }
    let mut transactions = vec![preparation];
    transactions.extend(execute(context, &f.config, session, &[&f.dvp.user_b]));
    if matches!(storage, ProofStorage::LoaderBuffer) {
        // A loader buffer must stay open until verification has finished.
        transactions.push(run(
            context,
            &f.config,
            vec![loader::close(
                &address,
                &f.config.payer,
                &f.config.payer,
                false,
            )],
            &[],
        ));
    }
    assert!(context.get_account(&address).is_none());
    (transactions, historical_bytes)
}

fn replay_external_proof(
    transactions: &[ExecutedTransaction],
    f: &ClientFixture,
    proof_account: Address,
    bytes: &[u8],
) -> BalanceHistory {
    let mut replay = BalanceHistory::new(f.dvp.accounts.dvp_ata_b);
    for tx in transactions {
        replay
            .push_with_proof_accounts(tx, |ix| {
                (ix.accounts.first().map(|a| a.pubkey) == Some(proof_account))
                    .then(|| bytes.to_vec())
            })
            .unwrap();
    }
    replay
}

fn record_instruction(record: Address, payer: Address, data: RecordInstruction<'_>) -> Instruction {
    let closing = matches!(data, RecordInstruction::CloseAccount);
    let mut accounts = vec![
        AccountMeta::new(record, false),
        AccountMeta::new_readonly(payer, !matches!(data, RecordInstruction::Initialize)),
    ];
    if closing {
        accounts.push(AccountMeta::new(payer, false));
    }
    Instruction {
        program_id: RECORD_PROGRAM_ID,
        accounts,
        data: data.pack(),
    }
}

// Mint/Burn setup and proof verification.
/// Token-2022 v11 reads the instructions sysvar before the proof contexts
/// whenever any Mint/Burn proof is inline, as for Transfer. With offsets
/// `[0, 0, 1]` the validity context therefore follows the sysvar.
#[test]
fn history_replays_mint_and_burn_with_an_inline_range_proof() {
    let mut context = TestContext::new();
    let config = SessionConfig::new(
        context.payer.pubkey(),
        TransactionFormat::V1,
        context.svm.get_sysvar(),
    );
    let supply = Keys::new();
    let mint = create_confidential_mint(&mut context, &config, &supply);
    let wallet = Keypair::new();
    let keys = EscrowKeys::from_seed(&[20; 32]).unwrap();
    let wallet_keys = Keys {
        elgamal: keys.elgamal.clone(),
        ae: keys.ae.clone(),
    };
    let account = create_wallet_account(&mut context, &wallet, &mint, &wallet_keys);
    const MINT_AMOUNT: u64 = (3 << AMOUNT_LO_BITS) + 42;
    const BURN_AMOUNT: u64 = (1 << AMOUNT_LO_BITS) + 1;
    let inline = std::num::NonZeroI8::new(1).unwrap();

    let proof = spl_token_confidential_transfer_proof_generation::mint::mint_split_proof_data(
        &Default::default(),
        MINT_AMOUNT,
        0,
        &supply.elgamal,
        keys.elgamal.pubkey(),
        None,
    )
    .unwrap();
    let mut transactions = vec![];
    let equality_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyCiphertextCommitmentEquality,
        &proof.equality_proof_data,
        &mut transactions,
    );
    let validity_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity,
        &proof
            .ciphertext_validity_proof_data_with_ciphertext
            .proof_data,
        &mut transactions,
    );
    let ciphertext = &proof.ciphertext_validity_proof_data_with_ciphertext;
    transactions.push(run(
        &mut context,
        &config,
        vec![
            mint_burn::inner_confidential_mint(
                &TOKEN,
                &account,
                &mint,
                &ciphertext.ciphertext_lo,
                &ciphertext.ciphertext_hi,
                &config.payer,
                &[],
                ProofLocation::ContextStateAccount(&equality_context),
                ProofLocation::ContextStateAccount(&validity_context),
                ProofLocation::InstructionOffset(inline, &proof.range_proof_data),
                &supply.balance(MINT_AMOUNT),
            )
            .unwrap(),
            ProofInstruction::VerifyBatchedRangeProofU128
                .encode_verify_proof(None, &proof.range_proof_data),
        ],
        &[],
    ));
    let mut replay = BalanceHistory::new(account);
    for tx in &transactions {
        replay.push(tx).unwrap();
    }
    assert_eq!(
        recover_escrow_balance(&state(&context, &account), 0, &keys, replay.events())
            .unwrap()
            .pending,
        MINT_AMOUNT
    );

    let tx = run(
        &mut context,
        &config,
        vec![ct::apply_pending_balance(
            &TOKEN,
            &account,
            1,
            &wallet_keys.balance(MINT_AMOUNT),
            &wallet.pubkey(),
            &[],
        )
        .unwrap()],
        &[&wallet],
    );
    replay.push(&tx).unwrap();

    let current = state(&context, &account);
    let burn = spl_token_confidential_transfer_proof_generation::burn::burn_split_proof_data(
        &current.available_balance.try_into().unwrap(),
        &keys.ae.encrypt(MINT_AMOUNT),
        BURN_AMOUNT,
        &keys.elgamal,
        &keys.ae,
        supply.elgamal.pubkey(),
        None,
    )
    .unwrap();
    let mut transactions = vec![];
    let equality_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyCiphertextCommitmentEquality,
        &burn.equality_proof_data,
        &mut transactions,
    );
    let validity_context = proof_context(
        &mut context,
        &config,
        ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity,
        &burn
            .ciphertext_validity_proof_data_with_ciphertext
            .proof_data,
        &mut transactions,
    );
    let ciphertext = &burn.ciphertext_validity_proof_data_with_ciphertext;
    transactions.push(run(
        &mut context,
        &config,
        vec![
            mint_burn::inner_confidential_burn(
                &TOKEN,
                &account,
                &mint,
                &wallet_keys.balance(MINT_AMOUNT - BURN_AMOUNT),
                &ciphertext.ciphertext_lo,
                &ciphertext.ciphertext_hi,
                &wallet.pubkey(),
                &[],
                ProofLocation::ContextStateAccount(&equality_context),
                ProofLocation::ContextStateAccount(&validity_context),
                ProofLocation::InstructionOffset(inline, &burn.range_proof_data),
            )
            .unwrap(),
            ProofInstruction::VerifyBatchedRangeProofU128
                .encode_verify_proof(None, &burn.range_proof_data),
        ],
        &[&wallet],
    ));
    for tx in &transactions {
        replay.push(tx).unwrap();
    }
    assert_eq!(
        recover_escrow_balance(&state(&context, &account), 0, &keys, replay.events())
            .unwrap()
            .available,
        MINT_AMOUNT - BURN_AMOUNT
    );
}

fn create_confidential_mint(
    context: &mut TestContext,
    config: &SessionConfig,
    supply: &Keys,
) -> Address {
    let mint = Keypair::new();
    let size = ExtensionType::try_calculate_account_len::<Mint>(&[
        ExtensionType::ConfidentialTransferMint,
        ExtensionType::ConfidentialMintBurn,
    ])
    .unwrap();
    send_v1(
        context,
        &[
            solana_system_interface::instruction::create_account(
                &config.payer,
                &mint.pubkey(),
                config.rent.minimum_balance(size),
                size as u64,
                &TOKEN,
            ),
            ct::initialize_mint(&TOKEN, &mint.pubkey(), Some(config.payer), true, None).unwrap(),
            mint_burn::initialize_mint(
                &TOKEN,
                &mint.pubkey(),
                &(*supply.elgamal.pubkey()).into(),
                &supply.balance(0),
            )
            .unwrap(),
            token::initialize_mint(&TOKEN, &mint.pubkey(), &config.payer, None, 6).unwrap(),
        ],
        &[&mint],
    )
    .unwrap();
    mint.pubkey()
}

fn proof_context<T: bytemuck::Pod + ZkProofData<U>, U: bytemuck::Pod>(
    context: &mut TestContext,
    config: &SessionConfig,
    kind: ProofInstruction,
    proof: &T,
    history: &mut Vec<ExecutedTransaction>,
) -> Address {
    let account = Keypair::new();
    let address = account.pubkey();
    let size = core::mem::size_of::<ProofContextState<U>>();
    history.push(run(
        context,
        config,
        vec![
            solana_system_interface::instruction::create_account(
                &config.payer,
                &address,
                config.rent.minimum_balance(size),
                size as u64,
                &solana_zk_elgamal_proof_interface::ID,
            ),
            kind.encode_verify_proof(
                Some(ContextStateInfo {
                    context_state_account: &address,
                    context_state_authority: &config.payer,
                }),
                proof,
            ),
        ],
        &[&account],
    ));
    address
}

// Send a transaction and retain the instructions needed for history replay.
fn run(
    context: &mut TestContext,
    config: &SessionConfig,
    instructions: Vec<Instruction>,
    signers: &[&dyn Signer],
) -> ExecutedTransaction {
    context.svm.expire_blockhash();
    let plan = PlannedTransaction {
        instructions,
        signers: vec![],
    };
    let mut all: Vec<&dyn Signer> = vec![&context.payer];
    all.extend_from_slice(signers);
    let tx = plan
        .sign(config, context.svm.latest_blockhash(), &all)
        .unwrap();
    let message = tx.message.clone();
    let meta = context.svm.send_transaction(tx).unwrap();
    trace(&message, config, &meta)
}
