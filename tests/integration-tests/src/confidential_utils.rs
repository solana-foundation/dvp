//! Shared setup for confidential lifecycle and PDA fixture tests.
//! Key derivation and transaction packing belong to the client stage; these
//! tests use random keys and one proof per tx.
use std::{mem::size_of, num::NonZeroI8};

use dvp_swap_program_client::instructions::{
    ApplyConfidentialDvpBuilder, CancelConfidentialDvp, CancelConfidentialDvpInstructionArgs,
    CreateConfidentialDvp, CreateConfidentialDvpInstructionArgs, ReclaimConfidentialDvp,
    ReclaimConfidentialDvpInstructionArgs, RecoverConfidentialDvp,
    RecoverConfidentialDvpInstructionArgs, RejectConfidentialDvp,
    RejectConfidentialDvpInstructionArgs,
};
use dvp_swap_program_client::types::{CtTransferData, LegBRefund};
use litesvm::types::{FailedTransactionMetadata, TransactionMetadata};
use solana_account::Account;
use solana_instruction::{error::InstructionError, AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{v1, VersionedMessage};
use solana_pubkey::{pubkey, Pubkey};
use solana_signer::Signer;
use solana_transaction::{versioned::VersionedTransaction, TransactionError};
use solana_zk_elgamal_proof_interface::{
    instruction::{ContextStateInfo, ProofInstruction},
    proof_data::ZkProofData,
    state::ProofContextState,
};
use solana_zk_sdk::{
    encryption::{
        auth_encryption::AeKey,
        elgamal::{ElGamalCiphertext, ElGamalKeypair, ElGamalPubkey},
        pedersen::PedersenOpening,
    },
    zk_elgamal_proof_program::{
        build_ciphertext_ciphertext_equality_proof_data, build_pubkey_validity_proof_data,
        build_zero_ciphertext_proof_data,
    },
};
use solana_zk_sdk_pod::encryption::auth_encryption::PodAeCiphertext;
use spl_token_2022_interface::{
    extension::{
        confidential_transfer::{
            instruction as ct_ix, ConfidentialTransferAccount, ConfidentialTransferMint,
        },
        BaseStateWithExtensions, BaseStateWithExtensionsMut, ExtensionType, StateWithExtensions,
        StateWithExtensionsMut,
    },
    instruction as token_ix,
    state::{Account as TokenAccount, Mint},
};
use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;
use spl_token_confidential_transfer_proof_generation::{
    transfer::{transfer_split_proof_data, TransferProofData},
    try_combine_lo_hi_ciphertexts, try_split_u64,
};

use crate::{
    state_utils::{AMOUNT_A, AMOUNT_B},
    utils::{
        create_ata, dvp_ata, fund_wallet_ata, hook_extras_for_mint, nonce_tombstone_pda,
        set_hook_extra_account_metas, set_mint, swap_dvp_pda, TestContext, HOOK_FIXTURE_PROGRAM_ID,
        MEMO_PROGRAM_ID, TOKEN_2022_PROGRAM_ID as TOKEN, TOKEN_PROGRAM_ID,
    },
};

pub const FIXTURE: Pubkey = pubkey!("FCDHD6mkL4c7hMLxdLy2a9aCjraYJJwpF6gnbukwHRV5");
pub use solana_zk_elgamal_proof_interface::ID as ZK;
pub const LO_BITS: usize = 16;
pub const MAX_PENDING: u64 = 65_536;
pub const DECIMALS: u8 = 6;

#[path = "../../transfer-hook-fixture/src/constants.rs"]
mod hook_constants;

const REJECT_CT_HOOK: Pubkey = Pubkey::new_from_array(hook_constants::REJECT_CT_PROGRAM_ID);

/// Change a funded mint's hook using its real authority, with a matching EAML.
pub fn switch_to_rejecting_hook(context: &mut TestContext, mint: &Pubkey) -> Vec<AccountMeta> {
    use spl_tlv_account_resolution::state::ExtraAccountMetaList;
    use spl_transfer_hook_interface::{
        get_extra_account_metas_address, instruction::ExecuteInstruction,
    };

    context
        .svm
        .add_program(
            REJECT_CT_HOOK,
            include_bytes!("../../../target/deploy/transfer_hook_fixture.so"),
        )
        .unwrap();
    let validation = get_extra_account_metas_address(mint, &REJECT_CT_HOOK);
    let mut data = vec![0; ExtraAccountMetaList::size_of(0).unwrap()];
    ExtraAccountMetaList::init::<ExecuteInstruction>(&mut data, &[]).unwrap();
    context
        .svm
        .set_account(
            validation,
            Account {
                lamports: context.svm.minimum_balance_for_rent_exemption(data.len()),
                data,
                owner: REJECT_CT_HOOK,
                ..Account::default()
            },
        )
        .unwrap();
    let update = spl_token_2022_interface::extension::transfer_hook::instruction::update(
        &TOKEN,
        mint,
        &context.payer.pubkey(),
        &[],
        Some(REJECT_CT_HOOK),
    )
    .unwrap();
    send_v1(context, &[update], &[]).unwrap();
    vec![
        AccountMeta::new_readonly(REJECT_CT_HOOK, false),
        AccountMeta::new_readonly(validation, false),
    ]
}

pub fn send_and_assert_hook_rejection(
    context: &mut TestContext,
    ix: Instruction,
    signer: &Keypair,
) {
    let before: Vec<_> = ix
        .accounts
        .iter()
        .map(|a| (a.pubkey, context.get_account(&a.pubkey)))
        .collect();
    let failure = send_v1(context, &[ix], &[signer]).unwrap_err();
    assert_eq!(
        failure.err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(hook_constants::REJECT_CT_ERROR),
        ),
        "{:?}",
        failure.meta.logs
    );
    assert!(failure
        .meta
        .logs
        .iter()
        .any(|s| s.contains("hook rejects confidential sentinel")));
    assert!(failure
        .meta
        .logs
        .contains(&format!("Program {REJECT_CT_HOOK} invoke [3]")));
    // The fee payer is separate from these instruction accounts. Preparatory
    // proof contexts must survive the failed final transaction unchanged too.
    for (key, account) in before {
        assert_ne!(key, context.payer.pubkey());
        assert_eq!(context.get_account(&key), account, "{key}");
    }
}

