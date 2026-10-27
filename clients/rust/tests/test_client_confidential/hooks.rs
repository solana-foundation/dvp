use crate::{
    confidential_utils::{pending, send_v1, state},
    state_utils::{AMOUNT_A, AMOUNT_B},
    utils::{
        execute, get_token_balance, ClientFixture, TestContext, HOOK_FIXTURE_PROGRAM_ID,
        MEMO_PROGRAM_ID, TOKEN_2022_PROGRAM_ID as TOKEN,
    },
};
use borsh::BorshDeserialize;
use dvp_swap_program_client::confidential::test_utils::{
    checked_available_balance, transfer_session, TransferAccounts, TransferRequest,
};
use dvp_swap_program_client::{
    confidential::*,
    instructions::{CancelConfidentialDvp, CancelConfidentialDvpInstructionArgs},
};
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_program_error::ProgramError;
use solana_signer::Signer;
use spl_tlv_account_resolution::{
    account::ExtraAccountMeta, seeds::Seed, state::ExtraAccountMetaList,
};
use spl_token_2022_interface::{
    extension::{
        confidential_transfer::instruction as ct, memo_transfer::instruction as memo, ExtensionType,
    },
    instruction as token,
};
use spl_transfer_hook_interface::{
    get_extra_account_metas_address, instruction::ExecuteInstruction, offchain::AccountDataResult,
};

// The protocol permits at most 32 extra accounts independently on each leg.
const HOOK_EXTRAS_LIMIT: usize = 32;

#[test]
fn client_hooked_settle_resolves_hidden_amount_and_emits_memos() {
    // An arbitrary nonzero surplus forces a second confidential transfer.
    const SURPLUS: u64 = 7;
    let mut context = TestContext::new();
    let f = ClientFixture::with_hook(
        &mut context,
        TransactionFormat::V1,
        AMOUNT_B + SURPLUS,
        true,
    );

    // Execute encodes its amount after the 8-byte discriminator; CT uses u64::MAX.
    let validation =
        get_extra_account_metas_address(&f.dvp.accounts.mint_b, &HOOK_FIXTURE_PROGRAM_ID);
    let entry = ExtraAccountMeta::new_with_seeds(
        &[Seed::InstructionData {
            index: 8,
            length: 8,
        }],
        false,
        false,
    )
    .unwrap();
    let data = hook_list(&[entry]);
    let mut account = context.get_account(&validation).unwrap();
    account.data = data;
    account.lamports = context
        .svm
        .minimum_balance_for_rent_exemption(account.data.len());
    context.svm.set_account(validation, account).unwrap();
    let hidden_pda = solana_address::Address::find_program_address(
        &[&u64::MAX.to_le_bytes()],
        &HOOK_FIXTURE_PROGRAM_ID,
    )
    .0;
    context.svm.airdrop(&hidden_pda, 1_000_000).unwrap();

    // Resolve hook accounts and require memos on both receiving accounts.
    let extras = futures::executor::block_on(resolve_confidential_hook_accounts(
        &HOOK_FIXTURE_PROGRAM_ID,
        &f.dvp.accounts.dvp_ata_b,
        &f.dvp.accounts.mint_b,
        &f.recipient,
        &f.dvp.accounts.swap_dvp,
        |key| {
            let data = context.get_account(&key).map(|account| account.data);
            std::future::ready(Ok(data))
        },
    ))
    .unwrap();
    assert_eq!(extras[0].pubkey, hidden_pda);
    assert!(extras.iter().all(|meta| !meta.is_signer));
    for (address, owner) in [(f.recipient, &f.dvp.user_a), (f.refund, &f.dvp.user_b)] {
        require_memo(&mut context, &f, address, owner);
    }

    // Execute Settle and check both memo and hook CPIs.
    let source = f.source(&context);
    let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
    let recipient = context.get_account(&f.recipient).unwrap();
    let refund = context.get_account(&f.refund).unwrap();
    let swap = f.swap(&context);
    let request = SettleRequest {
        source: TransferSource {
            state: &source,
            keys: &f.keys,
            history: &[],
        },
        swap: &swap,
        expected_amount_b: AMOUNT_B,
        recipient: crate::utils::recipient_account(f.recipient, &recipient),
        surplus_recipient: crate::utils::recipient_account(f.refund, &refund),
        expected_recipient: None,
        expected_surplus_recipient: None,
        mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
        expected_auditor: None,
    };
    let session = settle_session(&f.config, f.settle_accounts(), request, &[], &extras).unwrap();
    let history = execute(&mut context, &f.config, session, &[&f.dvp.authority]);
    let final_tx = history.last().unwrap();
    let inner = final_tx.inner_instructions.get(&0).unwrap();
    // Payment and surplus refund each require one memo and invoke the hook once.
    assert_eq!(
        inner
            .iter()
            .filter(|ix| ix.program_id == MEMO_PROGRAM_ID)
            .count(),
        2
    );
    assert_eq!(
        inner
            .iter()
            .filter(|ix| ix.program_id == HOOK_FIXTURE_PROGRAM_ID)
            .count(),
        2
    );

    // Both payment and surplus reach their recipients, and settlement closes the escrow.
    assert_eq!(get_token_balance(&context, &f.asset_recipient), AMOUNT_A);
    assert_eq!(pending(&context, &f.recipient, &f.recipient_keys), AMOUNT_B);
    assert_eq!(pending(&context, &f.refund, &f.buyer_keys), SURPLUS);
    assert!(context.get_account(&f.dvp.accounts.swap_dvp).is_none());
    assert!(context.get_account(&f.dvp.accounts.dvp_ata_b).is_none());
}

