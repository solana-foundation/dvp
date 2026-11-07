//! Mint extensions.
//!
//! Each leg gets one mint configuration out of ten. Oracle:
//! - Create ok ⇔ neither leg's mint carries a denied extension
//!   (TransferFee, InterestBearing, ScaledUiAmount, NonTransferable);
//! - for accepted configurations, an exactly funded DvP settles and both
//!   counterparties receive exactly the agreed amounts;
//! - if, after Create, a Token-2022 leg's mint is replaced by one with a
//!   denied extension (close + recreate), Settle must be rejected, while
//!   Cancel must still refund both depositors in full (recovery paths do not
//!   re-check extensions).

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use solana_address::Address;
use solana_instruction::AccountMeta;
use solana_signer::Signer;

use super::coverage::Coverage;
use super::helpers::{cancel, create, failed_with, fund, is_closed, settle, Extras};
use crate::state_utils::{
    setup_dvp_with_programs, DvpFixture, AMOUNT_A, AMOUNT_B, INITIAL_BALANCE,
};
use crate::utils::{
    get_token_balance, hook_extras_for_mint, set_mint_2022_with_confidential_transfer,
    set_mint_2022_with_interest_bearing, set_mint_2022_with_non_transferable,
    set_mint_2022_with_pausable, set_mint_2022_with_permanent_delegate,
    set_mint_2022_with_scaled_ui_amount, set_mint_2022_with_transfer_fee,
    set_token_2022_with_hook_account, setup_hook_mint, TestContext, BLOCKED_MINT_EXTENSION,
    TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};

static COV: Coverage = Coverage::new("extensions");

#[derive(Clone, Copy, Debug, PartialEq)]
enum MintCfg {
    Legacy,
    Bare22,
    TransferFee,
    InterestBearing,
    ScaledUi,
    NonTransferable,
    PermanentDelegate,
    Pausable,
    Hook,
    Confidential,
}

impl MintCfg {
    fn denied(self) -> bool {
        matches!(
            self,
            MintCfg::TransferFee
                | MintCfg::InterestBearing
                | MintCfg::ScaledUi
                | MintCfg::NonTransferable
        )
    }
    fn program(self) -> Address {
        if self == MintCfg::Legacy {
            TOKEN_PROGRAM_ID
        } else {
            TOKEN_2022_PROGRAM_ID
        }
    }
    fn name(self) -> &'static str {
        match self {
            MintCfg::Legacy => "legacy",
            MintCfg::Bare22 => "bare22",
            MintCfg::TransferFee => "transfer_fee",
            MintCfg::InterestBearing => "interest_bearing",
            MintCfg::ScaledUi => "scaled_ui",
            MintCfg::NonTransferable => "non_transferable",
            MintCfg::PermanentDelegate => "permanent_delegate",
            MintCfg::Pausable => "pausable",
            MintCfg::Hook => "hook",
            MintCfg::Confidential => "confidential",
        }
    }
}

/// Allowed configurations are weighted 3:1 over denied ones: with uniform
/// weights only ~1/3 of cases pass Create, too few to reliably reach the
/// allowed-extension coverage minimums.
fn mint_cfg() -> impl Strategy<Value = MintCfg> {
    prop_oneof![
        3 => Just(MintCfg::Legacy),
        3 => Just(MintCfg::Bare22),
        1 => Just(MintCfg::TransferFee),
        1 => Just(MintCfg::InterestBearing),
        1 => Just(MintCfg::ScaledUi),
        1 => Just(MintCfg::NonTransferable),
        3 => Just(MintCfg::PermanentDelegate),
        3 => Just(MintCfg::Pausable),
        3 => Just(MintCfg::Hook),
        3 => Just(MintCfg::Confidential),
    ]
}

fn denied_cfg() -> impl Strategy<Value = MintCfg> {
    prop_oneof![
        Just(MintCfg::TransferFee),
        Just(MintCfg::InterestBearing),
        Just(MintCfg::ScaledUi),
        Just(MintCfg::NonTransferable),
    ]
}

/// Write `cfg` onto an existing mint address (and hook-aware holder accounts for Hook).
fn apply(ctx: &mut TestContext, f: &DvpFixture, leg_a: bool, cfg: MintCfg) -> Vec<AccountMeta> {
    let mint = if leg_a { f.mint_a } else { f.mint_b };
    let authority = f.settlement_authority.pubkey(); // any key; not the DvP's concern
    match cfg {
        MintCfg::Legacy | MintCfg::Bare22 => {}
        MintCfg::TransferFee => set_mint_2022_with_transfer_fee(ctx, &mint, &authority, 100, 1_000),
        MintCfg::InterestBearing => set_mint_2022_with_interest_bearing(ctx, &mint, &authority),
        MintCfg::ScaledUi => set_mint_2022_with_scaled_ui_amount(ctx, &mint, &authority),
        MintCfg::NonTransferable => set_mint_2022_with_non_transferable(ctx, &mint),
        MintCfg::PermanentDelegate => set_mint_2022_with_permanent_delegate(ctx, &mint, &authority),
        MintCfg::Pausable => set_mint_2022_with_pausable(ctx, &mint, &authority),
        MintCfg::Confidential => set_mint_2022_with_confidential_transfer(ctx, &mint, &authority),
        MintCfg::Hook => {
            setup_hook_mint(ctx, &mint);
            let (depositor, dep_ata, counter, counter_ata) = if leg_a {
                (
                    f.user_a.pubkey(),
                    f.user_a_ata_a,
                    f.user_b.pubkey(),
                    f.user_b_ata_a,
                )
            } else {
                (
                    f.user_b.pubkey(),
                    f.user_b_ata_b,
                    f.user_a.pubkey(),
                    f.user_a_ata_b,
                )
            };
            set_token_2022_with_hook_account(ctx, &dep_ata, &mint, &depositor, INITIAL_BALANCE);
            set_token_2022_with_hook_account(ctx, &counter_ata, &mint, &counter, 0);
            return hook_extras_for_mint(&mint);
        }
    }
    vec![]
}