pub fn send_v1(
    context: &mut TestContext,
    instructions: &[Instruction],
    signers: &[&Keypair],
) -> Result<TransactionMetadata, Box<FailedTransactionMetadata>> {
    context.svm.expire_blockhash();
    // Agave v1 requires both limits explicitly; Token-2022 alone loads 1.3 MB.
    let config = v1::TransactionConfig::empty()
        .with_compute_unit_limit(400_000)
        .with_loaded_accounts_data_size_limit(4_000_000);
    let message = v1::Message::try_compile_with_config(
        &context.payer.pubkey(),
        instructions,
        context.svm.latest_blockhash(),
        config,
    )
    .unwrap();
    let mut all_signers = vec![&context.payer];
    all_signers.extend_from_slice(signers);
    let transaction =
        VersionedTransaction::try_new(VersionedMessage::V1(message), &all_signers).unwrap();
    let size = wincode::serialize(&transaction).unwrap().len();
    if size > v1::MAX_TRANSACTION_SIZE {
        return Err(Box::new(FailedTransactionMetadata {
            err: TransactionError::SanitizeFailure,
            meta: TransactionMetadata {
                logs: vec![format!(
                    "Transaction size {size} exceeds v1 limit {}",
                    v1::MAX_TRANSACTION_SIZE
                )],
                ..TransactionMetadata::default()
            },
        }));
    }
    context.svm.send_transaction(transaction).map_err(Box::new)
}

pub struct Keys {
    pub elgamal: ElGamalKeypair,
    pub ae: AeKey,
}
impl Keys {
    pub fn new() -> Self {
        Self {
            elgamal: ElGamalKeypair::new_rand(),
            ae: AeKey::new_rand(),
        }
    }
    pub fn balance(&self, amount: u64) -> PodAeCiphertext {
        self.ae.encrypt(amount).into()
    }
}

pub fn state(context: &TestContext, address: &Pubkey) -> ConfidentialTransferAccount {
    let account = context.get_account(address).unwrap();
    *StateWithExtensions::<TokenAccount>::unpack(&account.data)
        .unwrap()
        .get_extension::<ConfidentialTransferAccount>()
        .unwrap()
}

pub fn pending(context: &TestContext, address: &Pubkey, keys: &Keys) -> u64 {
    let state = state(context, address);
    let lo: ElGamalCiphertext = state.pending_balance_lo.try_into().unwrap();
    let hi: ElGamalCiphertext = state.pending_balance_hi.try_into().unwrap();
    lo.decrypt_u32(keys.elgamal.secret()).unwrap()
        + (hi.decrypt_u32(keys.elgamal.secret()).unwrap() << LO_BITS)
}

pub fn context_account<T: bytemuck::Pod + ZkProofData<U>, U: bytemuck::Pod>(
    context: &mut TestContext,
    authority: &Pubkey,
    instruction: ProofInstruction,
    proof: &T,
) -> Pubkey {
    let keypair = Keypair::new();
    let space = size_of::<ProofContextState<U>>();
    let instructions = [
        solana_system_interface::instruction::create_account(
            &context.payer.pubkey(),
            &keypair.pubkey(),
            context.svm.minimum_balance_for_rent_exemption(space),
            space as u64,
            &ZK,
        ),
        instruction.encode_verify_proof(
            Some(ContextStateInfo {
                context_state_account: &keypair.pubkey(),
                context_state_authority: authority,
            }),
            proof,
        ),
    ];
    send_v1(context, &instructions, &[&keypair]).unwrap();
    keypair.pubkey()
}

pub fn transfer_contexts(
    context: &mut TestContext,
    authority: &Pubkey,
    proof: &TransferProofData,
) -> [Pubkey; 3] {
    [
        context_account(
            context,
            authority,
            ProofInstruction::VerifyCiphertextCommitmentEquality,
            &proof.equality_proof_data,
        ),
        context_account(
            context,
            authority,
            ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity,
            &proof
                .ciphertext_validity_proof_data_with_ciphertext
                .proof_data,
        ),
        context_account(
            context,
            authority,
            ProofInstruction::VerifyBatchedRangeProofU128,
            &proof.range_proof_data,
        ),
    ]
}

pub fn create_wallet_account(
    context: &mut TestContext,
    wallet: &Keypair,
    mint: &Pubkey,
    keys: &Keys,
) -> Pubkey {
    let address = create_ata(context, &wallet.pubkey(), mint, &TOKEN);
    let proof = build_pubkey_validity_proof_data(&keys.elgamal).unwrap();
    let mut instructions = vec![token_ix::reallocate(
        &TOKEN,
        &address,
        &context.payer.pubkey(),
        &wallet.pubkey(),
        &[],
        &[ExtensionType::ConfidentialTransferAccount],
    )
    .unwrap()];
    instructions.extend(
        ct_ix::configure_account(
            &TOKEN,
            &address,
            mint,
            &keys.balance(0),
            MAX_PENDING,
            &wallet.pubkey(),
            &[],
            ProofLocation::InstructionOffset(NonZeroI8::new(1).unwrap(), &proof),
        )
        .unwrap(),
    );
    send_v1(context, &instructions, &[wallet]).unwrap();
    address
}

