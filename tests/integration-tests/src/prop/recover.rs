//! After the DvP closes: RecoverDvp, late deposits and the nonce tombstone,
//! over every leg configuration (legacy / Token-2022 / Token-2022 + hook).
//!
//! Phase 1: fund each leg (nothing / partial / exact / surplus); one Recover
//! attempt while the DvP is open must fail (DvpStillOpen); close it with
//! Settle if both legs are funded, otherwise Cancel.
//! Phase 2: random post-close operations — anyone recreates an escrow ATA of
//! the dead PDA, parties and a third party make late deposits into it, the
//! depositors recover (repeatedly), wrong signers try, CreateDvp is retried
//! with the same nonce (must fail forever), and a sibling DvP with the same
//! parties and a new nonce is created and funded (must be independent).
//!
//! Oracle after every step: accept/reject as the model says; every balance
//! exact; per-mint totals conserved over all holders, including the sibling.

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use solana_keypair::Keypair;
use solana_signer::Signer;

use super::coverage::Coverage;
use super::helpers::{
    cancel, create, failed_with, fund, is_closed, recover, recreate_escrow, settle, sibling,
    transfer, Extras,
};
use super::legs::{leg_kind, setup_legs, Aux, LegKind, DONOR_BALANCE};
use crate::state_utils::{assert_create_dvp, DvpFixture, AMOUNT_A, AMOUNT_B, INITIAL_BALANCE};
use crate::utils::{get_token_balance, TestContext, DVP_STILL_OPEN};

static COV: Coverage = Coverage::new("recover");

#[derive(Clone, Copy, Debug)]
enum Funding {
    None,
    Partial,
    Exact,
    Surplus,
}

fn funding() -> impl Strategy<Value = Funding> {
    prop_oneof![
        1 => Just(Funding::None),
        1 => Just(Funding::Partial),
        3 => Just(Funding::Exact),
        2 => Just(Funding::Surplus),
    ]
}

fn amount(f: Funding, target: u64) -> u64 {
    match f {
        Funding::None => 0,
        Funding::Partial => target / 2,
        Funding::Exact => target,
        Funding::Surplus => target + 17,
    }
}

#[derive(Clone, Debug)]
enum Op {
    RecreateEscrowA,
    RecreateEscrowB,
    LateDepositA(u64),
    LateDepositB(u64),
    DonorLateA(u64),
    RecoverA,
    RecoverB,
    /// user_b asks for leg A's escrow; authority and a stranger try too.
    RecoverAByUserB,
    RecoverAByAuthority,
    RecoverBByStranger,
    CreateSameNonce,
    CreateSibling,
    FundSiblingA(u64),
    CancelSibling,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::RecreateEscrowA),
        3 => Just(Op::RecreateEscrowB),
        3 => (1u64..=500).prop_map(Op::LateDepositA),
        3 => (1u64..=500).prop_map(Op::LateDepositB),
        2 => (1u64..=DONOR_BALANCE / 4).prop_map(Op::DonorLateA),
        4 => Just(Op::RecoverA),
        4 => Just(Op::RecoverB),
        1 => Just(Op::RecoverAByUserB),
        1 => Just(Op::RecoverAByAuthority),
        1 => Just(Op::RecoverBByStranger),
        1 => Just(Op::CreateSameNonce),
        2 => Just(Op::CreateSibling),
        2 => (1u64..=AMOUNT_A).prop_map(Op::FundSiblingA),
        2 => Just(Op::CancelSibling),
    ]
}

/// Half of the time phase 2 starts by opening and funding the sibling DvP,
/// so "sibling cancelled with funds" is actually reached.
fn post_close() -> impl Strategy<Value = Vec<Op>> {
    (
        prop_oneof![Just(false), Just(true)],
        1u64..=AMOUNT_A,
        prop::collection::vec(op(), 1..16),
    )
        .prop_map(|(sib_first, n, tail)| {
            let mut v = if sib_first {
                vec![Op::CreateSibling, Op::FundSiblingA(n)]
            } else {
                vec![]
            };
            v.extend(tail);
            v
        })
}