#[test]
fn client_hooked_funding_and_refund_in_both_formats() {
    for format in [TransactionFormat::V1, TransactionFormat::V0] {
        let mut context = TestContext::new();
        let f = ClientFixture::with_hook(&mut context, format, 0, true);

        // Make the payment available in the buyer wallet.
        send_v1(
            &mut context,
            &[
                token::mint_to(
                    &TOKEN,
                    &f.dvp.accounts.mint_b,
                    &f.refund,
                    &f.config.payer,
                    &[],
                    AMOUNT_B,
                )
                .unwrap(),
                ct::deposit(
                    &TOKEN,
                    &f.refund,
                    &f.dvp.accounts.mint_b,
                    AMOUNT_B,
                    6,
                    &f.dvp.user_b.pubkey(),
                    &[],
                )
                .unwrap(),
                ct::apply_pending_balance(
                    &TOKEN,
                    &f.refund,
                    1,
                    &f.buyer_keys.balance(AMOUNT_B),
                    &f.dvp.user_b.pubkey(),
                    &[],
                )
                .unwrap(),
            ],
            &[&f.dvp.user_b],
        )
        .unwrap();
        let wallet_keys = EscrowKeys {
            elgamal: f.buyer_keys.elgamal.clone(),
            ae: f.buyer_keys.ae.clone(),
            opening_lo: f.keys.opening_lo.clone(),
            opening_hi: f.keys.opening_hi.clone(),
        };
        let source = state(&context, &f.refund);
        // Fund escrow and verify that Token-2022 invokes the transfer hook.
        let destination = f.source(&context);
        let extras = crate::utils::hook_extras_for_mint(&f.dvp.accounts.mint_b);
        let session = transfer_session(
            &f.config,
            TransferAccounts {
                authority: f.dvp.user_b.pubkey(),
                source: f.refund,
                mint: f.dvp.accounts.mint_b,
                destination: f.dvp.accounts.dvp_ata_b,
            },
            TransferRequest {
                source: TransferSource {
                    state: &source,
                    keys: &wallet_keys,
                    history: &[],
                },
                recipient: &destination,
                amount: AMOUNT_B,
                auditor: None,
            },
            &extras,
        )
        .unwrap();
        let history = execute(&mut context, &f.config, session, &[&f.dvp.user_b]);
        assert_eq!(
            history
                .last()
                .unwrap()
                .inner_instructions
                .values()
                .flatten()
                .filter(|ix| ix.program_id == HOOK_FIXTURE_PROGRAM_ID)
                .count(),
            1
        );

        // Apply escrow funding and reclaim it through the hook after expiry.
        let source = f.source(&context);
        let session = apply_session(
            &f.config,
            &f.apply_accounts(f.dvp.user_b.pubkey()),
            f.apply_args(),
            TransferSource {
                state: &source,
                keys: &f.keys,
                history: &[],
            },
        )
        .unwrap();
        execute(&mut context, &f.config, session, &[&f.dvp.user_b]);
        // The fixture expires after 3600 seconds; move one second beyond expiry.
        context.advance_clock(3601);
        let source = f.source(&context);
        let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
        let recipient = context.get_account(&f.refund).unwrap();
        let session = refund_session(
            &f.config,
            RefundInstruction::Reclaim(super::refunds::reclaim_leg_b_accounts(&f)),
            Some(RefundRequest {
                source: TransferSource {
                    state: &source,
                    keys: &f.keys,
                    history: &[],
                },
                recipient: crate::utils::recipient_account(f.refund, &recipient),
                expected_recipient: None,
                amount: RefundAmount::Full,
                mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
                expected_auditor: None,
            }),
            &[],
            &extras,
        )
        .unwrap();
        let history = execute(&mut context, &f.config, session, &[&f.dvp.user_b]);
        assert_eq!(
            history
                .last()
                .unwrap()
                .inner_instructions
                .values()
                .flatten()
                .filter(|ix| ix.program_id == HOOK_FIXTURE_PROGRAM_ID)
                .count(),
            1
        );

        // The full payment returns to the buyer as one pending credit.
        assert_eq!(
            checked_available_balance(&f.source(&context), &f.keys).unwrap(),
            0
        );
        assert_eq!(pending(&context, &f.refund, &f.buyer_keys), AMOUNT_B);
        assert_eq!(
            u64::from(state(&context, &f.refund).pending_balance_credit_counter),
            1
        );
    }
}

