use dvp_swap_program_client::confidential::test_utils::RECORD_PROGRAM_ID;
#[path = "../../../tests/integration-tests/src/utils.rs"]
mod framework;
pub use framework::*;

use base64::{engine::general_purpose::STANDARD, Engine};
use framework::TOKEN_2022_PROGRAM_ID as TOKEN;
use std::borrow::Cow;

use dvp_swap_program_client::{
    confidential::*,
    instructions::*,
    verify::{
        decode_swap_dvp_account, ConfidentialSwapDvp, ExpectedConfidentialTerms, SwapDvpAccount,
    },
};
use litesvm::types::TransactionMetadata;
use solana_account::Account;
use solana_address_lookup_table_interface::state::{AddressLookupTable, LookupTableMeta};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{AddressLookupTableAccount, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token_2022_interface::extension::confidential_transfer::ConfidentialTransferAccount;

use crate::{
    confidential_utils::{
        create_wallet_account, fund_b, state, ConfidentialDvpFixture, Keys, MAX_PENDING,
    },
    state_utils::{AMOUNT_A, AMOUNT_B},
};

/// Real on-chain fixtures with client-derived escrow keys. Public helpers and
/// wallet funding are shared with the processor integration tests.
pub struct ClientFixture {
    pub dvp: ConfidentialDvpFixture,
    pub keys: EscrowKeys,
    pub buyer_keys: Keys,
    pub recipient_keys: Keys,
    pub recipient: Pubkey,
    pub refund: Pubkey,
    pub asset_recipient: Pubkey,
    pub asset_refund: Pubkey,
    pub config: SessionConfig,
}

impl ClientFixture {
    pub fn new(context: &mut TestContext, format: TransactionFormat, funded: u64) -> Self {
        Self::with_hook(context, format, funded, false)
    }

    pub fn with_hook(
        context: &mut TestContext,
        format: TransactionFormat,
        funded: u64,
        hook: bool,
    ) -> Self {
        let f = ConfidentialDvpFixture::new(context, true, hook);
        Self::with_fixture(context, format, funded, f, AMOUNT_B, hook, MAX_PENDING)
    }

    pub fn with_fixture(
        context: &mut TestContext,
        format: TransactionFormat,
        funded: u64,
        mut f: ConfidentialDvpFixture,
        amount_b: u64,
        hook: bool,
        recipient_capacity: u64,
    ) -> Self {
        let keys = EscrowKeys::from_seed(&[0x42; 32]).unwrap();
        f.keys = Keys {
            elgamal: keys.elgamal.clone(),
            ae: keys.ae.clone(),
        };
        f.amount_b_openings = [keys.opening_lo.clone(), keys.opening_hi.clone()];
        let mut config =
            SessionConfig::new(context.payer.pubkey(), format, context.svm.get_sysvar());
        // Exercise fee-aware proof packing and execution in both transaction formats.
        config.compute_unit_price = Some(1);
        // The table is active before construction. Proof addresses are generated
        // later and stay static; all stable DvP accounts are available through LUT.
        if format == TransactionFormat::V0 {
            context.advance_clock(1);
            install_lut(
                context,
                &mut config,
                f.accounts
                    .instruction(f.args.clone())
                    .accounts
                    .iter()
                    .map(|a| a.pubkey)
                    .collect(),
            );
        }
        let create = create_session(&config, &f.accounts, f.args.clone(), &keys, amount_b).unwrap();
        execute(context, &config, create, &[]);
        let asset_refund = fund_wallet_ata(
            context,
            &f.user_a,
            &f.accounts.mint_a,
            AMOUNT_A,
            &TOKEN_PROGRAM_ID,
        );
        context
            .send(
                spl_token_interface::instruction::transfer_checked(
                    &TOKEN_PROGRAM_ID,
                    &asset_refund,
                    &f.accounts.mint_a,
                    &f.accounts.dvp_ata_a,
                    &f.user_a.pubkey(),
                    &[],
                    AMOUNT_A,
                    6,
                )
                .unwrap(),
                &[&f.user_a],
            )
            .unwrap();
        let buyer_keys = Keys::new();
        let refund = wallet_with_capacity(
            context,
            &f.user_b,
            &f.accounts.mint_b,
            &buyer_keys,
            recipient_capacity,
        );
        let recipient_keys = Keys::new();
        let recipient =
            create_wallet_account(context, &f.user_a, &f.accounts.mint_b, &recipient_keys);
        let asset_recipient = create_ata(
            context,
            &f.user_b.pubkey(),
            &f.accounts.mint_a,
            &TOKEN_PROGRAM_ID,
        );
        if hook {
            crate::utils::set_hook_extra_account_metas(
                context,
                &f.accounts.mint_b,
                &[AccountMeta::new_readonly(
                    solana_sdk_ids::system_program::ID,
                    false,
                )],
            );
        }
        if format == TransactionFormat::V0 {
            install_lut(
                context,
                &mut config,
                vec![
                    refund,
                    recipient,
                    asset_recipient,
                    asset_refund,
                    MEMO_PROGRAM_ID,
                    solana_zk_elgamal_proof_interface::ID,
                ],
            );
        }
        let fixture = Self {
            dvp: f,
            keys,
            buyer_keys,
            recipient_keys,
            recipient,
            refund,
            asset_recipient,
            asset_refund,
            config,
        };
        if funded > 0 {
            fund_b(context, &fixture.dvp, &fixture.buyer_keys, funded, hook);
            let state = fixture.source(context);
            let plan = apply_session(
                &fixture.config,
                &fixture.apply_accounts(fixture.dvp.user_b.pubkey()),
                fixture.apply_args(),
                TransferSource {
                    state: &state,
                    keys: &fixture.keys,
                    history: &[],
                },
            )
            .unwrap();
            execute(context, &fixture.config, plan, &[&fixture.dvp.user_b]);
        }
        fixture
    }

    pub fn source(&self, context: &TestContext) -> ConfidentialTransferAccount {
        state(context, &self.dvp.accounts.dvp_ata_b)
    }
    pub fn swap(&self, context: &TestContext) -> ConfidentialSwapDvp {
        let account = context.get_account(&self.dvp.accounts.swap_dvp).unwrap();
        match decode_swap_dvp_account(&self.dvp.accounts.swap_dvp, &account).unwrap() {
            SwapDvpAccount::Confidential(swap) => swap,
            _ => panic!("expected confidential swap"),
        }
    }
    pub fn apply_accounts(&self, signer: Pubkey) -> ApplyConfidentialDvp {
        ApplyConfidentialDvp {
            signer,
            swap_dvp: self.dvp.accounts.swap_dvp,
            nonce_tombstone: self.dvp.accounts.nonce_tombstone,
            dvp_ata_b: self.dvp.accounts.dvp_ata_b,
            token_program: TOKEN,
        }
    }
    pub fn apply_args(&self) -> ApplyConfidentialDvpInstructionArgs {
        ApplyConfidentialDvpInstructionArgs {
            settlement_authority: self.dvp.authority.pubkey(),
            user_a: self.dvp.user_a.pubkey(),
            user_b: self.dvp.user_b.pubkey(),
            mint_a: self.dvp.accounts.mint_a,
            mint_b: self.dvp.accounts.mint_b,
            nonce: self.dvp.args.nonce,
            expected_pending_balance_credit_counter: 0,
            new_decryptable_available_balance: [0; 36],
        }
    }
    /// The agreed public terms, taken from the Create arguments.
    pub fn expected_terms(&self, amount_b: u64) -> ExpectedConfidentialTerms {
        let (accounts, args) = (&self.dvp.accounts, &self.dvp.args);
        ExpectedConfidentialTerms {
            settlement_authority: accounts.settlement_authority,
            user_a: accounts.user_a,
            user_b: accounts.user_b,
            mint_a: accounts.mint_a,
            mint_b: accounts.mint_b,
            token_program_a: accounts.token_program_a,
            amount_a: args.amount_a,
            amount_b,
            nonce: args.nonce,
            expiry_timestamp: args.expiry_timestamp,
            earliest_settlement_timestamp: args.earliest_settlement_timestamp,
            user_a_settlement_destination: args
                .user_a_settlement_destination
                .unwrap_or(accounts.user_a),
            user_b_settlement_destination: args
                .user_b_settlement_destination
                .unwrap_or(accounts.user_b),
        }
    }

    pub fn settle_accounts(&self) -> SettleConfidentialDvp {
        let placeholder = Pubkey::default();
        SettleConfidentialDvp {
            settlement_authority: self.dvp.authority.pubkey(),
            swap_dvp: self.dvp.accounts.swap_dvp,
            mint_a: self.dvp.accounts.mint_a,
            mint_b: self.dvp.accounts.mint_b,
            dvp_ata_a: self.dvp.accounts.dvp_ata_a,
            dvp_ata_b: self.dvp.accounts.dvp_ata_b,
            user_a_destination_ata_b: self.recipient,
            user_b_destination_ata_a: self.asset_recipient,
            user_a_ata_a: self.asset_refund,
            user_b_ata_b: self.refund,
            token_program_a: TOKEN_PROGRAM_ID,
            token_program_b: TOKEN,
            memo_program: MEMO_PROGRAM_ID,
            zk_elgamal_proof_program: solana_zk_elgamal_proof_interface::ID,
            payment_equality_context: placeholder,
            payment_validity_context: placeholder,
            payment_range_context: placeholder,
            eq_lo_context: placeholder,
            eq_hi_context: placeholder,
            zero_context: placeholder,
            surplus_equality_context: None,
            surplus_validity_context: None,
            surplus_range_context: None,
        }
    }
}

pub fn install_lut(context: &mut TestContext, config: &mut SessionConfig, addresses: Vec<Pubkey>) {
    let key = Pubkey::new_unique();
    let data = AddressLookupTable {
        meta: LookupTableMeta::default(),
        addresses: Cow::Borrowed(&addresses),
    }
    .serialize_for_tests()
    .unwrap();
    context
        .svm
        .set_account(
            key,
            Account {
                lamports: context.svm.minimum_balance_for_rent_exemption(data.len()),
                data,
                owner: solana_address_lookup_table_interface::program::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    config
        .lookup_tables
        .push(AddressLookupTableAccount { key, addresses });
}

/// Execute actual signed plans, retaining the proof transactions and Token CPIs
/// for history recovery. Cleanup accounts must disappear on successful final use.
pub fn execute(
    context: &mut TestContext,
    config: &SessionConfig,
    session: TransactionSession,
    signers: &[&Keypair],
) -> Vec<ExecutedTransaction> {
    let mut history = vec![];
    let mut all: Vec<&dyn Signer> = vec![&context.payer];
    all.extend(signers.iter().map(|s| *s as &dyn Signer));
    let plans = session
        .preparation
        .iter()
        .chain(std::iter::once(&session.final_transaction));
    for plan in plans {
        let transaction = plan
            .sign(config, context.svm.latest_blockhash(), &all)
            .unwrap();
        let size = wincode::serialize(&transaction).unwrap().len();
        assert!(size <= config.transaction_limit());
        let message = transaction.message.clone();
        let meta = context
            .svm
            .send_transaction(transaction)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(meta.compute_units_consumed <= config.compute_unit_limit as u64);
        history.push(trace(&message, config, &meta));
    }
    for ix in &session.cleanup {
        assert!(
            context.get_account(&ix.accounts[0].pubkey).is_none(),
            "temporary account was not closed"
        );
    }
    history
}

pub fn trace(
    message: &VersionedMessage,
    config: &SessionConfig,
    meta: &TransactionMetadata,
) -> ExecutedTransaction {
    let mut keys = message.static_account_keys().to_vec();
    if let Some(lookups) = message.address_table_lookups() {
        for writable in [true, false] {
            for lookup in lookups {
                let table = config
                    .lookup_tables
                    .iter()
                    .find(|t| t.key == lookup.account_key)
                    .unwrap();
                for index in if writable {
                    &lookup.writable_indexes
                } else {
                    &lookup.readonly_indexes
                } {
                    keys.push(table.addresses[*index as usize]);
                }
            }
        }
    }
    let decode = |ix: &solana_message::compiled_instruction::CompiledInstruction| Instruction {
        program_id: keys[ix.program_id_index as usize],
        data: ix.data.clone(),
        accounts: ix
            .accounts
            .iter()
            .map(|i| AccountMeta::new_readonly(keys[*i as usize], false))
            .collect(),
    };
    use solana_transaction_status_client_types::{
        EncodedTransaction, EncodedTransactionWithStatusMeta, TransactionBinaryEncoding,
        UiCompiledInstruction,
    };
    let transaction = solana_transaction::versioned::VersionedTransaction {
        signatures: vec![Default::default(); message.header().num_required_signatures as usize],
        message: message.clone(),
    };
    let static_len = message.static_account_keys().len();
    let writable_len: usize = message
        .address_table_lookups()
        .unwrap_or_default()
        .iter()
        .map(|v| v.writable_indexes.len())
        .sum();
    let value = EncodedTransactionWithStatusMeta {
        transaction: EncodedTransaction::Binary(STANDARD.encode(wincode::serialize(&transaction).unwrap()), TransactionBinaryEncoding::Base64),
        meta: Some(serde_json::from_value(serde_json::json!({
            "err": null, "status": { "Ok": null }, "fee": meta.fee, "preBalances": [], "postBalances": [],
            "innerInstructions": meta.inner_instructions.iter().enumerate().map(|(index, inner)| serde_json::json!({
                "index": index, "instructions": inner.iter().map(|ix| UiCompiledInstruction::from(&ix.instruction, Some(ix.stack_height.into()))).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "loadedAddresses": { "writable": keys[static_len..static_len + writable_len].iter().map(ToString::to_string).collect::<Vec<_>>(),
                "readonly": keys[static_len + writable_len..].iter().map(ToString::to_string).collect::<Vec<_>>() }
        })).unwrap()), version: None,
    };
    let decoded = ExecutedTransaction::from_rpc(&value).unwrap();
    assert_eq!(
        decoded.instructions,
        message
            .instructions()
            .iter()
            .map(decode)
            .collect::<Vec<_>>()
    );
    for (index, inner) in meta.inner_instructions.iter().enumerate() {
        assert_eq!(
            decoded.inner_instructions[&index],
            inner
                .iter()
                .map(|ix| decode(&ix.instruction))
                .collect::<Vec<_>>()
        );
    }
    decoded
}

fn wallet_with_capacity(
    context: &mut TestContext,
    wallet: &Keypair,
    mint: &Pubkey,
    keys: &Keys,
    maximum: u64,
) -> Pubkey {
    use solana_zk_sdk::zk_elgamal_proof_program::build_pubkey_validity_proof_data;
    use spl_token_2022_interface::{
        extension::{confidential_transfer::instruction as ct, ExtensionType},
        instruction as token,
    };
    use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;
    let address = create_ata(context, &wallet.pubkey(), mint, &TOKEN);
    let proof = build_pubkey_validity_proof_data(&keys.elgamal).unwrap();
    let mut instructions = vec![token::reallocate(
        &TOKEN,
        &address,
        &context.payer.pubkey(),
        &wallet.pubkey(),
        &[],
        &[ExtensionType::ConfidentialTransferAccount],
    )
    .unwrap()];
    instructions.extend(
        ct::configure_account(
            &TOKEN,
            &address,
            mint,
            &keys.balance(0),
            maximum,
            &wallet.pubkey(),
            &[],
            ProofLocation::InstructionOffset(std::num::NonZeroI8::new(1).unwrap(), &proof),
        )
        .unwrap(),
    );
    crate::confidential_utils::send_v1(context, &instructions, &[wallet]).unwrap();
    address
}

pub fn send_plan(
    context: &mut TestContext,
    config: &SessionConfig,
    plan: &PlannedTransaction,
    authority: &Keypair,
) -> Result<TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>> {
    context.svm.expire_blockhash();
    let tx = plan
        .sign(
            config,
            context.svm.latest_blockhash(),
            &[&context.payer, authority],
        )
        .unwrap();
    context.svm.send_transaction(tx).map_err(Box::new)
}

/// Run only cleanup instructions whose accounts were created before interruption.
/// Record rent goes to the payer; proof-context rent goes to operation authority.
pub fn cleanup(
    context: &mut TestContext,
    config: &SessionConfig,
    session: &TransactionSession,
    authority: &Keypair,
) {
    let payer_before = context.get_account(&config.payer).unwrap().lamports;
    let authority_before = context
        .get_account(&authority.pubkey())
        .map_or(0, |a| a.lamports);
    let (mut payer_rent, mut authority_rent, mut fees) = (0, 0, 0);
    for ix in &session.cleanup {
        let Some(account) = context.get_account(&ix.accounts[0].pubkey) else {
            continue;
        };
        if ix.program_id == RECORD_PROGRAM_ID {
            payer_rent += account.lamports;
        } else {
            authority_rent += account.lamports;
        }
        let meta = send_plan(
            context,
            config,
            &PlannedTransaction {
                instructions: vec![ix.clone()],
                signers: vec![],
            },
            authority,
        )
        .unwrap();
        fees += meta.fee;
    }
    for ix in &session.cleanup {
        assert!(context.get_account(&ix.accounts[0].pubkey).is_none());
    }
    if config.payer == authority.pubkey() {
        assert_eq!(
            context.get_account(&config.payer).unwrap().lamports + fees,
            payer_before + payer_rent + authority_rent
        );
    } else {
        assert_eq!(
            context.get_account(&config.payer).unwrap().lamports + fees,
            payer_before + payer_rent
        );
        assert_eq!(
            context
                .get_account(&authority.pubkey())
                .map_or(0, |a| a.lamports),
            authority_before + authority_rent
        );
    }
}

/// A fetched token account as a session recipient bound to `address`.
pub fn recipient_account(address: Pubkey, account: &Account) -> RecipientAccount<'_> {
    RecipientAccount {
        address,
        owner: account.owner,
        data: &account.data,
    }
}

/// A Token-2022 account for `mint` holding only the given CT extension, for
/// session tests that need a recipient snapshot without ledger setup.
pub fn ct_token_account(mint: Pubkey, ct: ConfidentialTransferAccount) -> Account {
    use spl_token_2022_interface::{
        extension::{BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut},
        state::{Account as TokenAccount, AccountState},
    };
    let len = ExtensionType::try_calculate_account_len::<TokenAccount>(&[
        ExtensionType::ConfidentialTransferAccount,
    ])
    .unwrap();
    let mut data = vec![0; len];
    let mut state =
        StateWithExtensionsMut::<TokenAccount>::unpack_uninitialized(&mut data).unwrap();
    state.base = TokenAccount {
        mint,
        owner: Pubkey::new_unique(),
        state: AccountState::Initialized,
        ..Default::default()
    };
    state.pack_base();
    state.init_account_type().unwrap();
    *state
        .init_extension::<ConfidentialTransferAccount>(true)
        .unwrap() = ct;
    Account {
        lamports: 1,
        data,
        owner: TOKEN,
        executable: false,
        rent_epoch: 0,
    }
}

/// A fetched mint account as a session mint snapshot bound to `address`.
pub fn mint_account(address: Pubkey, account: &Account) -> MintAccount<'_> {
    MintAccount {
        address,
        owner: account.owner,
        data: &account.data,
    }
}

/// A Token-2022 mint with a confidential transfer extension and no auditor,
/// for session tests without ledger setup.
pub fn ct_mint_account() -> Account {
    use spl_token_2022_interface::{
        extension::{
            confidential_transfer::ConfidentialTransferMint, BaseStateWithExtensionsMut,
            ExtensionType, StateWithExtensionsMut,
        },
        state::Mint,
    };
    let len = ExtensionType::try_calculate_account_len::<Mint>(&[
        ExtensionType::ConfidentialTransferMint,
    ])
    .unwrap();
    let mut data = vec![0; len];
    let mut state = StateWithExtensionsMut::<Mint>::unpack_uninitialized(&mut data).unwrap();
    state.base = Mint {
        decimals: 6,
        is_initialized: true,
        ..Default::default()
    };
    state.pack_base();
    state.init_account_type().unwrap();
    *state
        .init_extension::<ConfidentialTransferMint>(true)
        .unwrap() = ConfidentialTransferMint::default();
    Account {
        lamports: 1,
        data,
        owner: TOKEN,
        executable: false,
        rent_epoch: 0,
    }
}