/// A funded escrow owned by the CPI test program, ready for proof/helper tests.
pub struct ConfidentialTransferTestContext {
    pub context: TestContext,
    pub mint: Pubkey,
    pub authority: Pubkey,
    pub escrow: Pubkey,
    pub escrow_keys: Keys,
    pub recipient: Keypair,
    pub destination: Pubkey,
    pub recipient_keys: Keys,
    pub proof_authority: Keypair,
    source_owner: Keypair,
    source: Pubkey,
    source_keys: Keys,
    pub extras: Vec<AccountMeta>,
}

impl ConfidentialTransferTestContext {
    pub fn new(amount: u64, hook: bool) -> Self {
        let mut context = TestContext::new();
        context
            .svm
            .add_program(
                FIXTURE,
                include_bytes!("../../../target/deploy/confidential_transfer_fixture.so"),
            )
            .unwrap();
        let mint = create_confidential_mint(&mut context, true, hook);
        let source_owner = Keypair::new();
        let source_keys = Keys::new();
        let source = create_wallet_account(&mut context, &source_owner, &mint, &source_keys);
        let recipient = Keypair::new();
        let recipient_keys = Keys::new();
        let destination = create_wallet_account(&mut context, &recipient, &mint, &recipient_keys);
        let authority = Pubkey::find_program_address(&[b"ct-test"], &FIXTURE).0;
        let escrow = create_ata(&mut context, &authority, &mint, &TOKEN);
        let escrow_keys = Keys::new();
        let proof = build_pubkey_validity_proof_data(&escrow_keys.elgamal).unwrap();
        let mut data = vec![0];
        data.extend_from_slice(&escrow_keys.balance(0).0);
        data.push((-1i8) as u8);
        let instructions = [
            ProofInstruction::VerifyPubkeyValidity.encode_verify_proof(None, &proof),
            Instruction {
                program_id: FIXTURE,
                accounts: vec![
                    AccountMeta::new(context.payer.pubkey(), true),
                    AccountMeta::new_readonly(authority, false),
                    AccountMeta::new(escrow, false),
                    AccountMeta::new_readonly(mint, false),
                    AccountMeta::new_readonly(solana_sdk_ids::sysvar::instructions::ID, false),
                    AccountMeta::new_readonly(TOKEN, false),
                    AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
                ],
                data,
            },
        ];
        send_v1(&mut context, &instructions, &[]).unwrap();
        let mut ct_context = Self {
            context,
            mint,
            authority,
            escrow,
            escrow_keys,
            recipient,
            destination,
            recipient_keys,
            proof_authority: Keypair::new(),
            source_owner,
            source,
            source_keys,
            extras: vec![],
        };
        // Only the EAML is injected; mint and token accounts are created by their programs.
        if hook {
            ct_context.set_hook_extras(&[AccountMeta::new_readonly(
                solana_sdk_ids::system_program::ID,
                false,
            )]);
        }
        ct_context.fund(amount);
        let mut data = vec![1];
        data.extend_from_slice(&1u64.to_le_bytes());
        data.extend_from_slice(&ct_context.escrow_keys.balance(amount).0);
        let apply = Instruction {
            program_id: FIXTURE,
            accounts: vec![
                AccountMeta::new_readonly(authority, false),
                AccountMeta::new(escrow, false),
                AccountMeta::new_readonly(TOKEN, false),
            ],
            data,
        };
        send_v1(&mut ct_context.context, &[apply], &[]).unwrap();
        ct_context
    }

    pub fn set_hook_extras(&mut self, extras: &[AccountMeta]) {
        let address = set_hook_extra_account_metas(&mut self.context, &self.mint, extras);
        self.extras = extras.to_vec();
        self.extras.extend([
            AccountMeta::new_readonly(HOOK_FIXTURE_PROGRAM_ID, false),
            AccountMeta::new_readonly(address, false),
        ]);
    }

    /// Ordinary wallet CT funding, including a credit arriving after proof prep.
    pub fn fund(&mut self, amount: u64) {
        let instructions = prepare_funding_transfer(
            &mut self.context,
            &self.mint,
            &self.source_owner,
            &self.source,
            &self.source_keys,
            &self.escrow,
            &self.escrow_keys,
            amount,
            &self.extras,
        );
        send_v1(&mut self.context, &instructions, &[&self.source_owner]).unwrap();
    }

