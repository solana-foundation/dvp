use dvp_swap_program_client::{DvpSwapProgramError, DVP_SWAP_PROGRAM_ID};
use litesvm::{types::TransactionMetadata, LiteSVM};
use solana_account::Account;
use solana_clock::Clock;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_nullable::MaybeNull;
use solana_packet::PACKET_DATA_SIZE;
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_pubkey::pubkey;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use spl_associated_token_account_interface::{
    address::get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};
use spl_token_2022_interface::{
    extension::{
        confidential_transfer::ConfidentialTransferMint,
        interest_bearing_mint::InterestBearingConfig,
        non_transferable::NonTransferable,
        pausable::PausableConfig,
        permanent_delegate::PermanentDelegate,
        scaled_ui_amount::ScaledUiAmountConfig,
        transfer_fee::TransferFeeConfig,
        transfer_hook::{TransferHook, TransferHookAccount},
        BaseStateWithExtensionsMut, ExtensionType, PodStateWithExtensionsMut,
    },
    pod::{PodAccount, PodMint},
    state::{Account as Token2022Account, AccountState, Mint as Token2022Mint},
};
use spl_token_interface::state::{Account as TokenAccount, Mint};

pub use spl_token_2022_interface::ID as TOKEN_2022_PROGRAM_ID;
pub use spl_token_interface::ID as TOKEN_PROGRAM_ID;

pub const ATA_PROGRAM_ID: Pubkey = pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const MEMO_PROGRAM_ID: Pubkey = pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");
pub const SWAP_PROGRAM_ID: Pubkey = DVP_SWAP_PROGRAM_ID;

/// Program ID of the no-op transfer-hook fixture loaded into LiteSVM
/// for hook-bearing Token-2022 lifecycle tests. Matches
/// `declare_id!` in `dvp-swap-program/tests/transfer-hook-fixture/src/lib.rs`.
pub const HOOK_FIXTURE_PROGRAM_ID: Pubkey = pubkey!("HookqJupt6Khm8s8jB3p93NkhPoiAg2M7vkEhkS15CtC");

/// Program ID of the smart-wallet fixture loaded into LiteSVM for
/// Squads-style (vault PDA signs via CPI) party tests. Matches
/// `declare_id!` in `dvp-swap-program/tests/smart-wallet-fixture/src/lib.rs`.
pub const SMART_WALLET_FIXTURE_PROGRAM_ID: Pubkey =
    pubkey!("H5tY4bRL6jk62vkxeytUVVjYDjBWgV6FQMvgnmaJNuNz");

pub const SWAP_DVP_SEED: &[u8] = b"dvp";
pub const NONCE_TOMBSTONE_SEED: &[u8] = b"nonce";
pub const SMART_WALLET_VAULT_SEED: &[u8] = b"vault";

pub const SIGNER_NOT_PARTY: u32 = DvpSwapProgramError::SignerNotParty as u32;
pub const DVP_EXPIRED: u32 = DvpSwapProgramError::DvpExpired as u32;
pub const SETTLEMENT_AUTHORITY_MISMATCH: u32 =
    DvpSwapProgramError::SettlementAuthorityMismatch as u32;
pub const SETTLEMENT_TOO_EARLY: u32 = DvpSwapProgramError::SettlementTooEarly as u32;
pub const LEG_NOT_FUNDED: u32 = DvpSwapProgramError::LegNotFunded as u32;
pub const EXPIRY_NOT_IN_FUTURE: u32 = DvpSwapProgramError::ExpiryNotInFuture as u32;
pub const EXPIRY_TOO_FAR_IN_FUTURE: u32 = DvpSwapProgramError::ExpiryTooFarInFuture as u32;
pub const EARLIEST_AFTER_EXPIRY: u32 = DvpSwapProgramError::EarliestAfterExpiry as u32;
pub const SELF_DVP: u32 = DvpSwapProgramError::SelfDvp as u32;
pub const SAME_MINT: u32 = DvpSwapProgramError::SameMint as u32;
pub const ZERO_AMOUNT: u32 = DvpSwapProgramError::ZeroAmount as u32;
pub const BLOCKED_MINT_EXTENSION: u32 = DvpSwapProgramError::BlockedMintExtension as u32;
pub const SETTLEMENT_AUTHORITY_EXECUTABLE: u32 =
    DvpSwapProgramError::SettlementAuthorityExecutable as u32;
pub const SETTLEMENT_AUTHORITY_IS_PARTY: u32 =
    DvpSwapProgramError::SettlementAuthorityIsParty as u32;
