//! Model-based lifecycle over every leg configuration
//! (legacy SPL Token / Token-2022 / Token-2022 + transfer hook on each leg).
//! Random sequences of Fund / Reclaim / Settle / Cancel / Reject / clock
//! moves / wrong signers, plus the Cantina L3 scenario on leg A: a third
//! party donates "dust" into escrow A, user_a empties and closes the refund
//! ATA, anyone recreates it.
//!
//! Checked after every step:
//! - success/failure of each instruction matches the model;
//! - every tracked token balance matches the model exactly;
//! - per mint, the total over all accounts that can hold it never changes;
//! - after any terminal instruction the SwapDvp PDA and both escrows are closed.
//!
//! Model of L3, as documented on `user_a_ata_a` in `settle_dvp.rs`: a refund
//! into a missing ATA fails, so Settle with surplus on leg A, and Cancel /
//! Reject / Reclaim with a non-empty escrow A, are rejected while user_a's
//! ATA is closed — and succeed again once it is recreated.

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;

use super::coverage::Coverage;
use super::helpers::{
    cancel, close_token_account, fund, is_closed, reclaim, reject, settle, transfer, Extras,
};
use super::legs::{leg_kind, setup_legs, Aux, LegKind, DONOR_BALANCE};
use crate::state_utils::{assert_create_dvp, DvpFixture, AMOUNT_A, AMOUNT_B, INITIAL_BALANCE};
use crate::utils::{get_token_balance, TestContext};

static COV: Coverage = Coverage::new("lifecycle");

#[derive(Clone, Debug)]
enum Op {
    FundA(u64),
    FundB(u64),
    ReclaimA,
    ReclaimB,
    Settle,
    Cancel,
    RejectA,
    RejectB,
    /// Wrong signers: a party posing as settlement authority, a party
    /// cancelling, the authority rejecting, the authority reclaiming.
    SettleByUserA,
    CancelByUserB,
    RejectByAuthority,
    ReclaimByAuthority,
    /// L3: a third party sends mint A into escrow A.
    DonorDustA(u64),
    /// L3: user_a moves all mint A to a side wallet and closes the ATA.
    CloseRefundAtaA,
    /// Anyone (the payer) recreates user_a's mint-A ATA (idempotent).
    RecreateAtaA,
    Advance(i64),
}

fn surplus() -> impl Strategy<Value = u64> {
    // Exact funding has its own prefix below, so favour a surplus here.
    prop_oneof![1 => Just(0u64), 2 => 1u64..=50]
}

fn prefix() -> impl Strategy<Value = Vec<Op>> {
    prop_oneof![
        3 => (surplus(), surplus())
            .prop_map(|(a, b)| vec![Op::FundA(AMOUNT_A + a), Op::FundB(AMOUNT_B + b)]),
        1 => (surplus(), surplus())
            .prop_map(|(a, b)| vec![Op::FundB(AMOUNT_B + b), Op::FundA(AMOUNT_A + a)]),
        1 => Just(vec![]),
        // Exact funding settled right away (dust ops otherwise make exact settles rare).
        1 => Just(vec![Op::FundA(AMOUNT_A), Op::FundB(AMOUNT_B), Op::Settle]),
        // Cantina L3 set-up: dust from a third party, then user_a closes the refund ATA.
        1 => (surplus(), 1u64..=DONOR_BALANCE / 4).prop_map(|(b, d)| vec![
            Op::FundA(AMOUNT_A),
            Op::FundB(AMOUNT_B + b),
            Op::DonorDustA(d),
            Op::CloseRefundAtaA,
        ]),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (1..=AMOUNT_A + 10).prop_map(Op::FundA),
        3 => (1..=AMOUNT_B + 10).prop_map(Op::FundB),
        1 => Just(Op::FundA(AMOUNT_A)),
        1 => Just(Op::FundB(AMOUNT_B)),
        2 => Just(Op::ReclaimA),
        2 => Just(Op::ReclaimB),
        4 => Just(Op::Settle),
        1 => Just(Op::Cancel),
        1 => Just(Op::RejectA),
        1 => Just(Op::RejectB),
        1 => Just(Op::SettleByUserA),
        1 => Just(Op::CancelByUserB),
        1 => Just(Op::RejectByAuthority),
        1 => Just(Op::ReclaimByAuthority),
        3 => (1..=DONOR_BALANCE / 4).prop_map(Op::DonorDustA),
        3 => Just(Op::CloseRefundAtaA),
        2 => Just(Op::RecreateAtaA),
        2 => prop_oneof![0i64..600, 3000i64..4000].prop_map(Op::Advance),
        1 => Just(Op::Advance(3601)), // straight past the 1 h expiry
    ]
}