    /// A full payment with two independently encrypted stored amount limbs.
    pub fn prepare(&mut self, amount: u64) -> Payment {
        let source = state(&self.context, &self.escrow);
        let available: ElGamalCiphertext = source.available_balance.try_into().unwrap();
        let proof = transfer_split_proof_data(
            &available,
            &source.decryptable_available_balance.try_into().unwrap(),
            amount,
            &self.escrow_keys.elgamal,
            &self.escrow_keys.ae,
            self.recipient_keys.elgamal.pubkey(),
            None,
        )
        .unwrap();
        let authority = self.proof_authority.pubkey();
        let [equality, validity, range] = transfer_contexts(&mut self.context, &authority, &proof);
        let ciphertext = &proof.ciphertext_validity_proof_data_with_ciphertext;
        let validity_data = ciphertext.proof_data.context_data();
        let lo: ElGamalCiphertext = validity_data
            .grouped_ciphertext_lo
            .try_extract_ciphertext(0)
            .unwrap()
            .try_into()
            .unwrap();
        let hi: ElGamalCiphertext = validity_data
            .grouped_ciphertext_hi
            .try_extract_ciphertext(0)
            .unwrap()
            .try_into()
            .unwrap();
        let (amount_lo, amount_hi) = try_split_u64(amount, LO_BITS).unwrap();
        let opening_lo = PedersenOpening::new_rand();
        let opening_hi = PedersenOpening::new_rand();
        let stored_lo = self
            .escrow_keys
            .elgamal
            .pubkey()
            .encrypt_with(amount_lo, &opening_lo);
        let stored_hi = self
            .escrow_keys
            .elgamal
            .pubkey()
            .encrypt_with(amount_hi, &opening_hi);
        let eq_lo = build_ciphertext_ciphertext_equality_proof_data(
            &self.escrow_keys.elgamal,
            self.escrow_keys.elgamal.pubkey(),
            &lo,
            &stored_lo,
            &opening_lo,
            amount_lo,
        )
        .unwrap();
        let eq_hi = build_ciphertext_ciphertext_equality_proof_data(
            &self.escrow_keys.elgamal,
            self.escrow_keys.elgamal.pubkey(),
            &hi,
            &stored_hi,
            &opening_hi,
            amount_hi,
        )
        .unwrap();
        let eq_lo = context_account(
            &mut self.context,
            &authority,
            ProofInstruction::VerifyCiphertextCiphertextEquality,
            &eq_lo,
        );
        let eq_hi = context_account(
            &mut self.context,
            &authority,
            ProofInstruction::VerifyCiphertextCiphertextEquality,
            &eq_hi,
        );
        let remaining = available - try_combine_lo_hi_ciphertexts(&lo, &hi, LO_BITS).unwrap();
        let zero = build_zero_ciphertext_proof_data(&self.escrow_keys.elgamal, &remaining).unwrap();
        let zero = context_account(
            &mut self.context,
            &authority,
            ProofInstruction::VerifyZeroCiphertext,
            &zero,
        );
        let contexts = [eq_lo, eq_hi, equality, validity, range, zero];
        let mut data = vec![2];
        data.extend_from_slice(&self.escrow_keys.balance(0).0);
        data.extend_from_slice(&ciphertext.ciphertext_lo.0);
        data.extend_from_slice(&ciphertext.ciphertext_hi.0);
        data.extend_from_slice(&stored_lo.to_bytes());
        data.extend_from_slice(&stored_hi.to_bytes());
        let mut accounts = vec![
            AccountMeta::new_readonly(self.authority, false),
            AccountMeta::new(self.escrow, false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new(self.destination, false),
            AccountMeta::new(authority, true),
        ];
        accounts.extend(contexts.map(|address| AccountMeta::new(address, false)));
        accounts.extend([
            AccountMeta::new_readonly(TOKEN, false),
            AccountMeta::new_readonly(ZK, false),
            AccountMeta::new_readonly(MEMO_PROGRAM_ID, false),
        ]);
        accounts.extend_from_slice(&self.extras);
        Payment {
            instruction: Instruction {
                program_id: FIXTURE,
                accounts,
                data,
            },
            contexts,
        }
    }

    pub fn pay(
        &mut self,
        payment: &Payment,
    ) -> Result<TransactionMetadata, Box<FailedTransactionMetadata>> {
        send_v1(
            &mut self.context,
            std::slice::from_ref(&payment.instruction),
            &[&self.proof_authority],
        )
    }
}

pub struct Payment {
    pub instruction: Instruction,
    pub contexts: [Pubkey; 6],
}

/// Inputs for the real DvP program; the swap and escrows are created explicitly.
pub struct ConfidentialDvpFixture {
    pub user_a: Keypair,
    pub user_b: Keypair,
    pub authority: Keypair,
    pub keys: Keys,
    pub amount_b_openings: [PedersenOpening; 2],
    pub accounts: CreateConfidentialDvp,
    pub args: CreateConfidentialDvpInstructionArgs,
}

impl ConfidentialDvpFixture {
    pub fn new(context: &mut TestContext, auto_approve: bool, hook: bool) -> Self {
        let user_a = Keypair::new();
        let user_b = Keypair::new();
        let authority = Keypair::new();
        let mint_a = Pubkey::new_unique();
        set_mint(context, &mint_a, &TOKEN_PROGRAM_ID);
        let mint_b = create_confidential_mint(context, auto_approve, hook);
        let nonce = 42;
        let (swap, _) = swap_dvp_pda(
            &authority.pubkey(),
            &user_a.pubkey(),
            &user_b.pubkey(),
            &mint_a,
            &mint_b,
            nonce,
        );
        let keys = Keys::new();
        let amount_b_openings = [PedersenOpening::new_rand(), PedersenOpening::new_rand()];
        let args = CreateConfidentialDvpInstructionArgs {
            amount_a: AMOUNT_A,
            expiry_timestamp: context.now() + 3600,
            nonce,
            amount_b_ciphertext_lo: keys
                .elgamal
                .pubkey()
                .encrypt_with(AMOUNT_B & ((1 << LO_BITS) - 1), &amount_b_openings[0])
                .to_bytes(),
            amount_b_ciphertext_hi: keys
                .elgamal
                .pubkey()
                .encrypt_with(AMOUNT_B >> LO_BITS, &amount_b_openings[1])
                .to_bytes(),
            decryptable_zero_balance: keys.balance(0).0,
            pubkey_validity_proof_offset: -1,
            ref_string: None,
            user_a_settlement_destination: None,
            user_b_settlement_destination: None,
            earliest_settlement_timestamp: None,
        };
        let accounts = CreateConfidentialDvp {
            payer: context.payer.pubkey(),
            swap_dvp: swap,
            nonce_tombstone: nonce_tombstone_pda(&swap).0,
            settlement_authority: authority.pubkey(),
            user_a: user_a.pubkey(),
            user_b: user_b.pubkey(),
            mint_a,
            mint_b,
            dvp_ata_a: dvp_ata(&swap, &mint_a, &TOKEN_PROGRAM_ID),
            dvp_ata_b: dvp_ata(&swap, &mint_b, &TOKEN),
            token_program_a: TOKEN_PROGRAM_ID,
            token_program_b: TOKEN,
            instructions_sysvar: solana_sdk_ids::sysvar::instructions::ID,
            system_program: solana_sdk_ids::system_program::ID,
            associated_token_program: spl_associated_token_account_interface::program::ID,
        };
        Self {
            user_a,
            user_b,
            authority,
            keys,
            amount_b_openings,
            accounts,
            args,
        }
    }

