//! Settlement time window and who may terminate a DvP. Upstream has a single
//! test for `earliest_settlement_timestamp`; these explore its boundaries.
//!
//! Time window:
//! - Create succeeds iff 0 < expiry - now <= 365 days and earliest is unset
//!   or <= expiry.
//! - Settle by the authority, both legs funded, succeeds iff now <= expiry
//!   and earliest is unset or now >= earliest.
//! - Probe times are drawn only from {earliest, expiry} ± 1 plus one random
//!   point, so each boundary is hit in roughly one case out of seven.
//!
//! Permissions, at a time inside the window: only the authority may Settle
//! or Cancel, only user_a or user_b may Reject. Every (signer, action) pair
//! is drawn with equal probability.

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use solana_keypair::Keypair;
use solana_signer::Signer;

use super::coverage::Coverage;
use super::helpers::{cancel, create, reject, settle, Extras};
use crate::state_utils::{assert_create_dvp, assert_fund_a, assert_fund_b, setup_dvp};
use crate::utils::TestContext;

const MAX_DURATION: i64 = 365 * 24 * 60 * 60;

static COV_TIME: Coverage = Coverage::new("settle_time_window");
static COV_PERM: Coverage = Coverage::new("permissions");

// ---------- time window ----------

fn expiry_offset() -> impl Strategy<Value = i64> {
    prop_oneof![
        1 => Just(-1i64),
        1 => Just(0i64),
        1 => Just(MAX_DURATION),
        1 => Just(MAX_DURATION + 1),
        8 => 2i64..7200,
    ]
}

fn earliest_offset(expiry: i64) -> impl Strategy<Value = Option<i64>> {
    prop_oneof![
        1 => Just(None),
        1 => Just(Some(expiry)),
        1 => Just(Some(expiry + 1)),
        1 => Just(Some(0)),
        1 => Just(Some(-10)),
        3 => (1i64..expiry.max(2)).prop_map(Some),
    ]
}

#[derive(Clone, Copy, Debug)]
enum Probe {
    EarliestMinus1,
    Earliest,
    EarliestPlus1,
    ExpiryMinus1,
    Expiry,
    ExpiryPlus1,
    Random(i64),
}

fn probe() -> impl Strategy<Value = Probe> {
    prop_oneof![
        Just(Probe::EarliestMinus1),
        Just(Probe::Earliest),
        Just(Probe::EarliestPlus1),
        Just(Probe::ExpiryMinus1),
        Just(Probe::Expiry),
        Just(Probe::ExpiryPlus1),
        (0i64..8000).prop_map(Probe::Random),
    ]
}

fn time_case() -> impl Strategy<Value = (i64, Option<i64>, Probe)> {
    expiry_offset().prop_flat_map(|exp| (Just(exp), earliest_offset(exp), probe()))
}

fn run_time_case(exp: i64, ear: Option<i64>, p: Probe) -> Result<(), TestCaseError> {
    let mut ctx = TestContext::new();
    let f = setup_dvp(&mut ctx, 0);
    let now0 = ctx.now();

    let create_ok = exp > 0 && exp <= MAX_DURATION && ear.is_none_or(|e| e <= exp);
    let created = create(&mut ctx, &f, now0 + exp, ear.map(|e| now0 + e));
    prop_assert_eq!(
        created.is_ok(),
        create_ok,
        "CreateDvp expiry_off={} earliest_off={:?}: {:?}",
        exp,
        ear,
        created.err()
    );
    COV_TIME.hit(if create_ok {
        "create_ok"
    } else {
        "create_rejected"
    });
    if !create_ok {
        return Ok(());
    }

    assert_fund_a(&mut ctx, &f);
    assert_fund_b(&mut ctx, &f);

    let e = ear.unwrap_or(0);
    let t = match p {
        Probe::EarliestMinus1 => e - 1,
        Probe::Earliest => e,
        Probe::EarliestPlus1 => e + 1,
        Probe::ExpiryMinus1 => exp - 1,
        Probe::Expiry => exp,
        Probe::ExpiryPlus1 => exp + 1,
        Probe::Random(r) => r,
    }
    .max(0); // the clock only moves forward
    ctx.advance_clock(t);
    prop_assert_eq!(ctx.now() - now0, t);

    let expected = t <= exp && ear.is_none_or(|e| t >= e);
    let res = settle(&mut ctx, &f, &Extras::default(), &f.settlement_authority);
    prop_assert_eq!(
        res.is_ok(),
        expected,
        "Settle at t={} (expiry={}, earliest={:?}): {:?}",
        t,
        exp,
        ear,
        res.err()
    );

    if t == exp {
        COV_TIME.hit("settle_at_expiry");
    }
    if t == exp + 1 {
        COV_TIME.hit("settle_at_expiry_plus1");
    }
    if let Some(e) = ear {
        if t == e && e > 0 {
            COV_TIME.hit("settle_at_earliest");
        }
        if t == e - 1 && e > 0 {
            COV_TIME.hit("settle_at_earliest_minus1");
        }
    }
    Ok(())
}

