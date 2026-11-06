//! Mints with an extension the program's Token-2022 parser must know.
//!
//! `PermissionedBurn` (type 28) is supported by mainnet Token-2022. Placed
//! before a blocked extension, it must not hide that extension from the
//! Create check, and on its own it must not block a legitimate trade.

use solana_address::Address;
use solana_instruction::Instruction;
use solana_signer::Signer;
use spl_token_2022_interface::{
    extension::{permissioned_burn, transfer_fee, ExtensionType},
    instruction::initialize_non_transferable_mint,
};

use super::build_create_dvp_ix;
use crate::{
    state_utils::{
        assert_create_dvp, assert_fund_a, assert_fund_b, assert_settle_dvp,
        setup_dvp_with_programs, DvpFixture, AMOUNT_A, AMOUNT_B,
    },
    utils::{
        assert_instruction_error, assert_program_error, get_token_balance, token_2022_mint_account,
        TestContext, BLOCKED_MINT_EXTENSION, MINT_2022_FIRST_TLV_OFFSET, TOKEN_2022_PROGRAM_ID,
    },
};

/// Extension written after `PermissionedBurn` in the mint's TLV list.
#[derive(Clone, Copy, Debug)]
enum Next {
    None,
    TransferFee,
    NonTransferable,
}

fn setup(context: &mut TestContext) -> DvpFixture {
    setup_dvp_with_programs(context, 0, TOKEN_2022_PROGRAM_ID, TOKEN_2022_PROGRAM_ID)
}

/// Replaces `mint` with a runtime-built mint whose first TLV entry is
/// `PermissionedBurn`, followed by `next`.
fn set_permissioned_burn_mint(context: &mut TestContext, mint: &Address, next: Next) {
    let authority = context.payer.pubkey();
    let mut extensions = vec![ExtensionType::PermissionedBurn];
    match next {
        Next::None => {}
        Next::TransferFee => extensions.push(ExtensionType::TransferFeeConfig),
        Next::NonTransferable => extensions.push(ExtensionType::NonTransferable),
    }
    let account = token_2022_mint_account(context, &extensions, |mint| {
        let mut init: Vec<Instruction> = vec![permissioned_burn::instruction::initialize(
            &TOKEN_2022_PROGRAM_ID,
            mint,
            &authority,
        )
        .unwrap()];
        match next {
            Next::None => {}
            Next::TransferFee => init.push(
                transfer_fee::instruction::initialize_transfer_fee_config(
                    &TOKEN_2022_PROGRAM_ID,
                    mint,
                    Some(&authority),
                    Some(&authority),
                    100,
                    10_000,
                )
                .unwrap(),
            ),
            Next::NonTransferable => {
                init.push(initialize_non_transferable_mint(&TOKEN_2022_PROGRAM_ID, mint).unwrap())
            }
        }
        init
    });
    let first_type = &account.data[MINT_2022_FIRST_TLV_OFFSET..MINT_2022_FIRST_TLV_OFFSET + 2];
    assert_eq!(
        first_type,
        (ExtensionType::PermissionedBurn as u16).to_le_bytes()
    );
    context.svm.set_account(*mint, account).unwrap();
}

#[test]
fn create_rejects_blocked_extension_after_permissioned_burn() {
    for next in [Next::TransferFee, Next::NonTransferable] {
        for on_a in [true, false] {
            let mut context = TestContext::new();
            let fixture = setup(&mut context);
            let mint = if on_a { fixture.mint_a } else { fixture.mint_b };
            set_permissioned_burn_mint(&mut context, &mint, next);

            let ix = build_create_dvp_ix(&context, &fixture);
            assert_program_error(context.send(ix, &[]), BLOCKED_MINT_EXTENSION);
        }
    }
}

/// An extension type the parser doesn't know must reject the mint before any
/// CPI, rather than hide the entries after it.
#[test]
fn create_rejects_unknown_extension_type() {
    for on_a in [true, false] {
        let mut context = TestContext::new();
        let fixture = setup(&mut context);
        let mint = if on_a { fixture.mint_a } else { fixture.mint_b };
        set_permissioned_burn_mint(&mut context, &mint, Next::TransferFee);
        let mut account = context.get_account(&mint).unwrap();
        account.data[MINT_2022_FIRST_TLV_OFFSET..MINT_2022_FIRST_TLV_OFFSET + 2]
            .copy_from_slice(&u16::MAX.to_le_bytes());
        context.svm.set_account(mint, account).unwrap();

        let ix = build_create_dvp_ix(&context, &fixture);
        let result = context.send(ix, &[]);
        let logs = result.as_ref().err().cloned().unwrap_or_default();
        assert_instruction_error(result, "InvalidAccountData");
        assert!(!logs.contains("invoke [2]"), "{logs}");
    }
}

#[test]
fn permissioned_burn_mints_settle() {
    let mut context = TestContext::new();
    let fixture = setup(&mut context);
    for mint in [fixture.mint_a, fixture.mint_b] {
        set_permissioned_burn_mint(&mut context, &mint, Next::None);
    }

    assert_create_dvp(&mut context, &fixture);
    assert_fund_a(&mut context, &fixture);
    assert_fund_b(&mut context, &fixture);
    assert_settle_dvp(&mut context, &fixture);

    assert_eq!(get_token_balance(&context, &fixture.user_a_ata_b), AMOUNT_B);
    assert_eq!(get_token_balance(&context, &fixture.user_b_ata_a), AMOUNT_A);
}