    pub fn create_instructions(&self) -> [Instruction; 2] {
        let proof = build_pubkey_validity_proof_data(&self.keys.elgamal).unwrap();
        [
            ProofInstruction::VerifyPubkeyValidity.encode_verify_proof(None, &proof),
            self.accounts.instruction(self.args.clone()),
        ]
    }

    pub fn apply(&self, signer: Pubkey, amount: u64) -> ApplyConfidentialDvpBuilder {
        let mut builder = ApplyConfidentialDvpBuilder::new();
        builder
            .signer(signer)
            .swap_dvp(self.accounts.swap_dvp)
            .nonce_tombstone(self.accounts.nonce_tombstone)
            .dvp_ata_b(self.accounts.dvp_ata_b)
            .token_program(TOKEN)
            .expected_pending_balance_credit_counter(1)
            .new_decryptable_available_balance(self.keys.balance(amount).0)
            .settlement_authority(self.authority.pubkey())
            .user_a(self.user_a.pubkey())
            .user_b(self.user_b.pubkey())
            .mint_a(self.accounts.mint_a)
            .mint_b(self.accounts.mint_b)
            .nonce(self.args.nonce);
        builder
    }

    pub fn create(&self, context: &mut TestContext) {
        send_v1(context, &self.create_instructions(), &[]).unwrap();
    }

