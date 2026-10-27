use dvp_swap_program_client::instructions::{
    CancelDvpBuilder, ReclaimDvpBuilder, RecoverDvpBuilder, RejectDvpBuilder, SettleDvpBuilder,
};
use solana_account::Account;
use solana_address::Address;
use solana_clock::Clock;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_program_pack::Pack;
use solana_signer::Signer;
use solana_transaction::Transaction;
use spl_token_2022_interface::state::Account as TokenAccount;
use spl_token_2022_interface::{
    error::TokenError,
    extension::{
        transfer_fee::{
            instruction::{harvest_withheld_tokens_to_mint, set_transfer_fee},
            TransferFeeAmount, TransferFeeConfig,
        },
        BaseStateWithExtensions, StateWithExtensions,
    },
    state::Mint,
};

use crate::{
    state_utils::{
        assert_cancel_dvp, assert_create_dvp, assert_fund_a, assert_fund_a_amount, assert_fund_b,
        assert_fund_b_amount, assert_settle_dvp, DvpFixture, AMOUNT_A, AMOUNT_B, INITIAL_BALANCE,
    },
    utils::{
        assert_program_error, create_ata, dvp_ata, get_token_balance, nonce_tombstone_pda,
        set_mint, set_mint_2022_with_transfer_fee, swap_dvp_pda, TestContext,
        BLOCKED_MINT_EXTENSION, MEMO_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
    },
};

/// Some(rate) creates a fee mint; None creates a legacy SPL mint. Initialize
/// extensions before creating ATAs so real TransferFeeAmount accounts are used.
fn setup(context: &mut TestContext, rates: [Option<u16>; 2]) -> DvpFixture {
    let user_a = Keypair::new();
    let user_b = Keypair::new();
    let settlement_authority = Keypair::new();
    for wallet in [&user_a, &user_b, &settlement_authority] {
        context.airdrop_if_required(&wallet.pubkey(), 1_000_000_000);
    }
    let mint_a = Address::new_unique();
    let mint_b = Address::new_unique();
    let programs = rates.map(|rate| {
        if rate.is_some() {
            TOKEN_2022_PROGRAM_ID
        } else {
            TOKEN_PROGRAM_ID
        }
    });
    for (i, mint) in [mint_a, mint_b].iter().enumerate() {
        if let Some(rate) = rates[i] {
            set_mint_2022_with_transfer_fee(
                context,
                mint,
                &settlement_authority.pubkey(),
                rate,
                10_000,
            );
        } else {
            set_mint(context, mint, &programs[i]);
        }
    }
    let user_a_ata_a = create_ata(context, &user_a.pubkey(), &mint_a, &programs[0]);
    let user_b_ata_b = create_ata(context, &user_b.pubkey(), &mint_b, &programs[1]);
    let user_a_ata_b = create_ata(context, &user_a.pubkey(), &mint_b, &programs[1]);
    let user_b_ata_a = create_ata(context, &user_b.pubkey(), &mint_a, &programs[0]);
    for ata in [user_a_ata_a, user_b_ata_b] {
        // Seed spendable balances without discarding the ATA's extensions.
        let mut account = context.get_account(&ata).unwrap();
        let mut base = TokenAccount::unpack(&account.data[..TokenAccount::LEN]).unwrap();
        base.amount = INITIAL_BALANCE;
        base.pack_into_slice(&mut account.data[..TokenAccount::LEN]);
        context.svm.set_account(ata, account).unwrap();
    }
    let nonce = 0;
    let (swap_dvp, _) = swap_dvp_pda(
        &settlement_authority.pubkey(),
        &user_a.pubkey(),
        &user_b.pubkey(),
        &mint_a,
        &mint_b,
        nonce,
    );
    DvpFixture {
        dvp_ata_a: dvp_ata(&swap_dvp, &mint_a, &programs[0]),
        dvp_ata_b: dvp_ata(&swap_dvp, &mint_b, &programs[1]),
        nonce_tombstone: nonce_tombstone_pda(&swap_dvp).0,
        user_a,
        user_b,
        settlement_authority,
        mint_a,
        mint_b,
        token_program_a: programs[0],
        token_program_b: programs[1],
        swap_dvp,
        user_a_ata_a,
        user_a_ata_b,
        user_b_ata_a,
        user_b_ata_b,
        expiry: context.now() + 3600,
        nonce,
    }
}

