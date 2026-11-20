//! Leg configurations: which token program and mint extensions each leg
//! uses, and the account setup that goes with them (mirrors the upstream
//! `test_token_2022` setup: hook mint + `TransferHookAccount` on every
//! account that will hold the hook mint).

use proptest::prelude::*;
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;

use super::helpers::Extras;
use crate::state_utils::{setup_dvp_with_programs, DvpFixture, INITIAL_BALANCE};
use crate::utils::{
    create_ata, fund_wallet_ata, hook_extras_for_mint, set_token_2022_with_hook_account,
    setup_hook_mint, TestContext, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LegKind {
    Legacy,
    T22,
    T22Hook,
}

pub fn leg_kind() -> impl Strategy<Value = LegKind> {
    prop_oneof![
        Just(LegKind::Legacy),
        Just(LegKind::T22),
        Just(LegKind::T22Hook)
    ]
}

impl LegKind {
    pub fn program(self) -> Address {
        match self {
            LegKind::Legacy => TOKEN_PROGRAM_ID,
            LegKind::T22 | LegKind::T22Hook => TOKEN_2022_PROGRAM_ID,
        }
    }
}

/// Mint-A balance given to the third-party donor of "dust".
pub const DONOR_BALANCE: u64 = 1_000;

/// Extra wallets around leg A for the dust / closed-refund-ATA scenarios.
pub struct Aux {
    pub donor: Keypair,
    pub donor_ata_a: Address,
    /// ATA of user_a's second wallet, where user_a moves mint A before closing the ATA.
    pub side_ata_a: Address,
}

/// Build a DvP fixture whose legs use `ka` / `kb`, plus the aux wallets.
pub fn setup_legs(
    ctx: &mut TestContext,
    nonce: u64,
    ka: LegKind,
    kb: LegKind,
) -> (DvpFixture, Extras, Aux) {
    let f = setup_dvp_with_programs(ctx, nonce, ka.program(), kb.program());
    let mut x = Extras::default();

    let donor = Keypair::new();
    let side = Keypair::new();
    ctx.airdrop_if_required(&side.pubkey(), 1_000_000_000);

    let (donor_ata_a, side_ata_a);
    if ka == LegKind::T22Hook {
        setup_hook_mint(ctx, &f.mint_a);
        set_token_2022_with_hook_account(
            ctx,
            &f.user_a_ata_a,
            &f.mint_a,
            &f.user_a.pubkey(),
            INITIAL_BALANCE,
        );
        set_token_2022_with_hook_account(ctx, &f.user_b_ata_a, &f.mint_a, &f.user_b.pubkey(), 0);
        // Accounts written directly (same layout an ATA would have).
        ctx.airdrop_if_required(&donor.pubkey(), 1_000_000_000);
        donor_ata_a = get_associated_token_address_with_program_id(
            &donor.pubkey(),
            &f.mint_a,
            &TOKEN_2022_PROGRAM_ID,
        );
        set_token_2022_with_hook_account(
            ctx,
            &donor_ata_a,
            &f.mint_a,
            &donor.pubkey(),
            DONOR_BALANCE,
        );
        side_ata_a = get_associated_token_address_with_program_id(
            &side.pubkey(),
            &f.mint_a,
            &TOKEN_2022_PROGRAM_ID,
        );
        set_token_2022_with_hook_account(ctx, &side_ata_a, &f.mint_a, &side.pubkey(), 0);
        x.a = hook_extras_for_mint(&f.mint_a);
    } else {
        donor_ata_a = fund_wallet_ata(ctx, &donor, &f.mint_a, DONOR_BALANCE, &ka.program());
        side_ata_a = create_ata(ctx, &side.pubkey(), &f.mint_a, &ka.program());
    }

    if kb == LegKind::T22Hook {
        setup_hook_mint(ctx, &f.mint_b);
        set_token_2022_with_hook_account(
            ctx,
            &f.user_b_ata_b,
            &f.mint_b,
            &f.user_b.pubkey(),
            INITIAL_BALANCE,
        );
        set_token_2022_with_hook_account(ctx, &f.user_a_ata_b, &f.mint_b, &f.user_a.pubkey(), 0);
        x.b = hook_extras_for_mint(&f.mint_b);
    }

    (
        f,
        x,
        Aux {
            donor,
            donor_ata_a,
            side_ata_a,
        },
    )
}