pub const NONCE_ALREADY_USED: u32 = DvpSwapProgramError::NonceAlreadyUsed as u32;
pub const REF_STRING_TOO_LONG: u32 = DvpSwapProgramError::RefStringTooLong as u32;
pub const DVP_STILL_OPEN: u32 = DvpSwapProgramError::DvpStillOpen as u32;
pub const DVP_NEVER_CREATED: u32 = DvpSwapProgramError::DvpNeverCreated as u32;
pub const ESCROW_PRELOADED_WITH_LAMPORTS: u32 =
    DvpSwapProgramError::EscrowPreloadedWithLamports as u32;
pub const SETTLEMENT_DESTINATION_IS_SWAP_DVP: u32 =
    DvpSwapProgramError::SettlementDestinationIsSwapDvp as u32;
pub const SWAP_DVP_PRELOADED_WITH_LAMPORTS: u32 =
    DvpSwapProgramError::SwapDvpPreloadedWithLamports as u32;
pub const RECIPIENT_ATA_MISMATCH: u32 = DvpSwapProgramError::RecipientAtaMismatch as u32;
pub const PARTY_NOT_SIGNER_CAPABLE: u32 = DvpSwapProgramError::PartyNotSignerCapable as u32;
pub const MINT_AUTHORITY_CHANGED: u32 = DvpSwapProgramError::MintAuthorityChanged as u32;

const MIN_LAMPORTS: u64 = 500_000_000;

/// Pin external programs even when tests are run directly through cargo.
fn load_external_programs(svm: &mut LiteSVM) {
    use sha2::{Digest, Sha256};
    use std::sync::OnceLock;

    const TOKEN: &[u8] = include_bytes!("../../fixtures/spl_token_2022.so");
    const RECORD: &[u8] = include_bytes!("../../fixtures/spl_record.so");
    static VERIFIED: OnceLock<()> = OnceLock::new();
    VERIFIED.get_or_init(|| {
        let checksums = include_str!("../../fixtures/SHA256SUMS");
        for (name, bytes) in [("spl_token_2022.so", TOKEN), ("spl_record.so", RECORD)] {
            let expected = checksums
                .lines()
                .find_map(|line| {
                    let (hash, file) = line.split_once("  ")?;
                    (file == name).then_some(hash)
                })
                .expect("fixture must have a pinned hash");
            assert_eq!(format!("{:x}", Sha256::digest(bytes)), expected, "{name}");
        }
    });
    svm.add_program(TOKEN_2022_PROGRAM_ID, TOKEN).unwrap();
    svm.add_program(
        pubkey!("recr1L3PCGKLbckBqMNcJhuuyU1zgo8nBhfLVsJNwr5"),
        RECORD,
    )
    .unwrap();
}

pub struct TestContext {
    pub svm: LiteSVM,
    pub payer: Keypair,
}

impl TestContext {
    pub fn new() -> Self {
        let mut svm = LiteSVM::new();
        load_external_programs(&mut svm);

        // CreateDvp rejects expiry <= now; default LiteSVM Clock has
        // unix_timestamp = 0, which would let any future expiry of 1+
        // pass. Pin Clock to wall-clock so the tests' "expiry = now + 1h"
        // is meaningful.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        svm.set_sysvar(&Clock {
            slot: 1,
            epoch_start_timestamp: now,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: now,
        });

        let program_data = include_bytes!("../../../target/deploy/dvp_swap_program.so");
        svm.add_program(SWAP_PROGRAM_ID, program_data).unwrap();

        let hook_data = include_bytes!("../../../target/deploy/transfer_hook_fixture.so");
        svm.add_program(HOOK_FIXTURE_PROGRAM_ID, hook_data).unwrap();

        let smart_wallet_data = include_bytes!("../../../target/deploy/smart_wallet_fixture.so");
        svm.add_program(SMART_WALLET_FIXTURE_PROGRAM_ID, smart_wallet_data)
            .unwrap();

        let payer = Keypair::new();
        svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();

        Self { svm, payer }
    }

    pub fn now(&self) -> i64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp
    }

    pub fn advance_clock(&mut self, seconds: i64) {
        let clock = self.svm.get_sysvar::<Clock>();
        self.svm.set_sysvar(&Clock {
            slot: clock.slot + seconds as u64,
            unix_timestamp: clock.unix_timestamp + seconds,
            ..clock
        });
    }

    pub fn airdrop_if_required(&mut self, pubkey: &Pubkey, lamports: u64) {
        let needs = match self.svm.get_account(pubkey) {
            Some(account) => account.lamports < MIN_LAMPORTS,
            None => true,
        };
        if needs {
            self.svm.airdrop(pubkey, lamports).expect("airdrop");
        }
    }

    pub fn send(
        &mut self,
        ix: Instruction,
        signers: &[&Keypair],
    ) -> Result<TransactionMetadata, String> {
        // Advance the blockhash before signing so back-to-back identical
        // instructions (e.g. fund → reclaim → fund) don't collide on
        // signature and trip litesvm's AlreadyProcessed guard.
        self.svm.expire_blockhash();
        let mut all_signers = vec![&self.payer];
        all_signers.extend(signers);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &all_signers,
            self.svm.latest_blockhash(),
        );
        let size = wincode::serialize(&tx).map_err(|e| e.to_string())?.len();
        if size > PACKET_DATA_SIZE {
            return Err(format!(
                "transaction size {size} exceeds limit {PACKET_DATA_SIZE}"
            ));
        }
        self.svm
            .send_transaction(tx)
            .map_err(|e| format!("{:?}", e))
    }

    pub fn get_account(&self, pubkey: &Pubkey) -> Option<Account> {
        self.svm.get_account(pubkey)
    }
}