    pub fn close_fixture(&self, context: &mut TestContext) {
        // Only the live swap is removed. Keep its real tombstone and CT escrow.
        // Real Settle -> Apply is covered by the settlement tests.
        context
            .svm
            .set_account(self.accounts.swap_dvp, Account::default())
            .unwrap();
    }
}

pub fn assert_error(
    context: &mut TestContext,
    instructions: &[Instruction],
    signers: &[&Keypair],
    instruction_index: u8,
    expected: InstructionError,
) {
    let err = send_v1(context, instructions, signers).unwrap_err();
    assert_eq!(
        format!("{:?}", err.err),
        format!("InstructionError({instruction_index}, {expected:?})"),
        "{:?}",
        err.meta.logs
    );
}

pub fn create_confidential_mint(
    context: &mut TestContext,
    auto_approve: bool,
    hook: bool,
) -> Pubkey {
    let mint = Keypair::new();
    let mut extensions = vec![ExtensionType::ConfidentialTransferMint];
    if hook {
        extensions.push(ExtensionType::TransferHook);
    }
    let space = ExtensionType::try_calculate_account_len::<Mint>(&extensions).unwrap();
    let mut instructions = vec![
        solana_system_interface::instruction::create_account(
            &context.payer.pubkey(),
            &mint.pubkey(),
            context.svm.minimum_balance_for_rent_exemption(space),
            space as u64,
            &TOKEN,
        ),
        ct_ix::initialize_mint(
            &TOKEN,
            &mint.pubkey(),
            Some(context.payer.pubkey()),
            auto_approve,
            None,
        )
        .unwrap(),
    ];
    if hook {
        instructions.push(
            spl_token_2022_interface::extension::transfer_hook::instruction::initialize(
                &TOKEN,
                &mint.pubkey(),
                Some(context.payer.pubkey()),
                Some(HOOK_FIXTURE_PROGRAM_ID),
            )
            .unwrap(),
        );
    }
    instructions.push(
        token_ix::initialize_mint(
            &TOKEN,
            &mint.pubkey(),
            &context.payer.pubkey(),
            None,
            DECIMALS,
        )
        .unwrap(),
    );
    send_v1(context, &instructions, &[&mint]).unwrap();
    mint.pubkey()
}

/// Funds a wallet's available balance and prepares a real CT transfer plus proof cleanup.
/// The caller submits it separately so rejection and retry can be tested.
#[allow(clippy::too_many_arguments)]
pub fn prepare_funding_transfer(
    context: &mut TestContext,
    mint: &Pubkey,
    source_owner: &Keypair,
    source_address: &Pubkey,
    source_keys: &Keys,
    destination: &Pubkey,
    destination_keys: &Keys,
    amount: u64,
    extras: &[AccountMeta],
) -> Vec<Instruction> {
    let before = state(context, source_address);
    let instructions = [
        token_ix::mint_to(
            &TOKEN,
            mint,
            source_address,
            &context.payer.pubkey(),
            &[],
            amount,
        )
        .unwrap(),
        ct_ix::deposit(
            &TOKEN,
            source_address,
            mint,
            amount,
            DECIMALS,
            &source_owner.pubkey(),
            &[],
        )
        .unwrap(),
        ct_ix::apply_pending_balance(
            &TOKEN,
            source_address,
            u64::from(before.pending_balance_credit_counter) + 1,
            &source_keys.balance(amount),
            &source_owner.pubkey(),
            &[],
        )
        .unwrap(),
    ];
    send_v1(context, &instructions, &[source_owner]).unwrap();
    let source = state(context, source_address);
    let proof = transfer_split_proof_data(
        &source.available_balance.try_into().unwrap(),
        &source.decryptable_available_balance.try_into().unwrap(),
        amount,
        &source_keys.elgamal,
        &source_keys.ae,
        destination_keys.elgamal.pubkey(),
        mint_auditor(context, mint).as_ref(),
    )
    .unwrap();
    let [equality, validity, range] = transfer_contexts(context, &source_owner.pubkey(), &proof);
    let ciphertext = &proof.ciphertext_validity_proof_data_with_ciphertext;
    let mut instructions = ct_ix::transfer(
        &TOKEN,
        source_address,
        mint,
        destination,
        &source_keys.balance(0),
        &ciphertext.ciphertext_lo,
        &ciphertext.ciphertext_hi,
        &source_owner.pubkey(),
        &[],
        ProofLocation::ContextStateAccount(&equality),
        ProofLocation::ContextStateAccount(&validity),
        ProofLocation::ContextStateAccount(&range),
    )
    .unwrap();
    instructions[0].accounts.extend_from_slice(extras);
    for account in [equality, validity, range] {
        instructions.push(
            solana_zk_elgamal_proof_interface::instruction::close_context_state(
                ContextStateInfo {
                    context_state_account: &account,
                    context_state_authority: &source_owner.pubkey(),
                },
                &context.payer.pubkey(),
            ),
        );
    }
    instructions
}

pub fn setup_refund(
    context: &mut TestContext,
    amount_a: u64,
    amount_b: u64,
    hook: bool,
) -> (ConfidentialDvpFixture, Keys) {
    let f = ConfidentialDvpFixture::new(context, true, hook);
    f.create(context);
    let source_a = fund_wallet_ata(
        context,
        &f.user_a,
        &f.accounts.mint_a,
        amount_a,
        &TOKEN_PROGRAM_ID,
    );
    if amount_a > 0 {
        let ix = spl_token_interface::instruction::transfer_checked(
            &TOKEN_PROGRAM_ID,
            &source_a,
            &f.accounts.mint_a,
            &f.accounts.dvp_ata_a,
            &f.user_a.pubkey(),
            &[],
            amount_a,
            DECIMALS,
        )
        .unwrap();
        send_v1(context, &[ix], &[&f.user_a]).unwrap();
    }
    let keys = Keys::new();
    create_wallet_account(context, &f.user_b, &f.accounts.mint_b, &keys);
    if hook {
        set_hook_extra_account_metas(context, &f.accounts.mint_b, &[]);
    }
    if amount_b > 0 {
        fund_b(context, &f, &keys, amount_b, hook);
        send_v1(
            context,
            &[f.apply(f.user_b.pubkey(), amount_b).instruction()],
            &[&f.user_b],
        )
        .unwrap();
    }
    (f, keys)
}

pub fn fund_b(
    context: &mut TestContext,
    f: &ConfidentialDvpFixture,
    keys: &Keys,
    amount: u64,
    hook: bool,
) {
    let extras = if hook {
        hook_extras_for_mint(&f.accounts.mint_b)
    } else {
        vec![]
    };
    let ixs = prepare_funding_transfer(
        context,
        &f.accounts.mint_b,
        &f.user_b,
        &dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN),
        keys,
        &f.accounts.dvp_ata_b,
        &f.keys,
        amount,
        &extras,
    );
    send_v1(context, &ixs, &[&f.user_b]).unwrap();
}

// A third party can credit pending without reading or signing the swap.
pub fn fund_late_b(context: &mut TestContext, f: &ConfidentialDvpFixture, amount: u64) {
    let donor = Keypair::new();
    let keys = Keys::new();
    let source = create_wallet_account(context, &donor, &f.accounts.mint_b, &keys);
    let instructions = prepare_funding_transfer(
        context,
        &f.accounts.mint_b,
        &donor,
        &source,
        &keys,
        &f.accounts.dvp_ata_b,
        &f.keys,
        amount,
        &[],
    );
    send_v1(context, &instructions, &[&donor]).unwrap();
}

pub fn mint_public(context: &mut TestContext, f: &ConfidentialDvpFixture, amount: u64) {
    let ix = token_ix::mint_to(
        &TOKEN,
        &f.accounts.mint_b,
        &f.accounts.dvp_ata_b,
        &context.payer.pubkey(),
        &[],
        amount,
    )
    .unwrap();
    send_v1(context, &[ix], &[]).unwrap();
}

pub struct Refund {
    pub mode: LegBRefund,
    pub contexts: [Option<Pubkey>; 4],
}
impl Refund {
    pub fn none() -> Self {
        Self {
            mode: LegBRefund::None,
            contexts: [None; 4],
        }
    }
}