fn case() -> impl Strategy<Value = (LegKind, LegKind, Vec<Op>)> {
    (
        leg_kind(),
        leg_kind(),
        prefix(),
        prop::collection::vec(op(), 1..12),
    )
        .prop_map(|(ka, kb, mut p, t)| {
            p.extend(t);
            (ka, kb, p)
        })
}

#[derive(Clone, Debug, PartialEq)]
struct Model {
    open: bool,
    t: i64, // seconds since create
    expiry: i64,
    esc_a: u64,
    esc_b: u64,
    ua_a: u64, // user_a's mint-A ATA (refund destination of leg A)
    ub_a: u64, // user_b's mint-A ATA (settlement destination)
    ub_b: u64,
    ua_b: u64,
    donor_a: u64,
    side_a: u64,
    ua_a_exists: bool,
}

impl Model {
    /// Refund/Reclaim of leg A needs user_a's ATA unless escrow A is empty.
    fn a_refundable(&self) -> bool {
        self.esc_a == 0 || self.ua_a_exists
    }

    fn refund_and_close(&mut self) {
        self.ua_a += self.esc_a;
        self.ub_b += self.esc_b;
        self.esc_a = 0;
        self.esc_b = 0;
        self.open = false;
    }

    /// Apply `op`; return whether the program is expected to accept it
    /// (None for clock moves, which are not transactions).
    fn step(&mut self, op: &Op) -> Option<bool> {
        Some(match *op {
            Op::Advance(s) => {
                self.t += s;
                return None;
            }
            Op::FundA(x) => {
                let ok = self.open && self.ua_a_exists && x <= self.ua_a;
                if ok {
                    self.ua_a -= x;
                    self.esc_a += x;
                }
                ok
            }
            Op::FundB(x) => {
                let ok = self.open && x <= self.ub_b;
                if ok {
                    self.ub_b -= x;
                    self.esc_b += x;
                }
                ok
            }
            Op::DonorDustA(x) => {
                let ok = self.open && x <= self.donor_a;
                if ok {
                    self.donor_a -= x;
                    self.esc_a += x;
                }
                ok
            }
            Op::CloseRefundAtaA => {
                let ok = self.ua_a_exists;
                if ok {
                    self.side_a += self.ua_a;
                    self.ua_a = 0;
                    self.ua_a_exists = false;
                }
                ok
            }
            Op::RecreateAtaA => {
                self.ua_a_exists = true;
                true
            }
            Op::ReclaimA => {
                let ok = self.open && self.a_refundable();
                if ok {
                    self.ua_a += self.esc_a;
                    self.esc_a = 0;
                }
                ok
            }
            Op::ReclaimB => {
                if self.open {
                    self.ub_b += self.esc_b;
                    self.esc_b = 0;
                }
                self.open
            }
            Op::Settle => {
                let ok = self.open
                    && self.t <= self.expiry
                    && self.esc_a >= AMOUNT_A
                    && self.esc_b >= AMOUNT_B
                    && (self.esc_a == AMOUNT_A || self.ua_a_exists);
                if ok {
                    self.ub_a += AMOUNT_A;
                    self.ua_a += self.esc_a - AMOUNT_A;
                    self.ua_b += AMOUNT_B;
                    self.ub_b += self.esc_b - AMOUNT_B;
                    self.esc_a = 0;
                    self.esc_b = 0;
                    self.open = false;
                }
                ok
            }
            Op::Cancel | Op::RejectA | Op::RejectB => {
                let ok = self.open && self.a_refundable();
                if ok {
                    self.refund_and_close();
                }
                ok
            }
            Op::SettleByUserA
            | Op::CancelByUserB
            | Op::RejectByAuthority
            | Op::ReclaimByAuthority => false,
        })
    }
}