impl Default for TestContext {
    fn default() -> Self {
        Self::new()
    }
}

/// The wrapped-SOL (native) mint, owned by legacy SPL Token. LiteSVM does
/// not seed this account, so tests must write it with `set_native_mint`.
pub const NATIVE_MINT: Pubkey = spl_token_interface::native_mint::ID;

/// Write the wrapped-SOL mint (decimals 9, SPL Token) at its canonical
/// address so WSOL legs can be exercised.
pub fn set_native_mint(context: &mut TestContext) {
    let mint_state = Mint {
        decimals: 9,
        is_initialized: true,
        freeze_authority: COption::None,
        mint_authority: COption::None,
        supply: 0,
    };
    let mut data = vec![0u8; Mint::LEN];
    Mint::pack(mint_state, &mut data).unwrap();
    write_account(context, &NATIVE_MINT, data, TOKEN_PROGRAM_ID, 1_000_000_000);
}

/// Write a bare mint (no Token-2022 extensions) owned by `token_program`.
/// For Token-2022 mints with extensions use one of the
/// `set_mint_2022_with_*` builders below — they pack a proper TLV layout
/// the program will see exactly as a real on-chain mint.
pub fn set_mint(context: &mut TestContext, mint: &Pubkey, token_program: &Pubkey) {
    if *token_program == TOKEN_PROGRAM_ID {
        let mint_state = Mint {
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
            mint_authority: COption::None,
            supply: 1_000_000_000_000,
        };
        let mut data = vec![0u8; Mint::LEN];
        Mint::pack(mint_state, &mut data).unwrap();
        write_account(context, mint, data, *token_program, 1_000_000_000);
    } else if *token_program == TOKEN_2022_PROGRAM_ID {
        let mint_state = Token2022Mint {
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
            mint_authority: COption::None,
            supply: 1_000_000_000_000,
        };
        let mut data = vec![0u8; Token2022Mint::LEN];
        Token2022Mint::pack_into_slice(&mint_state, &mut data);
        write_account(context, mint, data, *token_program, 1_000_000_000);
    } else {
        panic!("unknown token program: {token_program}");
    }
}

/// Like `set_mint`, but writes a chosen mint authority instead of `None`.
pub fn set_mint_with_authority(
    context: &mut TestContext,
    mint: &Pubkey,
    token_program: &Pubkey,
    mint_authority: Option<Pubkey>,
) {
    let authority = mint_authority.map_or(COption::None, COption::Some);
    if *token_program == TOKEN_PROGRAM_ID {
        let mint_state = Mint {
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
            mint_authority: authority,
            supply: 1_000_000_000_000,
        };
        let mut data = vec![0u8; Mint::LEN];
        Mint::pack(mint_state, &mut data).unwrap();
        write_account(context, mint, data, *token_program, 1_000_000_000);
    } else if *token_program == TOKEN_2022_PROGRAM_ID {
        let mint_state = Token2022Mint {
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
            mint_authority: authority,
            supply: 1_000_000_000_000,
        };
        let mut data = vec![0u8; Token2022Mint::LEN];
        Token2022Mint::pack_into_slice(&mint_state, &mut data);
        write_account(context, mint, data, *token_program, 1_000_000_000);
    } else {
        panic!("unknown token program: {token_program}");
    }
}