fn case() -> impl Strategy<Value = (LegKind, LegKind, Funding, Funding, Vec<Op>)> {
    (leg_kind(), leg_kind(), funding(), funding(), post_close())
}

#[derive(Clone, Debug, PartialEq, Default)]
struct Model {
    ua_a: u64,
    ub_a: u64,
    ub_b: u64,
    ua_b: u64,
    donor_a: u64,
    esc_a: u64,
    esc_b: u64,
    esc_a_exists: bool,
    esc_b_exists: bool,
    sib_created: bool,
    sib_open: bool,
    sib_esc_a: u64,
}

impl Model {
    fn step(&mut self, op: &Op) -> bool {
        match *op {
            Op::RecreateEscrowA => {
                self.esc_a_exists = true;
                true
            }
            Op::RecreateEscrowB => {
                self.esc_b_exists = true;
                true
            }
            Op::LateDepositA(x) => {
                let ok = self.esc_a_exists && x <= self.ua_a;
                if ok {
                    self.ua_a -= x;
                    self.esc_a += x;
                }
                ok
            }
            Op::LateDepositB(x) => {
                let ok = self.esc_b_exists && x <= self.ub_b;
                if ok {
                    self.ub_b -= x;
                    self.esc_b += x;
                }
                ok
            }
            Op::DonorLateA(x) => {
                let ok = self.esc_a_exists && x <= self.donor_a;
                if ok {
                    self.donor_a -= x;
                    self.esc_a += x;
                }
                ok
            }
            Op::RecoverA => {
                let ok = self.esc_a_exists;
                if ok {
                    self.ua_a += self.esc_a;
                    self.esc_a = 0;
                    self.esc_a_exists = false;
                }
                ok
            }
            Op::RecoverB => {
                let ok = self.esc_b_exists;
                if ok {
                    self.ub_b += self.esc_b;
                    self.esc_b = 0;
                    self.esc_b_exists = false;
                }
                ok
            }
            Op::RecoverAByUserB | Op::RecoverAByAuthority | Op::RecoverBByStranger => false,
            Op::CreateSameNonce => false,
            Op::CreateSibling => {
                let ok = !self.sib_created;
                if ok {
                    self.sib_created = true;
                    self.sib_open = true;
                }
                ok
            }
            Op::FundSiblingA(x) => {
                let ok = self.sib_open && x <= self.ua_a;
                if ok {
                    self.ua_a -= x;
                    self.sib_esc_a += x;
                }
                ok
            }
            Op::CancelSibling => {
                let ok = self.sib_open;
                if ok {
                    self.ua_a += self.sib_esc_a;
                    self.sib_esc_a = 0;
                    self.sib_open = false;
                }
                ok
            }
        }
    }
}

fn record(m: &Model, op: &Op, hook: bool) {
    match op {
        Op::RecoverA | Op::RecoverB => {
            let (exists, amt) = if matches!(op, Op::RecoverA) {
                (m.esc_a_exists, m.esc_a)
            } else {
                (m.esc_b_exists, m.esc_b)
            };
            COV.hit(match (exists, amt > 0) {
                (false, _) => "recover_no_escrow_rejected",
                (true, true) => "recover_nonempty",
                (true, false) => "recover_empty_closes",
            });
            if exists && amt > 0 && hook {
                COV.hit("recover_nonempty_hook");
            }
        }
        Op::LateDepositA(_) | Op::LateDepositB(_) if m.esc_a_exists || m.esc_b_exists => {
            COV.hit("late_deposit")
        }
        Op::DonorLateA(_) if m.esc_a_exists => COV.hit("donor_late_deposit"),
        Op::RecoverAByUserB | Op::RecoverAByAuthority | Op::RecoverBByStranger => {
            COV.hit("recover_wrong_signer")
        }
        Op::CreateSameNonce => COV.hit("create_same_nonce_rejected"),
        Op::FundSiblingA(_) if m.sib_open => COV.hit("sibling_funded"),
        Op::CancelSibling if m.sib_open && m.sib_esc_a > 0 => {
            COV.hit("sibling_cancelled_with_funds")
        }
        _ => {}
    }
}

