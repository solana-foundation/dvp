//! Shared instruction builders that return `Result` (the upstream
//! `assert_*` helpers panic on failure, which a model-based test cannot use).
//! Every builder takes the per-leg transfer-hook extras (`Extras`); for
//! legs without a hook they are empty.

use dvp_swap_program_client::instructions::{
    CancelDvpBuilder, CreateDvpBuilder, RejectDvpBuilder, SettleDvpBuilder,
};
use litesvm::types::TransactionMetadata;
use solana_instruction::AccountMeta;
use solana_keypair::Keypair;
use solana_signer::Signer;

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