/// Record which interesting situation this step exercised (before `step`).
fn record(m: &Model, op: &Op, hook_a: bool, hook_b: bool) {
    let settle_ok = m.open && m.t <= m.expiry && m.esc_a >= AMOUNT_A && m.esc_b >= AMOUNT_B;
    match op {
        Op::Settle if m.open && m.t > m.expiry => COV.hit("settle_after_expiry"),
        Op::Settle if m.open && (m.esc_a < AMOUNT_A || m.esc_b < AMOUNT_B) => {
            COV.hit("settle_underfunded")
        }
        Op::Settle if settle_ok && m.esc_a > AMOUNT_A && !m.ua_a_exists => {
            COV.hit("L3_settle_blocked_no_refund_ata")
        }
        Op::Settle if settle_ok && (m.esc_a > AMOUNT_A || m.esc_b > AMOUNT_B) => {
            COV.hit("settle_ok_with_surplus");
            if hook_a || hook_b {
                COV.hit("settle_ok_surplus_hook");
            }
        }
        Op::Settle if settle_ok => {
            COV.hit("settle_ok_exact");
            if hook_a && hook_b {
                COV.hit("settle_ok_hook_both");
            }
        }
        Op::Cancel | Op::RejectA | Op::RejectB if m.open && !m.a_refundable() => {
            COV.hit("L3_refund_blocked_no_ata")
        }
        Op::Cancel | Op::RejectA | Op::RejectB if m.open && (hook_a || hook_b) => {
            COV.hit("terminal_refund_hook")
        }
        Op::Cancel | Op::RejectA | Op::RejectB if m.open => COV.hit("terminal_refund"),
        Op::ReclaimA | Op::ReclaimB if m.open && (m.esc_a > 0 || m.esc_b > 0) => {
            COV.hit("reclaim_nonempty")
        }
        Op::DonorDustA(_) if m.open => COV.hit("dust_donated"),
        Op::RecreateAtaA if !m.ua_a_exists && m.open && m.esc_a > 0 => {
            COV.hit("L3_recovered_by_recreate")
        }
        Op::SettleByUserA | Op::CancelByUserB | Op::RejectByAuthority | Op::ReclaimByAuthority => {
            COV.hit("wrong_signer")
        }
        _ => {}
    }
}

fn exec(ctx: &mut TestContext, f: &DvpFixture, x: &Extras, aux: &Aux, op: &Op) -> Option<bool> {
    let auth: &Keypair = &f.settlement_authority;
    let r = match *op {
        Op::Advance(s) => {
            ctx.advance_clock(s);
            return None;
        }
        Op::FundA(n) => fund(ctx, f, x, true, n),
        Op::FundB(n) => fund(ctx, f, x, false, n),
        Op::DonorDustA(n) => transfer(
            ctx,
            f,
            x,
            true,
            &aux.donor,
            &aux.donor_ata_a,
            &f.dvp_ata_a,
            n,
        ),
        Op::CloseRefundAtaA => {
            // Two transactions: empty the ATA into the side wallet, then close it.
            let have = get_token_balance(ctx, &f.user_a_ata_a);
            let moved = have == 0
                || transfer(
                    ctx,
                    f,
                    x,
                    true,
                    &f.user_a,
                    &f.user_a_ata_a,
                    &aux.side_ata_a,
                    have,
                )
                .is_ok();
            let closed =
                moved && close_token_account(ctx, f, true, &f.user_a, &f.user_a_ata_a).is_ok();
            return Some(closed);
        }
        Op::RecreateAtaA => {
            let ix = create_associated_token_account_idempotent(
                &ctx.payer.pubkey(),
                &f.user_a.pubkey(),
                &f.mint_a,
                &f.token_program_a,
            );
            ctx.send(ix, &[])
        }
        Op::ReclaimA => reclaim(ctx, f, x, &f.user_a, true),
        Op::ReclaimB => reclaim(ctx, f, x, &f.user_b, false),
        Op::Settle => settle(ctx, f, x, auth),
        Op::Cancel => cancel(ctx, f, x, auth),
        Op::RejectA => reject(ctx, f, x, &f.user_a),
        Op::RejectB => reject(ctx, f, x, &f.user_b),
        Op::SettleByUserA => settle(ctx, f, x, &f.user_a),
        Op::CancelByUserB => cancel(ctx, f, x, &f.user_b),
        Op::RejectByAuthority => reject(ctx, f, x, auth),
        Op::ReclaimByAuthority => reclaim(ctx, f, x, auth, true),
    };
    Some(r.is_ok())
}