fn exec(
    ctx: &mut TestContext,
    f: &DvpFixture,
    sib: &DvpFixture,
    x: &Extras,
    aux: &Aux,
    op: &Op,
) -> bool {
    let stranger = Keypair::new();
    ctx.airdrop_if_required(&stranger.pubkey(), 1_000_000_000);
    match *op {
        Op::RecreateEscrowA => recreate_escrow(ctx, f, true),
        Op::RecreateEscrowB => recreate_escrow(ctx, f, false),
        Op::LateDepositA(n) => fund(ctx, f, x, true, n),
        Op::LateDepositB(n) => fund(ctx, f, x, false, n),
        Op::DonorLateA(n) => transfer(
            ctx,
            f,
            x,
            true,
            &aux.donor,
            &aux.donor_ata_a,
            &f.dvp_ata_a,
            n,
        ),
        Op::RecoverA => recover(ctx, f, x, &f.user_a, true),
        Op::RecoverB => recover(ctx, f, x, &f.user_b, false),
        Op::RecoverAByUserB => recover(ctx, f, x, &f.user_b, true),
        Op::RecoverAByAuthority => recover(ctx, f, x, &f.settlement_authority, true),
        Op::RecoverBByStranger => recover(ctx, f, x, &stranger, false),
        Op::CreateSameNonce => create(ctx, f, f.expiry, None),
        Op::CreateSibling => create(ctx, sib, sib.expiry, None),
        Op::FundSiblingA(n) => fund(ctx, sib, x, true, n),
        Op::CancelSibling => cancel(ctx, sib, x, &sib.settlement_authority),
    }
    .is_ok()
}

fn observe(ctx: &TestContext, f: &DvpFixture, sib: &DvpFixture, aux: &Aux, m: &Model) -> Model {
    Model {
        ua_a: get_token_balance(ctx, &f.user_a_ata_a),
        ub_a: get_token_balance(ctx, &f.user_b_ata_a),
        ub_b: get_token_balance(ctx, &f.user_b_ata_b),
        ua_b: get_token_balance(ctx, &f.user_a_ata_b),
        donor_a: get_token_balance(ctx, &aux.donor_ata_a),
        esc_a: get_token_balance(ctx, &f.dvp_ata_a),
        esc_b: get_token_balance(ctx, &f.dvp_ata_b),
        esc_a_exists: !is_closed(ctx, &f.dvp_ata_a),
        esc_b_exists: !is_closed(ctx, &f.dvp_ata_b),
        sib_created: m.sib_created,
        sib_open: !is_closed(ctx, &sib.swap_dvp),
        sib_esc_a: get_token_balance(ctx, &sib.dvp_ata_a),
    }
}