/// Write a token-account into the SVM. Dispatches the pack format on
/// `token_program`. For Token-2022 we pack the base 165 bytes — no
/// account-side extensions (ImmutableOwner / CpiGuard / etc.) — since
/// the program's transfer/close CPIs don't depend on them.
pub fn set_token_balance(
    context: &mut TestContext,
    ata: &Pubkey,
    mint: &Pubkey,
    owner: &Pubkey,
    amount: u64,
    token_program: &Pubkey,
) {
    let data = if *token_program == TOKEN_PROGRAM_ID {
        let acct = TokenAccount {
            mint: *mint,
            owner: *owner,
            amount,
            delegate: COption::None,
            state: spl_token_interface::state::AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };
        let mut buf = vec![0u8; TokenAccount::LEN];
        TokenAccount::pack(acct, &mut buf).unwrap();
        buf
    } else if *token_program == TOKEN_2022_PROGRAM_ID {
        let acct = Token2022Account {
            mint: *mint,
            owner: *owner,
            amount,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };
        let mut buf = vec![0u8; Token2022Account::LEN];
        spl_token_2022_interface::state::Account::pack(acct, &mut buf).unwrap();
        buf
    } else {
        panic!("unknown token program: {token_program}");
    };
    write_account(context, ata, data, *token_program, 2_039_280);
}

/// Write an initialized SPL Token multisig account at `address`. A token
/// multisig is a data account owned by the token program, not a wallet:
/// it can authorize token operations but can never itself sign a
/// transaction, so it is not a valid DvP party.
pub fn set_token_multisig(context: &mut TestContext, address: &Pubkey, token_program: &Pubkey) {
    use spl_token_interface::state::Multisig;

    let mut multisig = Multisig {
        m: 1,
        n: 1,
        is_initialized: true,
        signers: [Pubkey::default(); 11],
    };
    multisig.signers[0] = Keypair::new().pubkey();
    let mut data = vec![0u8; Multisig::LEN];
    Multisig::pack(multisig, &mut data).unwrap();
    write_account(context, address, data, *token_program, 1_000_000);
}

pub fn fund_wallet_ata(
    context: &mut TestContext,
    wallet: &Keypair,
    mint: &Pubkey,
    amount: u64,
    token_program: &Pubkey,
) -> Pubkey {
    context.airdrop_if_required(&wallet.pubkey(), 1_000_000_000);
    let ata = get_associated_token_address_with_program_id(&wallet.pubkey(), mint, token_program);
    let create_ata_ix = create_associated_token_account_idempotent(
        &context.payer.pubkey(),
        &wallet.pubkey(),
        mint,
        token_program,
    );
    context.send(create_ata_ix, &[]).expect("create user ATA");
    set_token_balance(context, &ata, mint, &wallet.pubkey(), amount, token_program);
    ata
}

/// Create the ATA for `wallet` + `mint` without setting a balance. Used
/// for ATAs that the program will write into (settle recipient ATAs).
pub fn create_ata(
    context: &mut TestContext,
    wallet: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Pubkey {
    let create_ix = create_associated_token_account_idempotent(
        &context.payer.pubkey(),
        wallet,
        mint,
        token_program,
    );
    context.send(create_ix, &[]).expect("create ATA");
    get_associated_token_address_with_program_id(wallet, mint, token_program)
}

/// Read the token account `amount`. Works for both SPL Token and
/// Token-2022 since both layouts share the first 165 bytes.
pub fn get_token_balance(context: &TestContext, ata: &Pubkey) -> u64 {
    match context.get_account(ata) {
        Some(account)
            if account.owner == TOKEN_PROGRAM_ID || account.owner == TOKEN_2022_PROGRAM_ID =>
        {
            TokenAccount::unpack(&account.data[..TokenAccount::LEN])
                .unwrap()
                .amount
        }
        _ => 0,
    }
}

pub fn swap_dvp_pda(
    settlement_authority: &Pubkey,
    user_a: &Pubkey,
    user_b: &Pubkey,
    mint_a: &Pubkey,
    mint_b: &Pubkey,
    nonce: u64,
) -> (Pubkey, u8) {
    let nonce_bytes = nonce.to_le_bytes();
    Pubkey::find_program_address(
        &[
            SWAP_DVP_SEED,
            settlement_authority.as_ref(),
            user_a.as_ref(),
            user_b.as_ref(),
            mint_a.as_ref(),
            mint_b.as_ref(),
            &nonce_bytes,
        ],
        &SWAP_PROGRAM_ID,
    )
}

pub fn nonce_tombstone_pda(swap_dvp: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[NONCE_TOMBSTONE_SEED, swap_dvp.as_ref()], &SWAP_PROGRAM_ID)
}

/// Vault PDA of the smart-wallet fixture for `vault_index`. Used as a
/// DvP party address to model a Squads-style vault: it can never sign a
/// transaction directly, only via the fixture's CPI.
pub fn smart_wallet_vault_pda(vault_index: u8) -> Pubkey {
    Pubkey::find_program_address(
        &[SMART_WALLET_VAULT_SEED, &[vault_index]],
        &SMART_WALLET_FIXTURE_PROGRAM_ID,
    )
    .0
}