fn observe(ctx: &TestContext, f: &DvpFixture, aux: &Aux, m: &Model) -> Model {
    Model {
        open: m.open, // checked separately via account closure
        t: m.t,
        expiry: m.expiry,
        esc_a: get_token_balance(ctx, &f.dvp_ata_a),
        esc_b: get_token_balance(ctx, &f.dvp_ata_b),
        ua_a: get_token_balance(ctx, &f.user_a_ata_a),
        ub_a: get_token_balance(ctx, &f.user_b_ata_a),
        ub_b: get_token_balance(ctx, &f.user_b_ata_b),
        ua_b: get_token_balance(ctx, &f.user_a_ata_b),
        donor_a: get_token_balance(ctx, &aux.donor_ata_a),
        side_a: get_token_balance(ctx, &aux.side_ata_a),
        ua_a_exists: !is_closed(ctx, &f.user_a_ata_a),
    }
}

fn run_case(ka: LegKind, kb: LegKind, ops: &[Op]) -> Result<(), TestCaseError> {
    let mut ctx = TestContext::new();
    let (f, x, aux) = setup_legs(&mut ctx, 0, ka, kb);
    let expiry_offset = f.expiry - ctx.now();
    assert_create_dvp(&mut ctx, &f);
    COV.hit(match (ka, kb) {
        (LegKind::T22Hook, _) | (_, LegKind::T22Hook) => "case_with_hook",
        (LegKind::Legacy, LegKind::Legacy) => "case_legacy_only",
        _ => "case_t22_no_hook",
    });

    let mut m = Model {
        open: true,
        t: 0,
        expiry: expiry_offset,
        esc_a: 0,
        esc_b: 0,
        ua_a: INITIAL_BALANCE,
        ub_a: 0,
        ub_b: INITIAL_BALANCE,
        ua_b: 0,
        donor_a: DONOR_BALANCE,
        side_a: 0,
        ua_a_exists: true,
    };
    let total_a = INITIAL_BALANCE + DONOR_BALANCE;
    let (hook_a, hook_b) = (ka == LegKind::T22Hook, kb == LegKind::T22Hook);

    for (i, op) in ops.iter().enumerate() {
        record(&m, op, hook_a, hook_b);
        let expected = m.step(op);
        let got = exec(&mut ctx, &f, &x, &aux, op);
        prop_assert_eq!(
            got,
            expected,
            "{:?}/{:?} step {} {:?}: accept/reject differs from model",
            ka,
            kb,
            i,
            op
        );

        let seen = observe(&ctx, &f, &aux, &m);
        prop_assert_eq!(
            &seen,
            &m,
            "{:?}/{:?} step {} {:?}: state differs from model",
            ka,
            kb,
            i,
            op
        );

        // Conservation per mint, independent of the model.
        prop_assert_eq!(
            seen.ua_a + seen.ub_a + seen.esc_a + seen.donor_a + seen.side_a,
            total_a,
            "mint A total changed at step {}",
            i
        );
        prop_assert_eq!(
            seen.ub_b + seen.ua_b + seen.esc_b,
            INITIAL_BALANCE,
            "mint B total changed at step {}",
            i
        );

        if !m.open {
            prop_assert!(
                is_closed(&ctx, &f.swap_dvp),
                "SwapDvp not closed after terminal op (step {})",
                i
            );
            prop_assert!(
                is_closed(&ctx, &f.dvp_ata_a),
                "escrow A not closed (step {})",
                i
            );
            prop_assert!(
                is_closed(&ctx, &f.dvp_ata_b),
                "escrow B not closed (step {})",
                i
            );
        }
    }
    Ok(())
}

#[test]
fn prop_lifecycle_matches_model() {
    let mut runner = super::coverage::seeded_runner();
    if let Err(e) = runner.run(&case(), |(ka, kb, ops)| run_case(ka, kb, &ops)) {
        panic!("{e}");
    }
    // Minimums are sized for 64 cases and scale with PROPTEST_CASES.
    let k = (runner.config().cases as usize / 64).max(1);
    COV.report_and_check(&[
        ("case_with_hook", 15 * k),
        ("case_legacy_only", 2 * k),
        ("case_t22_no_hook", 5 * k),
        ("settle_ok_exact", 2 * k),
        ("settle_ok_with_surplus", 3 * k),
        ("settle_ok_surplus_hook", 2 * k),
        ("settle_underfunded", 3 * k),
        ("settle_after_expiry", k),
        ("terminal_refund", 2 * k),
        ("terminal_refund_hook", 2 * k),
        ("reclaim_nonempty", 3 * k),
        ("dust_donated", 3 * k),
        ("L3_refund_blocked_no_ata", 3 * k),
        ("L3_settle_blocked_no_refund_ata", 2 * k),
        ("wrong_signer", 5 * k),
    ]);
}