fn run_case(
    ka: LegKind,
    kb: LegKind,
    fa: Funding,
    fb: Funding,
    ops: &[Op],
) -> Result<(), TestCaseError> {
    let fail = |m: String| TestCaseError::fail(m);
    let mut ctx = TestContext::new();
    let (f, x, aux) = setup_legs(&mut ctx, 0, ka, kb);
    let sib = sibling(&f, 1);
    assert_create_dvp(&mut ctx, &f);

    // Phase 1.
    let (na, nb) = (amount(fa, AMOUNT_A), amount(fb, AMOUNT_B));
    if na > 0 {
        fund(&mut ctx, &f, &x, true, na).map_err(|e| fail(format!("fund A: {e}")))?;
    }
    if nb > 0 {
        fund(&mut ctx, &f, &x, false, nb).map_err(|e| fail(format!("fund B: {e}")))?;
    }
    let r = recover(&mut ctx, &f, &x, &f.user_a, true);
    prop_assert!(
        failed_with(&r, DVP_STILL_OPEN),
        "Recover while the DvP is open must fail with DvpStillOpen: {:?}",
        r.err()
    );
    COV.hit("recover_while_open_rejected");

    let mut m = Model {
        ua_a: INITIAL_BALANCE - na,
        ub_b: INITIAL_BALANCE - nb,
        donor_a: DONOR_BALANCE,
        ..Model::default()
    };
    if na >= AMOUNT_A && nb >= AMOUNT_B {
        settle(&mut ctx, &f, &x, &f.settlement_authority)
            .map_err(|e| fail(format!("Settle: {e}")))?;
        m.ub_a = AMOUNT_A;
        m.ua_a += na - AMOUNT_A;
        m.ua_b = AMOUNT_B;
        m.ub_b += nb - AMOUNT_B;
        COV.hit("closed_by_settle");
    } else {
        cancel(&mut ctx, &f, &x, &f.settlement_authority)
            .map_err(|e| fail(format!("Cancel: {e}")))?;
        m.ua_a += na;
        m.ub_b += nb;
        COV.hit("closed_by_cancel");
    }
    prop_assert!(is_closed(&ctx, &f.swap_dvp), "SwapDvp not closed");
    let seen = observe(&ctx, &f, &sib, &aux, &m);
    prop_assert_eq!(&seen, &m, "state after close differs from model");

    // Phase 2.
    let total_a = INITIAL_BALANCE + DONOR_BALANCE;
    let hook = ka == LegKind::T22Hook || kb == LegKind::T22Hook;
    for (i, op) in ops.iter().enumerate() {
        record(&m, op, hook);
        let expected = m.step(op);
        let got = exec(&mut ctx, &f, &sib, &x, &aux, op);
        prop_assert_eq!(
            got,
            expected,
            "{:?}/{:?} step {} {:?}: accept/reject differs from model",
            ka,
            kb,
            i,
            op
        );
        let seen = observe(&ctx, &f, &sib, &aux, &m);
        prop_assert_eq!(
            &seen,
            &m,
            "{:?}/{:?} step {} {:?}: state differs from model",
            ka,
            kb,
            i,
            op
        );
        prop_assert_eq!(
            seen.ua_a + seen.ub_a + seen.esc_a + seen.donor_a + seen.sib_esc_a,
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
        // The tombstone is permanent and the dead PDA never revives.
        prop_assert!(
            !is_closed(&ctx, &f.nonce_tombstone),
            "nonce tombstone disappeared at step {}",
            i
        );
        prop_assert!(
            is_closed(&ctx, &f.swap_dvp),
            "SwapDvp revived at step {}",
            i
        );
    }
    Ok(())
}

#[test]
fn prop_recover_and_nonce() {
    let mut runner = super::coverage::seeded_runner();
    if let Err(e) = runner.run(&case(), |(ka, kb, fa, fb, ops)| {
        run_case(ka, kb, fa, fb, &ops)
    }) {
        panic!("{e}");
    }
    let k = (runner.config().cases as usize / 64).max(1);
    COV.report_and_check(&[
        ("recover_while_open_rejected", 64 * k),
        ("closed_by_settle", 10 * k),
        ("closed_by_cancel", 10 * k),
        ("recover_nonempty", 5 * k),
        ("recover_nonempty_hook", 2 * k),
        ("recover_empty_closes", 3 * k),
        ("recover_no_escrow_rejected", 10 * k),
        ("late_deposit", 10 * k),
        ("donor_late_deposit", 3 * k),
        ("recover_wrong_signer", 10 * k),
        ("create_same_nonce_rejected", 5 * k),
        ("sibling_funded", 3 * k),
        ("sibling_cancelled_with_funds", 3 * k),
    ]);
}
