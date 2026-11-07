//! Shared instruction builders that return `Result` (the upstream
//! `assert_*` helpers panic on failure, which a model-based test cannot use).
//! Every builder takes the per-leg transfer-hook extras (`Extras`); for
//! legs without a hook they are empty.

use dvp_swap_program_client::instructions::{
    CancelDvpBuilder, CreateDvpBuilder, ReclaimDvpBuilder, RejectDvpBuilder, SettleDvpBuilder,
};
use litesvm::types::TransactionMetadata;
use solana_address::Address;
use solana_instruction::AccountMeta;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spl_token_2022_interface::instruction::{close_account, transfer_checked};

use crate::state_utils::{DvpFixture, AMOUNT_A, AMOUNT_B, REF_STRING};
use crate::utils::{TestContext, MEMO_PROGRAM_ID};

pub type TxResult = Result<TransactionMetadata, String>;

/// Trailing hook accounts per leg (empty when the leg's mint has no hook).
#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub a: Vec<AccountMeta>,
    pub b: Vec<AccountMeta>,
}

impl Extras {
    fn leg(&self, leg_a: bool) -> &[AccountMeta] {
        if leg_a {
            &self.a
        } else {
            &self.b
        }
    }
    fn both(&self) -> Vec<AccountMeta> {
        let mut v = self.a.clone();
        v.extend_from_slice(&self.b);
        v
    }
}

pub fn create(
    ctx: &mut TestContext,
    f: &DvpFixture,
    expiry: i64,
    earliest: Option<i64>,
) -> TxResult {
    let mut b = CreateDvpBuilder::new();
    b.payer(ctx.payer.pubkey())
        .swap_dvp(f.swap_dvp)
        .nonce_tombstone(f.nonce_tombstone)
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .dvp_ata_a(f.dvp_ata_a)
        .dvp_ata_b(f.dvp_ata_b)
        .token_program_a(f.token_program_a)
        .token_program_b(f.token_program_b)
        .user_a(f.user_a.pubkey())
        .user_b(f.user_b.pubkey())
        .settlement_authority(f.settlement_authority.pubkey())
        .amount_a(AMOUNT_A)
        .amount_b(AMOUNT_B)
        .expiry_timestamp(expiry)
        .nonce(f.nonce)
        .ref_string(REF_STRING.to_string());
    if let Some(e) = earliest {
        b.earliest_settlement_timestamp(e);
    }
    ctx.send(b.instruction(), &[])
}

fn leg_accounts(f: &DvpFixture, leg_a: bool) -> (Address, Address) {
    if leg_a {
        (f.mint_a, f.token_program_a)
    } else {
        (f.mint_b, f.token_program_b)
    }
}

/// Plain token transfer of a leg's mint between two accounts, with the
/// leg's hook extras appended (decimals 6 as in `set_mint`).
#[allow(clippy::too_many_arguments)]
pub fn transfer(
    ctx: &mut TestContext,
    f: &DvpFixture,
    x: &Extras,
    leg_a: bool,
    owner: &Keypair,
    src: &Address,
    dst: &Address,
    amount: u64,
) -> TxResult {
    let (mint, tp) = leg_accounts(f, leg_a);
    let mut ix = transfer_checked(&tp, src, &mint, dst, &owner.pubkey(), &[], amount, 6)
        .map_err(|e| format!("{e:?}"))?;
    ix.accounts.extend_from_slice(x.leg(leg_a));
    ctx.send(ix, &[owner])
}

/// Deposit `amount` of a leg's mint from its depositor into its escrow
/// (raw TransferChecked, the canonical funding path).
pub fn fund(
    ctx: &mut TestContext,
    f: &DvpFixture,
    x: &Extras,
    leg_a: bool,
    amount: u64,
) -> TxResult {
    let (user, src, dst) = if leg_a {
        (&f.user_a, f.user_a_ata_a, f.dvp_ata_a)
    } else {
        (&f.user_b, f.user_b_ata_b, f.dvp_ata_b)
    };
    transfer(ctx, f, x, leg_a, user, &src, &dst, amount)
}