pub fn prepare_refund(
    context: &mut TestContext,
    f: &ConfidentialDvpFixture,
    keys: &Keys,
    signer: Pubkey,
    balance: u64,
    amount: u64,
    full: bool,
) -> Refund {
    let available: ElGamalCiphertext = state(context, &f.accounts.dvp_ata_b)
        .available_balance
        .try_into()
        .unwrap();
    let proof = transfer_split_proof_data(
        &available,
        &f.keys.balance(balance).try_into().unwrap(),
        amount,
        &f.keys.elgamal,
        &f.keys.ae,
        keys.elgamal.pubkey(),
        mint_auditor(context, &f.accounts.mint_b).as_ref(),
    )
    .unwrap();
    let [equality, validity, range] = transfer_contexts(context, &signer, &proof);
    let ct = &proof.ciphertext_validity_proof_data_with_ciphertext;
    let data = CtTransferData {
        new_source_decryptable_available_balance: f.keys.balance(balance - amount).0,
        auditor_ciphertext_lo: ct.ciphertext_lo.0,
        auditor_ciphertext_hi: ct.ciphertext_hi.0,
    };
    let zero = if full {
        let ctx = ct.proof_data.context_data();
        let lo = ctx
            .grouped_ciphertext_lo
            .try_extract_ciphertext(0)
            .unwrap()
            .try_into()
            .unwrap();
        let hi = ctx
            .grouped_ciphertext_hi
            .try_extract_ciphertext(0)
            .unwrap()
            .try_into()
            .unwrap();
        let remaining = available - try_combine_lo_hi_ciphertexts(&lo, &hi, LO_BITS).unwrap();
        let proof = build_zero_ciphertext_proof_data(&f.keys.elgamal, &remaining).unwrap();
        Some(context_account(
            context,
            &signer,
            ProofInstruction::VerifyZeroCiphertext,
            &proof,
        ))
    } else {
        None
    };
    Refund {
        mode: if full {
            LegBRefund::Full(data)
        } else {
            LegBRefund::Partial(data)
        },
        contexts: [Some(equality), Some(validity), Some(range), zero],
    }
}

pub fn terminal(
    f: &ConfidentialDvpFixture,
    signer: Pubkey,
    refund: &Refund,
    cancel: bool,
    extras: &[AccountMeta],
) -> Instruction {
    let [equality_context, validity_context, range_context, zero_context] = refund.contexts;
    let accounts = CancelConfidentialDvp {
        settlement_authority: signer,
        swap_dvp: f.accounts.swap_dvp,
        mint_a: f.accounts.mint_a,
        mint_b: f.accounts.mint_b,
        dvp_ata_a: f.accounts.dvp_ata_a,
        dvp_ata_b: f.accounts.dvp_ata_b,
        user_a_ata_a: dvp_ata(
            &f.user_a.pubkey(),
            &f.accounts.mint_a,
            &f.accounts.token_program_a,
        ),
        user_b_ata_b: dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN),
        token_program_a: f.accounts.token_program_a,
        token_program_b: TOKEN,
        memo_program: MEMO_PROGRAM_ID,
        zk_elgamal_proof_program: ZK,
        equality_context,
        validity_context,
        range_context,
        zero_context,
    };
    if cancel {
        accounts.instruction_with_remaining_accounts(
            CancelConfidentialDvpInstructionArgs {
                leg_a_extras_count: 0,
                leg_b_refund: refund.mode.clone(),
            },
            extras,
        )
    } else {
        RejectConfidentialDvp {
            signer,
            swap_dvp: accounts.swap_dvp,
            mint_a: accounts.mint_a,
            mint_b: accounts.mint_b,
            dvp_ata_a: accounts.dvp_ata_a,
            dvp_ata_b: accounts.dvp_ata_b,
            user_a_ata_a: accounts.user_a_ata_a,
            user_b_ata_b: accounts.user_b_ata_b,
            token_program_a: accounts.token_program_a,
            token_program_b: accounts.token_program_b,
            memo_program: accounts.memo_program,
            zk_elgamal_proof_program: ZK,
            equality_context,
            validity_context,
            range_context,
            zero_context,
        }
        .instruction_with_remaining_accounts(
            RejectConfidentialDvpInstructionArgs {
                leg_a_extras_count: 0,
                leg_b_refund: refund.mode.clone(),
            },
            extras,
        )
    }
}

pub fn reclaim(
    f: &ConfidentialDvpFixture,
    leg_a: bool,
    refund: &Refund,
    extras: &[AccountMeta],
) -> Instruction {
    let [equality_context, validity_context, range_context, zero_context] = refund.contexts;
    let (signer, mint, escrow, token) = if leg_a {
        (
            f.user_a.pubkey(),
            f.accounts.mint_a,
            f.accounts.dvp_ata_a,
            TOKEN_PROGRAM_ID,
        )
    } else {
        (
            f.user_b.pubkey(),
            f.accounts.mint_b,
            f.accounts.dvp_ata_b,
            TOKEN,
        )
    };
    ReclaimConfidentialDvp {
        signer,
        swap_dvp: f.accounts.swap_dvp,
        mint,
        dvp_source_ata: escrow,
        signer_dest_ata: dvp_ata(&signer, &mint, &token),
        token_program: token,
        memo_program: MEMO_PROGRAM_ID,
        zk_elgamal_proof_program: ZK,
        equality_context,
        validity_context,
        range_context,
        zero_context,
    }
    .instruction_with_remaining_accounts(
        ReclaimConfidentialDvpInstructionArgs {
            leg_b_refund: refund.mode.clone(),
        },
        extras,
    )
}