#[test]
fn prop_settle_time_window() {
    let mut runner = super::coverage::seeded_runner();
    if let Err(e) = runner.run(&time_case(), |(exp, ear, p)| run_time_case(exp, ear, p)) {
        panic!("{e}");
    }
    COV_TIME.report_and_check(
        runner.config().cases,
        &[
            ("create_rejected", 3),
            ("settle_at_expiry", 2),
            ("settle_at_expiry_plus1", 2),
            ("settle_at_earliest", 1),
            ("settle_at_earliest_minus1", 1),
        ],
    );
}

// ---------- who may terminate ----------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Who {
    Authority,
    UserA,
    UserB,
    Stranger,
}

#[derive(Clone, Copy, Debug)]
enum Action {
    Settle,
    Cancel,
    Reject,
}

fn perm_case() -> impl Strategy<Value = (Who, Action, i64)> {
    (
        prop_oneof![
            Just(Who::Authority),
            Just(Who::UserA),
            Just(Who::UserB),
            Just(Who::Stranger)
        ],
        prop_oneof![
            Just(Action::Settle),
            Just(Action::Cancel),
            Just(Action::Reject)
        ],
        0i64..3600, // strictly inside the default 1 h window
    )
}

fn run_perm_case(who: Who, action: Action, t: i64) -> Result<(), TestCaseError> {
    let mut ctx = TestContext::new();
    let f = setup_dvp(&mut ctx, 0);
    assert_create_dvp(&mut ctx, &f);
    assert_fund_a(&mut ctx, &f);
    assert_fund_b(&mut ctx, &f);
    ctx.advance_clock(t);

    let stranger = Keypair::new();
    ctx.airdrop_if_required(&stranger.pubkey(), 1_000_000_000);
    let signer = match who {
        Who::Authority => &f.settlement_authority,
        Who::UserA => &f.user_a,
        Who::UserB => &f.user_b,
        Who::Stranger => &stranger,
    };
    let (res, expected, cell) = match action {
        Action::Settle => (
            settle(&mut ctx, &f, &Extras::default(), signer),
            who == Who::Authority,
            "settle",
        ),
        Action::Cancel => (
            cancel(&mut ctx, &f, &Extras::default(), signer),
            who == Who::Authority,
            "cancel",
        ),
        Action::Reject => (
            reject(&mut ctx, &f, &Extras::default(), signer),
            matches!(who, Who::UserA | Who::UserB),
            "reject",
        ),
    };
    prop_assert_eq!(
        res.is_ok(),
        expected,
        "{:?} by {:?} at t={}: {:?}",
        action,
        who,
        t,
        res.err()
    );
    COV_PERM.hit(if expected { cell } else { "denied" });
    Ok(())
}

#[test]
fn prop_permissions() {
    let mut runner = super::coverage::seeded_runner();
    if let Err(e) = runner.run(&perm_case(), |(w, a, t)| run_perm_case(w, a, t)) {
        panic!("{e}");
    }
    COV_PERM.report_and_check(
        runner.config().cases,
        &[("settle", 2), ("cancel", 2), ("reject", 4), ("denied", 20)],
    );
}