#[test]
fn hook_extras_preserve_order_and_writable_but_remove_signers() {
    let leg_a = [AccountMeta::new(Address::new_unique(), true)];
    let leg_b = [AccountMeta::new_readonly(Address::new_unique(), true)];
    let ix = refund_with_hook_extras(&leg_a, &leg_b).unwrap();

    // Sanitization must preserve leg ordering, addresses, and writable privileges.
    assert_eq!(
        &ix.accounts[ix.accounts.len() - leg_a.len() - leg_b.len()..],
        &[
            AccountMeta::new(leg_a[0].pubkey, false),
            AccountMeta::new_readonly(leg_b[0].pubkey, false),
        ],
    );
    // The first byte is the generated instruction discriminator.
    let args = CancelConfidentialDvpInstructionArgs::try_from_slice(&ix.data[1..]).unwrap();
    assert_eq!(usize::from(args.leg_a_extras_count), leg_a.len());
}

#[test]
fn hook_extras_limit_applies_separately_to_each_leg() {
    let leg_a = hook_extras(HOOK_EXTRAS_LIMIT);
    // Share addresses across legs to stay below V1's separate limit of 64 unique addresses.
    let leg_b: Vec<_> = leg_a.iter().rev().cloned().collect();
    let ix = refund_with_hook_extras(&leg_a, &leg_b).unwrap();

    // 32 + 32 is valid: a combined limit of 32 would incorrectly reject this request.
    let expected: Vec<_> = leg_a.iter().chain(&leg_b).cloned().collect();
    assert_eq!(&ix.accounts[ix.accounts.len() - expected.len()..], expected);
    let args = CancelConfidentialDvpInstructionArgs::try_from_slice(&ix.data[1..]).unwrap();
    assert_eq!(usize::from(args.leg_a_extras_count), HOOK_EXTRAS_LIMIT);
}

#[test]
fn hook_resolver_preserves_writable_but_removes_signers() {
    let writable = Address::new_unique();
    let readonly = Address::new_unique();
    let data = hook_list(&[
        ExtraAccountMeta::new_with_pubkey(&writable, true, true).unwrap(),
        ExtraAccountMeta::new_with_pubkey(&readonly, true, false).unwrap(),
    ]);
    let extras = resolve_hook_accounts(|_| Ok(Some(data.clone()))).unwrap();

    assert_eq!(extras[0], AccountMeta::new(writable, false));
    assert_eq!(extras[1], AccountMeta::new_readonly(readonly, false));
    assert!(extras.iter().all(|meta| !meta.is_signer));
}

#[test]
fn hooked_settle_rejects_v0() {
    let mut context = TestContext::new();
    let mut f = ClientFixture::with_hook(&mut context, TransactionFormat::V1, 0, true);
    let source = f.source(&context);
    let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
    let recipient = context.get_account(&f.recipient).unwrap();
    let refund = context.get_account(&f.refund).unwrap();
    let swap = f.swap(&context);
    let extras = crate::utils::hook_extras_for_mint(&f.dvp.accounts.mint_b);
    f.config.format = TransactionFormat::V0;

    // A hook on either leg is enough to require V1; no funding or proofs are needed.
    for (leg_a, leg_b) in [
        (extras.as_slice(), &[][..]),
        (&[][..], extras.as_slice()),
        (extras.as_slice(), extras.as_slice()),
    ] {
        assert!(matches!(
            settle_session(
                &f.config,
                f.settle_accounts(),
                SettleRequest {
                    source: TransferSource {
                        state: &source,
                        keys: &f.keys,
                        history: &[]
                    },
                    swap: &swap,
                    expected_amount_b: AMOUNT_B,
                    recipient: crate::utils::recipient_account(f.recipient, &recipient),
                    surplus_recipient: crate::utils::recipient_account(f.refund, &refund),
                    expected_recipient: None,
                    expected_surplus_recipient: None,
                    mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
                    expected_auditor: None,
                },
                leg_a,
                leg_b,
            ),
            Err(ConfidentialError::HookedSettleRequiresV1)
        ));
    }
}