/// Post-create mutation: which leg (if any) gets its mint swapped for a denied one.
fn case() -> impl Strategy<Value = (MintCfg, MintCfg, Option<(bool, MintCfg)>)> {
    (
        mint_cfg(),
        mint_cfg(),
        prop_oneof![2 => Just(None), 1 => (any::<bool>(), denied_cfg()).prop_map(Some)],
    )
}

fn run_case(
    ca: MintCfg,
    cb: MintCfg,
    mutate: Option<(bool, MintCfg)>,
) -> Result<(), TestCaseError> {
    let mut ctx = TestContext::new();
    let f = setup_dvp_with_programs(&mut ctx, 0, ca.program(), cb.program());
    let x = Extras {
        a: apply(&mut ctx, &f, true, ca),
        b: apply(&mut ctx, &f, false, cb),
    };

    let create_ok = !ca.denied() && !cb.denied();
    let r = create(&mut ctx, &f, f.expiry, None);
    if !create_ok {
        prop_assert!(
            failed_with(&r, BLOCKED_MINT_EXTENSION),
            "Create {:?}/{:?} must fail with BlockedMintExtension: {:?}",
            ca,
            cb,
            r.err()
        );
        COV.hit("create_denied");
        return Ok(());
    }
    prop_assert!(r.is_ok(), "Create {:?}/{:?}: {:?}", ca, cb, r.err());
    COV.hit(ca.name());
    COV.hit(cb.name());

    // Post-create mutation, modelled on the attack described in `settle_dvp.rs`: a party
    // leaves its OWN leg unfunded, closes that zero-supply mint and recreates it
    // with a denied extension. Only the unfunded leg can be swapped — a mint
    // whose tokens sit in escrow has non-zero supply and cannot be closed.
    // Only Token-2022 legs (a legacy mint cannot carry extensions).
    let mutate = mutate.filter(|(leg_a, _)| if *leg_a { ca } else { cb } != MintCfg::Legacy);
    if let Some((swap_a, bad)) = mutate {
        // Fund only the other leg.
        fund(
            &mut ctx,
            &f,
            &x,
            !swap_a,
            if swap_a { AMOUNT_B } else { AMOUNT_A },
        )
        .map_err(|e| TestCaseError::fail(format!("fund other leg: {e}")))?;
        apply(&mut ctx, &f, swap_a, bad);
        // The replaced mint has no hook any more: drop that leg's hook extras.
        let mut x = x.clone();
        if swap_a {
            x.a.clear()
        } else {
            x.b.clear()
        }
        let leg = if swap_a { "A" } else { "B" };
        let s = settle(&mut ctx, &f, &x, &f.settlement_authority);
        prop_assert!(
            failed_with(&s, BLOCKED_MINT_EXTENSION),
            "Settle after leg {} mint became {:?} ({:?}/{:?}) must fail with \
             BlockedMintExtension: {:?}",
            leg,
            bad,
            ca,
            cb,
            s.err()
        );
        let c = cancel(&mut ctx, &f, &x, &f.settlement_authority);
        prop_assert!(
            c.is_ok(),
            "Cancel blocked after leg {} mint became {:?} ({:?}/{:?}): {:?}",
            leg,
            bad,
            ca,
            cb,
            c.err()
        );
        prop_assert_eq!(
            get_token_balance(&ctx, &f.user_a_ata_a),
            INITIAL_BALANCE,
            "leg A not fully refunded"
        );
        prop_assert_eq!(
            get_token_balance(&ctx, &f.user_b_ata_b),
            INITIAL_BALANCE,
            "leg B not fully refunded"
        );
        prop_assert!(is_closed(&ctx, &f.swap_dvp), "SwapDvp not closed by Cancel");
        COV.hit("post_create_mutation");
        return Ok(());
    }

    fund(&mut ctx, &f, &x, true, AMOUNT_A)
        .map_err(|e| TestCaseError::fail(format!("fund A {ca:?}: {e}")))?;
    fund(&mut ctx, &f, &x, false, AMOUNT_B)
        .map_err(|e| TestCaseError::fail(format!("fund B {cb:?}: {e}")))?;

    let s = settle(&mut ctx, &f, &x, &f.settlement_authority);
    prop_assert!(s.is_ok(), "Settle {:?}/{:?}: {:?}", ca, cb, s.err());
    prop_assert_eq!(get_token_balance(&ctx, &f.user_b_ata_a), AMOUNT_A);
    prop_assert_eq!(get_token_balance(&ctx, &f.user_a_ata_b), AMOUNT_B);
    prop_assert_eq!(
        get_token_balance(&ctx, &f.user_a_ata_a),
        INITIAL_BALANCE - AMOUNT_A
    );
    prop_assert_eq!(
        get_token_balance(&ctx, &f.user_b_ata_b),
        INITIAL_BALANCE - AMOUNT_B
    );
    COV.hit("settled");
    Ok(())
}

#[test]
fn prop_mint_extensions() {
    let mut runner = super::coverage::seeded_runner();
    if let Err(e) = runner.run(&case(), |(a, b, m)| run_case(a, b, m)) {
        panic!("{e}");
    }
    let k = (runner.config().cases as usize / 64).max(1);
    COV.report_and_check(&[
        ("create_denied", 10 * k),
        ("settled", 5 * k),
        ("post_create_mutation", 3 * k),
        ("hook", 2 * k),
        ("pausable", 2 * k),
        ("permanent_delegate", 2 * k),
        ("confidential", 2 * k),
    ]);
}