/// Close a token account owned by `owner` (balance must be zero).
pub fn close_token_account(
    ctx: &mut TestContext,
    f: &DvpFixture,
    leg_a: bool,
    owner: &Keypair,
    account: &Address,
) -> TxResult {
    let (_, tp) = leg_accounts(f, leg_a);
    let ix = close_account(&tp, account, &owner.pubkey(), &owner.pubkey(), &[])
        .map_err(|e| format!("{e:?}"))?;
    ctx.send(ix, &[owner])
}

pub fn reclaim(
    ctx: &mut TestContext,
    f: &DvpFixture,
    x: &Extras,
    signer: &Keypair,
    leg_a: bool,
) -> TxResult {
    let (mint, src, dst, tp) = if leg_a {
        (f.mint_a, f.dvp_ata_a, f.user_a_ata_a, f.token_program_a)
    } else {
        (f.mint_b, f.dvp_ata_b, f.user_b_ata_b, f.token_program_b)
    };
    let ix = ReclaimDvpBuilder::new()
        .signer(signer.pubkey())
        .swap_dvp(f.swap_dvp)
        .mint(mint)
        .dvp_source_ata(src)
        .signer_dest_ata(dst)
        .token_program(tp)
        .memo_program(MEMO_PROGRAM_ID)
        .add_remaining_accounts(x.leg(leg_a))
        .instruction();
    ctx.send(ix, &[signer])
}

pub fn settle(ctx: &mut TestContext, f: &DvpFixture, x: &Extras, signer: &Keypair) -> TxResult {
    let ix = SettleDvpBuilder::new()
        .settlement_authority(signer.pubkey())
        .swap_dvp(f.swap_dvp)
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .dvp_ata_a(f.dvp_ata_a)
        .dvp_ata_b(f.dvp_ata_b)
        .user_a_destination_ata_b(f.user_a_ata_b)
        .user_b_destination_ata_a(f.user_b_ata_a)
        .user_a_ata_a(f.user_a_ata_a)
        .user_b_ata_b(f.user_b_ata_b)
        .token_program_a(f.token_program_a)
        .token_program_b(f.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(x.a.len() as u8)
        .add_remaining_accounts(&x.both())
        .instruction();
    ctx.send(ix, &[signer])
}

pub fn cancel(ctx: &mut TestContext, f: &DvpFixture, x: &Extras, signer: &Keypair) -> TxResult {
    let ix = CancelDvpBuilder::new()
        .settlement_authority(signer.pubkey())
        .swap_dvp(f.swap_dvp)
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .dvp_ata_a(f.dvp_ata_a)
        .dvp_ata_b(f.dvp_ata_b)
        .user_a_ata_a(f.user_a_ata_a)
        .user_b_ata_b(f.user_b_ata_b)
        .token_program_a(f.token_program_a)
        .token_program_b(f.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(x.a.len() as u8)
        .add_remaining_accounts(&x.both())
        .instruction();
    ctx.send(ix, &[signer])
}

pub fn reject(ctx: &mut TestContext, f: &DvpFixture, x: &Extras, signer: &Keypair) -> TxResult {
    let ix = RejectDvpBuilder::new()
        .signer(signer.pubkey())
        .swap_dvp(f.swap_dvp)
        .mint_a(f.mint_a)
        .mint_b(f.mint_b)
        .dvp_ata_a(f.dvp_ata_a)
        .dvp_ata_b(f.dvp_ata_b)
        .user_a_ata_a(f.user_a_ata_a)
        .user_b_ata_b(f.user_b_ata_b)
        .token_program_a(f.token_program_a)
        .token_program_b(f.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(x.a.len() as u8)
        .add_remaining_accounts(&x.both())
        .instruction();
    ctx.send(ix, &[signer])
}

/// True if the account is gone (absent, or drained and zeroed).
pub fn is_closed(ctx: &TestContext, key: &Address) -> bool {
    match ctx.get_account(key) {
        None => true,
        Some(a) => a.lamports == 0,
    }
}