#[test]
fn hook_extras_reject_one_account_above_either_leg_limit() {
    let oversized = hook_extras(HOOK_EXTRAS_LIMIT + 1);
    for (leg_a, leg_b) in [
        (oversized.as_slice(), &[][..]),
        (&[][..], oversized.as_slice()),
    ] {
        assert!(matches!(
            refund_with_hook_extras(leg_a, leg_b),
            Err(ConfidentialError::Account("too many hook extras"))
        ));
    }
}

#[test]
fn hook_resolver_accepts_missing_validation_account() {
    assert_eq!(
        resolve_hook_accounts(|_| Ok(None)).unwrap(),
        vec![AccountMeta::new_readonly(HOOK_FIXTURE_PROGRAM_ID, false)]
    );
}

#[test]
fn hook_resolver_rejects_truncated_validation_data() {
    let mut data = hook_list(&[]);
    // A partial Execute discriminator cannot describe a valid TLV account list.
    data.truncate(1);
    assert!(matches!(
        resolve_hook_accounts(|_| Ok(Some(data.clone()))),
        Err(ConfidentialError::Transaction(message))
            if message == ProgramError::InvalidAccountData.to_string()
    ));
}

#[test]
fn hook_resolver_preserves_fetch_error() {
    const FETCH_ERROR: &str = "test hook account fetch failed";
    assert!(matches!(
        resolve_hook_accounts(|_| Err(std::io::Error::other(FETCH_ERROR).into())),
        Err(ConfidentialError::Transaction(message)) if message == FETCH_ERROR
    ));
}

fn require_memo(context: &mut TestContext, f: &ClientFixture, address: Address, owner: &Keypair) {
    send_v1(
        context,
        &[
            token::reallocate(
                &TOKEN,
                &address,
                &f.config.payer,
                &owner.pubkey(),
                &[],
                &[ExtensionType::MemoTransfer],
            )
            .unwrap(),
            memo::enable_required_transfer_memos(&TOKEN, &address, &owner.pubkey(), &[]).unwrap(),
        ],
        &[owner],
    )
    .unwrap();
}

fn hook_extras(count: usize) -> Vec<AccountMeta> {
    (0..count)
        .map(|_| AccountMeta::new_readonly(Address::new_unique(), false))
        .collect()
}

fn hook_list(entries: &[ExtraAccountMeta]) -> Vec<u8> {
    let mut data = vec![0; ExtraAccountMetaList::size_of(entries.len()).unwrap()];
    ExtraAccountMetaList::init::<ExecuteInstruction>(&mut data, entries).unwrap();
    data
}

fn resolve_hook_accounts(
    fetch: impl Fn(Address) -> AccountDataResult,
) -> Result<Vec<AccountMeta>, ConfidentialError> {
    futures::executor::block_on(resolve_confidential_hook_accounts(
        &HOOK_FIXTURE_PROGRAM_ID,
        &Address::new_unique(),
        &Address::new_unique(),
        &Address::new_unique(),
        &Address::new_unique(),
        |key| std::future::ready(fetch(key)),
    ))
}