/// Wrap `inner` so it is relayed through the smart-wallet fixture, which
/// re-signs it as `vault_index`'s vault PDA. The fixture takes the inner
/// program as account 0, then the inner accounts; the vault's signer bit
/// is dropped here (a PDA can't sign the outer tx) and re-added by the
/// fixture via seeds. Data is `[vault_index] ++ inner.data`.
pub fn wrap_via_smart_wallet(inner: Instruction, vault_index: u8) -> Instruction {
    let vault = smart_wallet_vault_pda(vault_index);
    let mut accounts = vec![AccountMeta::new_readonly(inner.program_id, false)];
    for meta in inner.accounts {
        let is_signer = meta.is_signer && meta.pubkey != vault;
        accounts.push(AccountMeta {
            pubkey: meta.pubkey,
            is_signer,
            is_writable: meta.is_writable,
        });
    }
    let mut data = vec![vault_index];
    data.extend_from_slice(&inner.data);
    Instruction {
        program_id: SMART_WALLET_FIXTURE_PROGRAM_ID,
        accounts,
        data,
    }
}

pub fn dvp_ata(swap_dvp: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(swap_dvp, mint, token_program)
}

pub fn assert_program_error(result: Result<TransactionMetadata, String>, expected_code: u32) {
    match result {
        Err(e) => {
            let expected = format!("Custom({})", expected_code);
            assert!(
                e.contains(&expected),
                "expected Custom({}) in error, got: {}",
                expected_code,
                e
            );
        }
        Ok(_) => panic!("expected tx to fail with Custom({})", expected_code),
    }
}

/// Assert the tx failed with a built-in `ProgramError` variant (anything
/// that surfaces as `InstructionError(_, <Variant>)` rather than
/// `Custom(N)` — `InvalidAccountData`, `InvalidSeeds`, etc.). Matches
/// the full `InstructionError(0, <Variant>)` substring so it can't
/// spuriously hit on the same variant name appearing elsewhere in the
/// error string.
pub fn assert_instruction_error(
    result: Result<TransactionMetadata, String>,
    expected_variant: &str,
) {
    match result {
        Err(e) => {
            let needle = format!("InstructionError(0, {})", expected_variant);
            assert!(
                e.contains(&needle),
                "expected {} in error, got: {}",
                needle,
                e
            );
        }
        Ok(_) => panic!(
            "expected tx to fail with InstructionError(_, {})",
            expected_variant
        ),
    }
}

// ---------------------------------------------------------------------
// Token-2022 mint builders with extensions.
//
// Each helper writes a properly TLV-encoded mint owned by Token-2022
// directly into the SVM. The mint's base fields (`decimals`,
// `is_initialized`) are set up so `TransferChecked` against the mint
// works; the extension is added on top so the program's
// `validate_mint_extensions` check can see it at CreateDvp.
// ---------------------------------------------------------------------

#[inline]
fn build_mint_2022_with_extensions(
    extensions: &[ExtensionType],
    init_extension: impl FnOnce(&mut PodStateWithExtensionsMut<PodMint>),
) -> Vec<u8> {
    let space = ExtensionType::try_calculate_account_len::<Token2022Mint>(extensions).unwrap();
    let mut data = vec![0u8; space];
    let mut state = PodStateWithExtensionsMut::<PodMint>::unpack_uninitialized(&mut data).unwrap();
    init_extension(&mut state);
    *state.base = PodMint {
        mint_authority: COption::None.into(),
        supply: 1_000_000_000_000u64.into(),
        decimals: 6,
        is_initialized: true.into(),
        freeze_authority: COption::None.into(),
    };
    state.init_account_type().unwrap();
    data
}