pub fn recover(f: &ConfidentialDvpFixture, refund: &Refund, extras: &[AccountMeta]) -> Instruction {
    let [equality_context, validity_context, range_context, zero_context] = refund.contexts;
    RecoverConfidentialDvp {
        signer: f.user_b.pubkey(),
        swap_dvp: f.accounts.swap_dvp,
        nonce_tombstone: f.accounts.nonce_tombstone,
        mint: f.accounts.mint_b,
        dvp_escrow_ata: f.accounts.dvp_ata_b,
        signer_dest_ata: dvp_ata(&f.user_b.pubkey(), &f.accounts.mint_b, &TOKEN),
        token_program: TOKEN,
        memo_program: MEMO_PROGRAM_ID,
        zk_elgamal_proof_program: ZK,
        equality_context,
        validity_context,
        range_context,
        zero_context,
    }
    .instruction_with_remaining_accounts(
        RecoverConfidentialDvpInstructionArgs {
            settlement_authority: f.authority.pubkey(),
            user_a: f.user_a.pubkey(),
            user_b: f.user_b.pubkey(),
            mint_a: f.accounts.mint_a,
            mint_b: f.accounts.mint_b,
            nonce: f.args.nonce,
            leg_b_refund: refund.mode.clone(),
        },
        extras,
    )
}

pub fn assert_failure(
    context: &mut TestContext,
    ix: Instruction,
    signer: &Keypair,
    expected: InstructionError,
    before_cpi: bool,
) {
    let before: Vec<_> = ix
        .accounts
        .iter()
        .filter(|a| a.is_writable)
        .map(|a| (a.pubkey, context.get_account(&a.pubkey)))
        .collect();
    let signers = if ix.accounts[0].is_signer {
        vec![signer]
    } else {
        vec![]
    };
    let failure = send_v1(context, &[ix], &signers).unwrap_err();
    assert_eq!(
        format!("{:?}", failure.err),
        format!("InstructionError(0, {expected:?})"),
        "{:?}",
        failure.meta.logs
    );
    if before_cpi {
        assert!(
            !failure.meta.logs.iter().any(|s| s.contains("invoke [2]")),
            "{:?}",
            failure.meta.logs
        );
    }
    for (key, account) in before {
        assert_eq!(context.get_account(&key), account, "{key}");
    }
}

pub fn assert_contexts_closed(context: &TestContext, refund: &Refund) {
    for key in refund.contexts.iter().flatten() {
        assert!(context.get_account(key).is_none());
    }
}

pub fn available(context: &TestContext, f: &ConfidentialDvpFixture) -> u64 {
    let ct: ElGamalCiphertext = state(context, &f.accounts.dvp_ata_b)
        .available_balance
        .try_into()
        .unwrap();
    ct.decrypt_u32(f.keys.elgamal.secret()).unwrap()
}

pub fn mutate_ct(
    context: &mut TestContext,
    address: Pubkey,
    change: impl FnOnce(&mut ConfidentialTransferAccount),
) {
    let mut account = context.get_account(&address).unwrap();
    let mut state = StateWithExtensionsMut::<TokenAccount>::unpack(&mut account.data).unwrap();
    change(
        state
            .get_extension_mut::<ConfidentialTransferAccount>()
            .unwrap(),
    );
    context.svm.set_account(address, account).unwrap();
}

/// Applies two pending credits of 2^47 each, producing a balance of 2^48.
pub fn setup_large_refund(
    context: &mut TestContext,
    closed_swap: bool,
) -> (ConfidentialDvpFixture, Keys, u64) {
    let (f, keys) = setup_refund(context, 0, 0, false);
    let credit = 1u64 << 47;
    fund_b(context, &f, &keys, credit, false);
    fund_b(context, &f, &keys, credit, false);
    // Recover starts with a real closed swap and unapplied pending balance.
    if closed_swap {
        send_v1(
            context,
            &[terminal(
                &f,
                f.authority.pubkey(),
                &Refund::none(),
                true,
                &[],
            )],
            &[&f.authority],
        )
        .unwrap();
    }
    send_v1(
        context,
        &[f.apply(f.user_b.pubkey(), credit * 2)
            .expected_pending_balance_credit_counter(2)
            .instruction()],
        &[&f.user_b],
    )
    .unwrap();
    (f, keys, credit * 2)
}

pub fn mint_auditor(context: &TestContext, mint: &Pubkey) -> Option<ElGamalPubkey> {
    let account = context.get_account(mint).unwrap();
    let mint = StateWithExtensions::<Mint>::unpack(&account.data).unwrap();
    Option::from(
        mint.get_extension::<ConfidentialTransferMint>()
            .unwrap()
            .auditor_elgamal_pubkey,
    )
    .map(|key: solana_zk_sdk_pod::encryption::elgamal::PodElGamalPubkey| key.try_into().unwrap())
}

pub fn set_mint_auditor(context: &mut TestContext, mint: &Pubkey, auditor: &ElGamalPubkey) {
    let account = context.get_account(mint).unwrap();
    let mint_state = StateWithExtensions::<Mint>::unpack(&account.data).unwrap();
    let auto_approve = mint_state
        .get_extension::<ConfidentialTransferMint>()
        .unwrap()
        .auto_approve_new_accounts
        .into();
    let ix = ct_ix::update_mint(
        &TOKEN,
        mint,
        &context.payer.pubkey(),
        &[],
        auto_approve,
        Some((*auditor).into()),
    )
    .unwrap();
    send_v1(context, &[ix], &[]).unwrap();
}