#[test]
fn zero_rate_settles_exact_amounts_and_refunds_surplus() {
    for rates in [[Some(0), Some(0)], [Some(0), None], [None, Some(0)]] {
        let mut context = TestContext::new();
        let fixture = setup(&mut context, rates);
        assert_create_dvp(&mut context, &fixture);
        assert_fund_a_amount(&mut context, &fixture, AMOUNT_A + 123);
        assert_fund_b_amount(&mut context, &fixture, AMOUNT_B + 456);
        assert_settle_dvp(&mut context, &fixture);
        assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
        assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
        assert_eq!(
            get_token_balance(&context, &fixture.user_a_ata_a),
            INITIAL_BALANCE - AMOUNT_A
        );
        assert_eq!(
            get_token_balance(&context, &fixture.user_b_ata_b),
            INITIAL_BALANCE - AMOUNT_B
        );
        for address in [fixture.swap_dvp, fixture.dvp_ata_a, fixture.dvp_ata_b] {
            assert!(context.get_account(&address).is_none());
        }
    }
}

// Change epochs independently of unix time to keep expiry out of the fee tests.
fn set_epoch(context: &mut TestContext, epoch: u64) {
    let mut clock = context.svm.get_sysvar::<Clock>();
    clock.epoch = epoch;
    context.svm.set_sysvar(&clock);
}

fn schedule_fee(
    context: &mut TestContext,
    fixture: &DvpFixture,
    mint: Address,
    rate: u16,
    cap: u64,
) {
    let ix = set_transfer_fee(
        &TOKEN_2022_PROGRAM_ID,
        &mint,
        &fixture.settlement_authority.pubkey(),
        &[],
        rate,
        cap,
    )
    .unwrap();
    context.send(ix, &[&fixture.settlement_authority]).unwrap();
}

fn fee_config(context: &TestContext, mint: Address) -> TransferFeeConfig {
    let account = context.get_account(&mint).unwrap();
    *StateWithExtensions::<Mint>::unpack(&account.data)
        .unwrap()
        .get_extension::<TransferFeeConfig>()
        .unwrap()
}

fn withheld(context: &TestContext, ata: Address) -> u64 {
    let account = context.get_account(&ata).unwrap();
    let state = StateWithExtensions::<TokenAccount>::unpack(&account.data).unwrap();
    state
        .get_extension::<TransferFeeAmount>()
        .unwrap()
        .withheld_amount
        .into()
}

fn snapshot(context: &TestContext, fixture: &DvpFixture) -> [Option<Account>; 7] {
    [
        fixture.swap_dvp,
        fixture.dvp_ata_a,
        fixture.dvp_ata_b,
        fixture.user_a_ata_a,
        fixture.user_a_ata_b,
        fixture.user_b_ata_a,
        fixture.user_b_ata_b,
    ]
    .map(|address| context.get_account(&address))
}