fn refund_with_hook_extras(
    leg_a: &[AccountMeta],
    leg_b: &[AccountMeta],
) -> Result<Instruction, ConfidentialError> {
    // A no-proof Cancel exercises the public builder without accounts on a ledger.
    let config = SessionConfig::new(
        Address::new_unique(),
        TransactionFormat::V1,
        Default::default(),
    );
    let accounts = CancelConfidentialDvp {
        settlement_authority: config.payer,
        swap_dvp: Address::new_unique(),
        mint_a: Address::new_unique(),
        mint_b: Address::new_unique(),
        dvp_ata_a: Address::new_unique(),
        dvp_ata_b: Address::new_unique(),
        user_a_ata_a: Address::new_unique(),
        user_b_ata_b: Address::new_unique(),
        token_program_a: crate::utils::TOKEN_PROGRAM_ID,
        token_program_b: TOKEN,
        memo_program: MEMO_PROGRAM_ID,
        zk_elgamal_proof_program: solana_zk_elgamal_proof_interface::ID,
        equality_context: None,
        validity_context: None,
        range_context: None,
        zero_context: None,
    };
    let mut session = refund_session(
        &config,
        RefundInstruction::Cancel(accounts),
        None,
        leg_a,
        leg_b,
    )?;
    assert!(session.preparation.is_empty());
    assert_eq!(session.final_transaction.instructions.len(), 1);
    Ok(session.final_transaction.instructions.remove(0))
}

#[test]
fn hook_without_validation_account_settles() {
    settle_without_validation_list(false);
}

#[test]
fn hook_with_prefunded_validation_pda_settles() {
    // Anyone can fund the PDA, leaving a System-owned account with no data.
    settle_without_validation_list(true);
}

fn settle_without_validation_list(prefund: bool) {
    let mut context = TestContext::new();
    let f = ClientFixture::with_hook(&mut context, TransactionFormat::V1, AMOUNT_B, true);
    let hook = Address::new_unique();
    context
        .svm
        .add_program(
            hook,
            include_bytes!("../../../../target/deploy/transfer_hook_fixture.so"),
        )
        .unwrap();
    let validation = get_extra_account_metas_address(&f.dvp.accounts.mint_b, &hook);
    assert!(context.get_account(&validation).is_none());
    let update = spl_token_2022_interface::extension::transfer_hook::instruction::update(
        &TOKEN,
        &f.dvp.accounts.mint_b,
        &context.payer.pubkey(),
        &[],
        Some(hook),
    )
    .unwrap();
    send_v1(&mut context, &[update], &[]).unwrap();
    if prefund {
        let rent = context.svm.minimum_balance_for_rent_exemption(0);
        let fund = solana_system_interface::instruction::transfer(
            &context.payer.pubkey(),
            &validation,
            rent,
        );
        send_v1(&mut context, &[fund], &[]).unwrap();
        assert!(context.get_account(&validation).unwrap().data.is_empty());
    }
    let resolved = futures::executor::block_on(resolve_confidential_hook_accounts(
        &hook,
        &f.dvp.accounts.dvp_ata_b,
        &f.dvp.accounts.mint_b,
        &f.recipient,
        &f.dvp.accounts.swap_dvp,
        |key| std::future::ready(Ok(context.get_account(&key).map(|account| account.data))),
    ));
    let extras = resolved.unwrap();
    assert_eq!(extras, vec![AccountMeta::new_readonly(hook, false)]);
    let source = f.source(&context);
    let mint_b = context.get_account(&f.dvp.accounts.mint_b).unwrap();
    let recipient = context.get_account(&f.recipient).unwrap();
    let refund = context.get_account(&f.refund).unwrap();
    let swap = f.swap(&context);
    let session = settle_session(
        &f.config,
        f.settle_accounts(),
        SettleRequest {
            source: TransferSource {
                state: &source,
                keys: &f.keys,
                history: &[],
            },
            swap: &swap,
            expected_amount_b: AMOUNT_B,
            recipient: crate::utils::recipient_account(f.recipient, &recipient),
            surplus_recipient: crate::utils::recipient_account(f.refund, &refund),
            expected_recipient: None,
            expected_surplus_recipient: None,
            mint: crate::utils::mint_account(f.dvp.accounts.mint_b, &mint_b),
            expected_auditor: None,
        },
        &[],
        &extras,
    )
    .unwrap();
    let history = execute(&mut context, &f.config, session, &[&f.dvp.authority]);
    let hook_calls: Vec<_> = history
        .iter()
        .flat_map(|tx| tx.inner_instructions.values())
        .flatten()
        .filter(|ix| ix.program_id == hook)
        .collect();
    assert_eq!(hook_calls.len(), 1);
    assert_eq!(hook_calls[0].accounts.len(), 4);
    assert_eq!(get_token_balance(&context, &f.asset_recipient), AMOUNT_A);
    assert_eq!(pending(&context, &f.recipient, &f.recipient_keys), AMOUNT_B);
    assert!(context.get_account(&f.dvp.accounts.swap_dvp).is_none());
    assert_eq!(context.get_account(&validation).is_some(), prefund);
}