#[inline]
fn write_account(
    context: &mut TestContext,
    pubkey: &Pubkey,
    data: Vec<u8>,
    owner: Pubkey,
    lamports: u64,
) {
    context
        .svm
        .set_account(
            *pubkey,
            Account {
                lamports,
                data,
                owner,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

pub fn set_mint_2022_with_permanent_delegate(
    context: &mut TestContext,
    mint: &Pubkey,
    delegate: &Pubkey,
) {
    let data = build_mint_2022_with_extensions(&[ExtensionType::PermanentDelegate], |state| {
        let ext = state.init_extension::<PermanentDelegate>(true).unwrap();
        ext.delegate = MaybeNull::try_from(Some(*delegate)).unwrap();
    });
    write_account(context, mint, data, TOKEN_2022_PROGRAM_ID, 1_000_000_000);
}

pub fn set_mint_2022_with_pausable(context: &mut TestContext, mint: &Pubkey, authority: &Pubkey) {
    let data = build_mint_2022_with_extensions(&[ExtensionType::Pausable], |state| {
        let ext = state.init_extension::<PausableConfig>(true).unwrap();
        *ext = PausableConfig {
            paused: false.into(),
            authority: MaybeNull::try_from(Some(*authority)).unwrap(),
        };
    });
    write_account(context, mint, data, TOKEN_2022_PROGRAM_ID, 1_000_000_000);
}

pub fn set_mint_2022_with_transfer_hook(
    context: &mut TestContext,
    mint: &Pubkey,
    hook_program_id: &Pubkey,
    authority: &Pubkey,
) {
    let data = build_mint_2022_with_extensions(&[ExtensionType::TransferHook], |state| {
        let ext = state.init_extension::<TransferHook>(true).unwrap();
        *ext = TransferHook {
            authority: MaybeNull::try_from(Some(*authority)).unwrap(),
            program_id: MaybeNull::try_from(Some(*hook_program_id)).unwrap(),
        };
    });
    write_account(context, mint, data, TOKEN_2022_PROGRAM_ID, 1_000_000_000);
}

/// Writes a Token-2022 token account with the `TransferHookAccount`
/// extension initialized. Token-2022's `process_transfer` calls
/// `set_transferring` on both source and destination, which reads the
/// `TransferHookAccount` extension off each — a bare 165-byte account
/// would fail with `InvalidAccountData`. Real ATAs created by the SPL
/// ATA program against a hook-bearing mint already carry this
/// extension; this helper is the test-side equivalent for any user ATA
/// that was created *before* its mint gained `TransferHook` (or had its
/// data stomped by `set_token_balance`).
pub fn set_token_2022_with_hook_account(
    context: &mut TestContext,
    ata: &Pubkey,
    mint: &Pubkey,
    owner: &Pubkey,
    amount: u64,
) {
    use spl_token_2022_interface::pod::PodCOption;

    let space = ExtensionType::try_calculate_account_len::<Token2022Account>(&[
        ExtensionType::TransferHookAccount,
    ])
    .unwrap();
    let mut data = vec![0u8; space];
    let mut state =
        PodStateWithExtensionsMut::<PodAccount>::unpack_uninitialized(&mut data).unwrap();
    state.init_extension::<TransferHookAccount>(true).unwrap();
    *state.base = PodAccount {
        mint: *mint,
        owner: *owner,
        amount: amount.into(),
        delegate: PodCOption::none(),
        state: AccountState::Initialized as u8,
        is_native: PodCOption::none(),
        delegated_amount: 0u64.into(),
        close_authority: PodCOption::none(),
    };
    state.init_account_type().unwrap();
    write_account(context, ata, data, TOKEN_2022_PROGRAM_ID, 2_039_280);
}

/// Writes a Token-2022 token account with MemoTransfer enabled. Incoming
/// transfers then require a preceding Memo-program instruction.
pub fn set_token_2022_with_memo_required(
    context: &mut TestContext,
    ata: &Pubkey,
    mint: &Pubkey,
    owner: &Pubkey,
    amount: u64,
) {
    use spl_token_2022_interface::extension::memo_transfer::MemoTransfer;
    use spl_token_2022_interface::pod::PodCOption;

    let space = ExtensionType::try_calculate_account_len::<Token2022Account>(&[
        ExtensionType::MemoTransfer,
    ])
    .unwrap();
    let mut data = vec![0u8; space];
    let mut state =
        PodStateWithExtensionsMut::<PodAccount>::unpack_uninitialized(&mut data).unwrap();
    let ext = state.init_extension::<MemoTransfer>(true).unwrap();
    ext.require_incoming_transfer_memos = true.into();
    *state.base = PodAccount {
        mint: *mint,
        owner: *owner,
        amount: amount.into(),
        delegate: PodCOption::none(),
        state: AccountState::Initialized as u8,
        is_native: PodCOption::none(),
        delegated_amount: 0u64.into(),
        close_authority: PodCOption::none(),
    };
    state.init_account_type().unwrap();
    write_account(context, ata, data, TOKEN_2022_PROGRAM_ID, 2_039_280);
}

/// Writes the hook fixture's extra-account list without changing the mint.
/// Returns the validation PDA to append to the transfer's extra accounts.
pub fn set_hook_extra_account_metas(
    context: &mut TestContext,
    mint: &Pubkey,
    extras: &[AccountMeta],
) -> Pubkey {
    use spl_tlv_account_resolution::{account::ExtraAccountMeta, state::ExtraAccountMetaList};
    use spl_transfer_hook_interface::{
        get_extra_account_metas_address, instruction::ExecuteInstruction,
    };

    let validation_pda = get_extra_account_metas_address(mint, &HOOK_FIXTURE_PROGRAM_ID);
    let metas: Vec<_> = extras
        .iter()
        .map(|meta| {
            ExtraAccountMeta::new_with_pubkey(&meta.pubkey, meta.is_signer, meta.is_writable)
                .unwrap()
        })
        .collect();
    let mut data = vec![0; ExtraAccountMetaList::size_of(metas.len()).unwrap()];
    ExtraAccountMetaList::init::<ExecuteInstruction>(&mut data, &metas).unwrap();
    let lamports = context.svm.minimum_balance_for_rent_exemption(data.len());
    write_account(
        context,
        &validation_pda,
        data,
        HOOK_FIXTURE_PROGRAM_ID,
        lamports,
    );
    validation_pda
}

/// Sets up a Token-2022 mint that delegates to the test hook fixture
/// (`HOOK_FIXTURE_PROGRAM_ID`) and creates its `ExtraAccountMetaList`
/// validation PDA declaring **one** extra account: the system program.
///
/// The hook fixture only logs the account count it receives, so the
/// tests assert `hook accounts: N` where N is the standard 5 (source,
/// mint, destination, authority, validation PDA) plus the 1 extra we
/// declared here = **6**. Use [`hook_extras_for_mint`] to get the
/// trailing accounts the client must pass to Settle/Cancel/Reject/Reclaim
/// for this mint.
pub fn setup_hook_mint(context: &mut TestContext, mint: &Pubkey) {
    set_mint_2022_with_transfer_hook(
        context,
        mint,
        &HOOK_FIXTURE_PROGRAM_ID,
        &context.payer.pubkey(),
    );

    set_hook_extra_account_metas(
        context,
        mint,
        &[AccountMeta::new_readonly(
            solana_sdk_ids::system_program::ID,
            false,
        )],
    );
}

/// Returns the trailing accounts the client must append to a
/// `TransferChecked` (here surfaced as the swap program's
/// `leg_*_extras`) when transferring a mint set up via
/// [`setup_hook_mint`]. Order matches `spl_transfer_hook_interface`'s
/// offchain resolver: declared extras first, then the hook program ID,
/// then the validation PDA.
pub fn hook_extras_for_mint(mint: &Pubkey) -> Vec<solana_instruction::AccountMeta> {
    use solana_instruction::AccountMeta;
    use spl_transfer_hook_interface::get_extra_account_metas_address;

    let validation_pda = get_extra_account_metas_address(mint, &HOOK_FIXTURE_PROGRAM_ID);
    vec![
        AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
        AccountMeta::new_readonly(HOOK_FIXTURE_PROGRAM_ID, false),
        AccountMeta::new_readonly(validation_pda, false),
    ]
}

/// Like [`setup_hook_mint`], but the `ExtraAccountMetaList` declares two
/// extras aimed at draining a wallet: `victim` as a **signer-bearing**
/// static extra and `attacker` as a writable one. A generic client
/// resolver faithfully marks `victim` as a signer; the swap program
/// strips that bit before forwarding to the hook.
pub fn setup_malicious_hook_mint(
    context: &mut TestContext,
    mint: &Pubkey,
    victim: &Pubkey,
    attacker: &Pubkey,
) {
    set_mint_2022_with_transfer_hook(
        context,
        mint,
        &HOOK_FIXTURE_PROGRAM_ID,
        &context.payer.pubkey(),
    );

    // victim: signer + writable (a System transfer's source needs both) -
    // the signer bit is what the swap program strips. attacker: writable sink.
    // system program: so the hook can CPI it to move the drained lamports.
    set_hook_extra_account_metas(
        context,
        mint,
        &[
            AccountMeta::new(*victim, true),
            AccountMeta::new(*attacker, false),
            AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
        ],
    );
}

/// Trailing accounts for a mint set up via [`setup_malicious_hook_mint`]:
/// victim (marked writable so the client resolver would forward its
/// runtime signer bit), attacker, hook program, validation PDA.
pub fn malicious_hook_extras(
    mint: &Pubkey,
    victim: &Pubkey,
    attacker: &Pubkey,
) -> Vec<solana_instruction::AccountMeta> {
    use solana_instruction::AccountMeta;
    use spl_transfer_hook_interface::get_extra_account_metas_address;

    let validation_pda = get_extra_account_metas_address(mint, &HOOK_FIXTURE_PROGRAM_ID);
    vec![
        AccountMeta::new(*victim, false),
        AccountMeta::new(*attacker, false),
        AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
        AccountMeta::new_readonly(HOOK_FIXTURE_PROGRAM_ID, false),
        AccountMeta::new_readonly(validation_pda, false),
    ]
}

/// TransferFeeConfig — a blocked extension; used in negative tests.
pub fn set_mint_2022_with_transfer_fee(
    context: &mut TestContext,
    mint: &Pubkey,
    authority: &Pubkey,
    basis_points: u16,
    maximum_fee: u64,
) {
    use spl_token_2022_interface::extension::transfer_fee::TransferFee;
    use spl_token_2022_interface::pod::PodCOption;

    let data = build_mint_2022_with_extensions(&[ExtensionType::TransferFeeConfig], |state| {
        let ext = state.init_extension::<TransferFeeConfig>(true).unwrap();
        let auth = MaybeNull::try_from(Some(*authority)).unwrap();
        ext.transfer_fee_config_authority = auth;
        ext.withdraw_withheld_authority = auth;
        ext.withheld_amount = 0u64.into();
        ext.older_transfer_fee = TransferFee {
            epoch: 0u64.into(),
            maximum_fee: maximum_fee.into(),
            transfer_fee_basis_points: basis_points.into(),
        };
        ext.newer_transfer_fee = ext.older_transfer_fee;
        // Suppress unused-import / dead-field warnings for PodCOption when
        // some downstream `TransferFee` field types change versions.
        let _ = PodCOption::<Pubkey>::default();
    });
    write_account(context, mint, data, TOKEN_2022_PROGRAM_ID, 1_000_000_000);
}

/// InterestBearingConfig — blocked; used in negative tests.
pub fn set_mint_2022_with_interest_bearing(
    context: &mut TestContext,
    mint: &Pubkey,
    authority: &Pubkey,
) {
    let data = build_mint_2022_with_extensions(&[ExtensionType::InterestBearingConfig], |state| {
        let ext = state.init_extension::<InterestBearingConfig>(true).unwrap();
        ext.rate_authority = MaybeNull::try_from(Some(*authority)).unwrap();
        ext.initialization_timestamp = 0i64.into();
        ext.pre_update_average_rate = 0i16.into();
        ext.last_update_timestamp = 0i64.into();
        ext.current_rate = 0i16.into();
    });
    write_account(context, mint, data, TOKEN_2022_PROGRAM_ID, 1_000_000_000);
}

/// ScaledUiAmountConfig — blocked; used in negative tests.
pub fn set_mint_2022_with_scaled_ui_amount(
    context: &mut TestContext,
    mint: &Pubkey,
    authority: &Pubkey,
) {
    let data = build_mint_2022_with_extensions(&[ExtensionType::ScaledUiAmount], |state| {
        let ext = state.init_extension::<ScaledUiAmountConfig>(true).unwrap();
        ext.authority = MaybeNull::try_from(Some(*authority)).unwrap();
        ext.multiplier = 1.0f64.into();
        ext.new_multiplier_effective_timestamp = 0i64.into();
        ext.new_multiplier = 1.0f64.into();
    });
    write_account(context, mint, data, TOKEN_2022_PROGRAM_ID, 1_000_000_000);
}

/// NonTransferable — blocked; used in negative tests. A balance reaching
/// a non-transferable escrow can never be drained, so Create rejects it.
pub fn set_mint_2022_with_non_transferable(context: &mut TestContext, mint: &Pubkey) {
    let data = build_mint_2022_with_extensions(&[ExtensionType::NonTransferable], |state| {
        state.init_extension::<NonTransferable>(true).unwrap();
    });
    write_account(context, mint, data, TOKEN_2022_PROGRAM_ID, 1_000_000_000);
}

/// ConfidentialTransferMint: allowed; used in the lifecycle test.
pub fn set_mint_2022_with_confidential_transfer(
    context: &mut TestContext,
    mint: &Pubkey,
    authority: &Pubkey,
) {
    let data =
        build_mint_2022_with_extensions(&[ExtensionType::ConfidentialTransferMint], |state| {
            let ext = state
                .init_extension::<ConfidentialTransferMint>(true)
                .unwrap();
            ext.authority = MaybeNull::try_from(Some(*authority)).unwrap();
            ext.auto_approve_new_accounts = false.into();
            ext.auditor_elgamal_pubkey = Default::default();
        });
    write_account(context, mint, data, TOKEN_2022_PROGRAM_ID, 1_000_000_000);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_rejects_oversized_transaction() {
        let mut context = TestContext::new();
        let ix = Instruction {
            program_id: MEMO_PROGRAM_ID,
            accounts: vec![],
            data: vec![b'a'; PACKET_DATA_SIZE],
        };
        let tx = Transaction::new_signed_with_payer(
            std::slice::from_ref(&ix),
            Some(&context.payer.pubkey()),
            &[&context.payer],
            context.svm.latest_blockhash(),
        );
        let size = wincode::serialize(&tx).unwrap().len();

        assert_eq!(
            context.send(ix, &[]).unwrap_err(),
            format!("transaction size {size} exceeds limit {PACKET_DATA_SIZE}")
        );
    }
}