fn settle_ix(fixture: &DvpFixture) -> Instruction {
    SettleDvpBuilder::new()
        .settlement_authority(fixture.settlement_authority.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .dvp_ata_a(fixture.dvp_ata_a)
        .dvp_ata_b(fixture.dvp_ata_b)
        .user_a_destination_ata_b(fixture.user_a_ata_b)
        .user_b_destination_ata_a(fixture.user_b_ata_a)
        .user_a_ata_a(fixture.user_a_ata_a)
        .user_b_ata_b(fixture.user_b_ata_b)
        .token_program_a(fixture.token_program_a)
        .token_program_b(fixture.token_program_b)
        .memo_program(MEMO_PROGRAM_ID)
        .leg_a_extras_count(0)
        .instruction()
}

fn refund_ix(fixture: &DvpFixture, cancel: bool) -> Instruction {
    if cancel {
        CancelDvpBuilder::new()
            .settlement_authority(fixture.settlement_authority.pubkey())
            .swap_dvp(fixture.swap_dvp)
            .mint_a(fixture.mint_a)
            .mint_b(fixture.mint_b)
            .dvp_ata_a(fixture.dvp_ata_a)
            .dvp_ata_b(fixture.dvp_ata_b)
            .user_a_ata_a(fixture.user_a_ata_a)
            .user_b_ata_b(fixture.user_b_ata_b)
            .token_program_a(fixture.token_program_a)
            .token_program_b(fixture.token_program_b)
            .memo_program(MEMO_PROGRAM_ID)
            .leg_a_extras_count(0)
            .instruction()
    } else {
        RejectDvpBuilder::new()
            .signer(fixture.user_a.pubkey())
            .swap_dvp(fixture.swap_dvp)
            .mint_a(fixture.mint_a)
            .mint_b(fixture.mint_b)
            .dvp_ata_a(fixture.dvp_ata_a)
            .dvp_ata_b(fixture.dvp_ata_b)
            .user_a_ata_a(fixture.user_a_ata_a)
            .user_b_ata_b(fixture.user_b_ata_b)
            .token_program_a(fixture.token_program_a)
            .token_program_b(fixture.token_program_b)
            .memo_program(MEMO_PROGRAM_ID)
            .leg_a_extras_count(0)
            .instruction()
    }
}

fn reclaim_a_ix(fixture: &DvpFixture) -> Instruction {
    ReclaimDvpBuilder::new()
        .signer(fixture.user_a.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .mint(fixture.mint_a)
        .dvp_source_ata(fixture.dvp_ata_a)
        .signer_dest_ata(fixture.user_a_ata_a)
        .token_program(fixture.token_program_a)
        .memo_program(MEMO_PROGRAM_ID)
        .instruction()
}

fn recover_a_ix(fixture: &DvpFixture) -> Instruction {
    RecoverDvpBuilder::new()
        .signer(fixture.user_a.pubkey())
        .swap_dvp(fixture.swap_dvp)
        .nonce_tombstone(fixture.nonce_tombstone)
        .mint(fixture.mint_a)
        .dvp_escrow_ata(fixture.dvp_ata_a)
        .signer_dest_ata(fixture.user_a_ata_a)
        .token_program(fixture.token_program_a)
        .memo_program(MEMO_PROGRAM_ID)
        .settlement_authority(fixture.settlement_authority.pubkey())
        .user_a(fixture.user_a.pubkey())
        .user_b(fixture.user_b.pubkey())
        .mint_a(fixture.mint_a)
        .mint_b(fixture.mint_b)
        .nonce(fixture.nonce)
        .instruction()
}

/// Permissionless harvest is a separate instruction in the SAME transaction.
/// No mint/fee-authority signature is included unless it is the DvP signer.
fn harvest_then(
    context: &mut TestContext,
    fixture: &DvpFixture,
    ix: Instruction,
    signer: &Keypair,
) {
    let harvest = harvest_withheld_tokens_to_mint(
        &TOKEN_2022_PROGRAM_ID,
        &fixture.mint_a,
        &[&fixture.dvp_ata_a],
    )
    .unwrap();
    context.svm.expire_blockhash();
    let tx = Transaction::new_signed_with_payer(
        &[harvest, ix],
        Some(&context.payer.pubkey()),
        &[&context.payer, signer],
        context.svm.latest_blockhash(),
    );
    context.svm.send_transaction(tx).unwrap();
}

#[test]
fn create_checks_active_and_scheduled_rates_on_either_leg() {
    for on_a in [true, false] {
        for active in [0, 100] {
            for scheduled in [0, 100] {
                for epoch in [0, 2] {
                    let mut context = TestContext::new();
                    let rates = if on_a {
                        [Some(active), None]
                    } else {
                        [None, Some(active)]
                    };
                    let fixture = setup(&mut context, rates);
                    let mint = if on_a { fixture.mint_a } else { fixture.mint_b };
                    schedule_fee(&mut context, &fixture, mint, scheduled, 10_000);
                    assert_eq!(
                        u64::from(fee_config(&context, mint).newer_transfer_fee.epoch),
                        2
                    );
                    set_epoch(&mut context, epoch);
                    let ix = super::build_create_dvp_ix(&context, &fixture);
                    if scheduled == 0 && (epoch == 2 || active == 0) {
                        // At epoch 2, older_transfer_fee is historical, even if non-zero.
                        context.send(ix, &[]).unwrap();
                    } else {
                        assert_program_error(context.send(ix, &[]), BLOCKED_MINT_EXTENSION);
                        assert!(context.get_account(&fixture.swap_dvp).is_none());
                        assert!(context.get_account(&fixture.nonce_tombstone).is_none());
                    }
                }
            }
        }
    }
}

#[test]
fn nonzero_basis_points_are_rejected_even_with_zero_cap() {
    let mut context = TestContext::new();
    let fixture = setup(&mut context, [Some(0), None]);
    schedule_fee(&mut context, &fixture, fixture.mint_a, 100, 0);
    for epoch in [0, 2] {
        set_epoch(&mut context, epoch);
        assert_eq!(
            fee_config(&context, fixture.mint_a).calculate_epoch_fee(epoch, AMOUNT_A),
            Some(0)
        );
        let ix = super::build_create_dvp_ix(&context, &fixture);
        assert_program_error(context.send(ix, &[]), BLOCKED_MINT_EXTENSION);
    }
}

#[test]
fn settle_checks_current_epoch_and_rolls_back_at_activation() {
    for on_a in [true, false] {
        for epoch in [1, 2] {
            let mut context = TestContext::new();
            let fixture = setup(&mut context, [Some(0), Some(0)]);
            assert_create_dvp(&mut context, &fixture);
            assert_fund_a(&mut context, &fixture);
            assert_fund_b(&mut context, &fixture);
            let mint = if on_a { fixture.mint_a } else { fixture.mint_b };
            schedule_fee(&mut context, &fixture, mint, 100, 10_000);
            set_epoch(&mut context, epoch);
            if epoch == 1 {
                assert_settle_dvp(&mut context, &fixture);
                assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
                assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
            } else {
                let before = snapshot(&context, &fixture);
                assert_program_error(
                    context.send(settle_ix(&fixture), &[&fixture.settlement_authority]),
                    BLOCKED_MINT_EXTENSION,
                );
                assert_eq!(snapshot(&context, &fixture), before);
            }
        }
    }
}

#[test]
fn cancel_and_reject_refund_at_the_active_rate() {
    for cancel in [true, false] {
        for rate in [0, 100] {
            let mut context = TestContext::new();
            let fixture = setup(&mut context, [Some(0), Some(0)]);
            assert_create_dvp(&mut context, &fixture);
            assert_fund_a(&mut context, &fixture);
            assert_fund_b(&mut context, &fixture);
            for mint in [fixture.mint_a, fixture.mint_b] {
                schedule_fee(&mut context, &fixture, mint, rate, 10_000);
            }
            set_epoch(&mut context, 2);
            let signer = if cancel {
                &fixture.settlement_authority
            } else {
                &fixture.user_a
            };
            context
                .send(refund_ix(&fixture, cancel), &[signer])
                .unwrap();
            let fee_a = AMOUNT_A * u64::from(rate) / 10_000;
            let fee_b = AMOUNT_B * u64::from(rate) / 10_000;
            assert_eq!(
                get_token_balance(&context, &fixture.user_a_ata_a),
                INITIAL_BALANCE - fee_a
            );
            assert_eq!(
                get_token_balance(&context, &fixture.user_b_ata_b),
                INITIAL_BALANCE - fee_b
            );
            assert_eq!(withheld(&context, fixture.user_a_ata_a), fee_a);
            assert_eq!(withheld(&context, fixture.user_b_ata_b), fee_b);
            for address in [fixture.swap_dvp, fixture.dvp_ata_a, fixture.dvp_ata_b] {
                assert!(context.get_account(&address).is_none());
            }
        }
    }
}

#[test]
fn late_fee_deposit_requires_harvest_before_cancel_or_reject() {
    for cancel in [true, false] {
        let mut context = TestContext::new();
        let fixture = setup(&mut context, [Some(0), None]);
        assert_create_dvp(&mut context, &fixture);
        assert_fund_a(&mut context, &fixture);
        assert_fund_b(&mut context, &fixture);
        schedule_fee(&mut context, &fixture, fixture.mint_a, 100, 10_000);
        set_epoch(&mut context, 2);
        assert_fund_a_amount(&mut context, &fixture, 10_000);
        assert_eq!(withheld(&context, fixture.dvp_ata_a), 100);
        let signer = if cancel {
            &fixture.settlement_authority
        } else {
            &fixture.user_a
        };
        let before = snapshot(&context, &fixture);
        assert_program_error(
            context.send(refund_ix(&fixture, cancel), &[signer]),
            TokenError::AccountHasWithheldTransferFees as u32,
        );
        assert_eq!(snapshot(&context, &fixture), before);
        harvest_then(&mut context, &fixture, refund_ix(&fixture, cancel), signer);
        // Escrow returns 84,900 minus its 1% outgoing fee (849).
        assert_eq!(
            get_token_balance(&context, &fixture.user_a_ata_a),
            INITIAL_BALANCE - 100 - 849
        );
        assert_eq!(withheld(&context, fixture.user_a_ata_a), 849);
        assert_eq!(
            u64::from(fee_config(&context, fixture.mint_a).withheld_amount),
            100
        );
        assert_eq!(
            get_token_balance(&context, &fixture.user_b_ata_b),
            INITIAL_BALANCE
        );
        assert!(context.get_account(&fixture.dvp_ata_a).is_none());
        assert!(context.get_account(&fixture.swap_dvp).is_none());
    }
}

#[test]
fn settle_after_return_to_zero_harvests_historical_fees() {
    for precreated in [true, false] {
        let mut context = TestContext::new();
        let fixture = setup(&mut context, [Some(if precreated { 100 } else { 0 }), None]);
        if precreated {
            create_ata(
                &mut context,
                &fixture.swap_dvp,
                &fixture.mint_a,
                &TOKEN_2022_PROGRAM_ID,
            );
            assert_fund_a_amount(&mut context, &fixture, 10_000);
            schedule_fee(&mut context, &fixture, fixture.mint_a, 0, 10_000);
            set_epoch(&mut context, 2);
            assert_create_dvp(&mut context, &fixture);
            assert_fund_a_amount(&mut context, &fixture, AMOUNT_A - 9_900);
        } else {
            assert_create_dvp(&mut context, &fixture);
            assert_fund_a(&mut context, &fixture);
            schedule_fee(&mut context, &fixture, fixture.mint_a, 100, 10_000);
            set_epoch(&mut context, 2);
            assert_fund_a_amount(&mut context, &fixture, 10_000);
            schedule_fee(&mut context, &fixture, fixture.mint_a, 0, 10_000);
            // A future decrease to zero does not permit settlement at today's 1% rate.
            assert_program_error(
                context.send(settle_ix(&fixture), &[&fixture.settlement_authority]),
                BLOCKED_MINT_EXTENSION,
            );
            set_epoch(&mut context, 4);
        }
        assert_fund_b(&mut context, &fixture);
        assert_eq!(withheld(&context, fixture.dvp_ata_a), 100);
        let before = snapshot(&context, &fixture);
        assert_program_error(
            context.send(settle_ix(&fixture), &[&fixture.settlement_authority]),
            TokenError::AccountHasWithheldTransferFees as u32,
        );
        assert_eq!(snapshot(&context, &fixture), before);
        harvest_then(
            &mut context,
            &fixture,
            settle_ix(&fixture),
            &fixture.settlement_authority,
        );
        assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
        assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
        assert_eq!(
            get_token_balance(&context, &fixture.user_a_ata_a),
            INITIAL_BALANCE - AMOUNT_A - 100
        );
        assert_eq!(
            u64::from(fee_config(&context, fixture.mint_a).withheld_amount),
            100
        );
        assert!(context.get_account(&fixture.dvp_ata_a).is_none());
        assert!(context.get_account(&fixture.swap_dvp).is_none());
    }
}

#[test]
fn reclaim_drains_spendable_balance_without_closing_or_harvesting() {
    for rate in [0, 100] {
        let mut context = TestContext::new();
        let fixture = setup(&mut context, [Some(0), None]);
        assert_create_dvp(&mut context, &fixture);
        schedule_fee(&mut context, &fixture, fixture.mint_a, rate, 10_000);
        set_epoch(&mut context, 2);
        assert_fund_a_amount(&mut context, &fixture, 10_000);
        context
            .send(reclaim_a_ix(&fixture), &[&fixture.user_a])
            .unwrap();
        let (incoming_fee, outgoing_fee) = if rate == 0 { (0, 0) } else { (100, 99) };
        assert_eq!(get_token_balance(&context, &fixture.dvp_ata_a), 0);
        assert_eq!(withheld(&context, fixture.dvp_ata_a), incoming_fee);
        assert_eq!(
            get_token_balance(&context, &fixture.user_a_ata_a),
            INITIAL_BALANCE - incoming_fee - outgoing_fee
        );
        assert_eq!(withheld(&context, fixture.user_a_ata_a), outgoing_fee);
        assert!(context.get_account(&fixture.swap_dvp).is_some());
        harvest_then(
            &mut context,
            &fixture,
            refund_ix(&fixture, false),
            &fixture.user_a,
        );
        assert!(context.get_account(&fixture.dvp_ata_a).is_none());
        assert!(context.get_account(&fixture.swap_dvp).is_none());
    }
}

#[test]
fn recover_late_deposit_with_zero_or_nonzero_fee() {
    for rate in [0, 100] {
        let mut context = TestContext::new();
        let fixture = setup(&mut context, [Some(0), None]);
        assert_create_dvp(&mut context, &fixture);
        assert_cancel_dvp(&mut context, &fixture);
        schedule_fee(&mut context, &fixture, fixture.mint_a, rate, 10_000);
        set_epoch(&mut context, 2);
        create_ata(
            &mut context,
            &fixture.swap_dvp,
            &fixture.mint_a,
            &TOKEN_2022_PROGRAM_ID,
        );
        assert_fund_a_amount(&mut context, &fixture, 10_000);
        if rate == 0 {
            context
                .send(recover_a_ix(&fixture), &[&fixture.user_a])
                .unwrap();
            assert_eq!(
                get_token_balance(&context, &fixture.user_a_ata_a),
                INITIAL_BALANCE
            );
        } else {
            let before = snapshot(&context, &fixture);
            assert_program_error(
                context.send(recover_a_ix(&fixture), &[&fixture.user_a]),
                TokenError::AccountHasWithheldTransferFees as u32,
            );
            assert_eq!(snapshot(&context, &fixture), before);
            harvest_then(
                &mut context,
                &fixture,
                recover_a_ix(&fixture),
                &fixture.user_a,
            );
            assert_eq!(
                get_token_balance(&context, &fixture.user_a_ata_a),
                INITIAL_BALANCE - 100 - 99
            );
            assert_eq!(withheld(&context, fixture.user_a_ata_a), 99);
            assert_eq!(
                u64::from(fee_config(&context, fixture.mint_a).withheld_amount),
                100
            );
        }
        assert!(context.get_account(&fixture.dvp_ata_a).is_none());
        assert!(context.get_account(&fixture.swap_dvp).is_none());
    }
}
