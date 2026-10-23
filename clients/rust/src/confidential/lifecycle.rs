use super::{transaction::SessionBuilder, *};
use crate::{
    instructions::*,
    types::{CtTransferData, LegBRefund},
    verify::ConfidentialSwapDvp,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_zk_elgamal_proof_interface::{instruction::ProofInstruction, proof_data::ZkProofData};
use solana_zk_sdk::{
    encryption::elgamal::{ElGamalCiphertext, ElGamalPubkey},
    zk_elgamal_proof_program::{
        build_ciphertext_ciphertext_equality_proof_data, build_pubkey_validity_proof_data,
        build_zero_ciphertext_proof_data,
    },
};
use spl_token_2022_interface::extension::{
    confidential_transfer::ConfidentialTransferAccount, BaseStateWithExtensions,
    StateWithExtensions,
};
use spl_token_confidential_transfer_proof_generation::{
    transfer::transfer_split_proof_data, try_combine_lo_hi_ciphertexts,
};

// The on-chain bound applies independently to each transfer leg.
const MAX_HOOK_EXTRAS: usize = 32;

// Each CT transfer increments the recipient's pending credit counter once.
const SINGLE_TRANSFER_CREDITS: u64 = 1;
const PAYMENT_AND_SURPLUS_CREDITS: u64 = SINGLE_TRANSFER_CREDITS + SINGLE_TRANSFER_CREDITS;

/// A recipient token account as fetched. Sessions bind it to the instruction's
/// destination, Token-2022 and the leg B mint before reading its ElGamal key.
#[derive(Clone, Copy)]
pub struct RecipientAccount<'a> {
    pub address: Pubkey,
    pub owner: Pubkey,
    pub data: &'a [u8],
}

/// A mint account as fetched. Sessions bind it to the leg B mint before
/// reading its optional auditor key.
#[derive(Clone, Copy)]
pub struct MintAccount<'a> {
    pub address: Pubkey,
    pub owner: Pubkey,
    pub data: &'a [u8],
}

pub struct TransferSource<'a> {
    pub state: &'a ConfidentialTransferAccount,
    pub keys: &'a EscrowKeys,
    pub history: &'a [BalanceEvent],
}

pub fn create_session(
    config: &SessionConfig,
    accounts: &CreateConfidentialDvp,
    mut args: CreateConfidentialDvpInstructionArgs,
    keys: &EscrowKeys,
    amount_b: u64,
) -> Result<TransactionSession, ConfidentialError> {
    let amount = keys.encrypt_amount(amount_b)?;
    args.amount_b_ciphertext_lo = amount.lo;
    args.amount_b_ciphertext_hi = amount.hi;
    args.decryptable_zero_balance = keys.ae.encrypt(0).to_bytes();
    args.pubkey_validity_proof_offset = -1;
    let proof = build_pubkey_validity_proof_data(&keys.elgamal).map_err(proof_error)?;
    SessionBuilder::new(config, accounts.settlement_authority).finish(vec![
        ProofInstruction::VerifyPubkeyValidity.encode_verify_proof(None, &proof),
        accounts.instruction(args),
    ])
}

pub fn apply_session(
    config: &SessionConfig,
    accounts: &ApplyConfidentialDvp,
    mut args: ApplyConfidentialDvpInstructionArgs,
    source: TransferSource<'_>,
) -> Result<TransactionSession, ConfidentialError> {
    // The program does not check the AE that Apply writes. When history is
    // supplied, the snapshot must match it rather than only itself.
    let balance = if source.history.is_empty() {
        read_escrow_balance(source.state, 0, source.keys, source.history)?
    } else {
        recover_escrow_balance(source.state, 0, source.keys, source.history)?
    };
    let amount = balance
        .available
        .checked_add(balance.pending)
        .ok_or(ConfidentialError::Arithmetic)?;
    args.expected_pending_balance_credit_counter = balance.pending_credit_counter;
    args.new_decryptable_available_balance = source.keys.ae.encrypt(amount).to_bytes();
    SessionBuilder::new(config, accounts.signer).finish(vec![accounts.instruction(args)])
}

pub struct SettleRequest<'a> {
    pub source: TransferSource<'a>,
    pub swap: &'a ConfidentialSwapDvp,
    pub expected_amount_b: u64,
    pub recipient: RecipientAccount<'a>,
    pub surplus_recipient: RecipientAccount<'a>,
    /// Recipient keys from the trade agreement. When `None`, the key read from
    /// the recipient snapshot is trusted.
    pub expected_recipient: Option<&'a ElGamalPubkey>,
    pub expected_surplus_recipient: Option<&'a ElGamalPubkey>,
    /// Fetched leg B mint, the source of the optional auditor key.
    pub mint: MintAccount<'a>,
    /// Auditor from application configuration: `Some(None)` requires a mint
    /// without auditor. When `None`, the mint snapshot is trusted.
    pub expected_auditor: Option<Option<&'a ElGamalPubkey>>,
}

pub fn settle_session(
    config: &SessionConfig,
    mut accounts: SettleConfidentialDvp,
    request: SettleRequest<'_>,
    leg_a_extras: &[AccountMeta],
    leg_b_extras: &[AccountMeta],
) -> Result<TransactionSession, ConfidentialError> {
    if config.format == TransactionFormat::V0
        && (!leg_a_extras.is_empty() || !leg_b_extras.is_empty())
    {
        return Err(ConfidentialError::HookedSettleRequiresV1);
    }
    check_settle_accounts(&accounts, request.swap)?;
    let source = request.source;
    check_escrow_keys(source.state, source.keys)?;
    if source.keys.encrypt_amount(request.expected_amount_b)? != request.swap.amount_b {
        return Err(ConfidentialError::AmountMismatch);
    }
    let balance = read_available_balance(source.state, source.keys, source.history)?;
    let surplus = balance.checked_sub(request.expected_amount_b).ok_or(
        ConfidentialError::InsufficientAvailable {
            available: balance,
            required: request.expected_amount_b,
        },
    )?;
    if surplus > MAX_TRANSFER_AMOUNT {
        return Err(ConfidentialError::SurplusTooLarge(surplus));
    }
    let payment_credits =
        if surplus > 0 && accounts.user_a_destination_ata_b == accounts.user_b_ata_b {
            PAYMENT_AND_SURPLUS_CREDITS
        } else {
            SINGLE_TRANSFER_CREDITS
        };
    let mint = &request.swap.base.mint_b;
    let recipient = read_recipient(
        &request.recipient,
        &accounts.user_a_destination_ata_b,
        mint,
        request.expected_recipient,
    )?;
    check_recipient(&recipient, payment_credits)?;
    let surplus_recipient = if surplus > 0 {
        let surplus_recipient = read_recipient(
            &request.surplus_recipient,
            &accounts.user_b_ata_b,
            mint,
            request.expected_surplus_recipient,
        )?;
        check_recipient(&surplus_recipient, payment_credits)?;
        Some(surplus_recipient)
    } else {
        None
    };
    let auditor = read_auditor(&request.mint, mint, request.expected_auditor)?;
    let mut session = SessionBuilder::new(config, accounts.settlement_authority);
    let available = source
        .state
        .available_balance
        .try_into()
        .map_err(|_| ConfidentialError::Account("available ciphertext"))?;
    let payment = prepare_transfer(
        &mut session,
        source.keys,
        &available,
        balance,
        request.expected_amount_b,
        &recipient,
        auditor.as_ref(),
    )?;
    [
        accounts.payment_equality_context,
        accounts.payment_validity_context,
        accounts.payment_range_context,
    ] = payment.contexts;
    for (limb, stored, amount, opening, target) in [
        (
            &payment.limbs[0],
            &request.swap.amount_b.lo,
            request.expected_amount_b & ((1 << AMOUNT_LO_BITS) - 1),
            &source.keys.opening_lo,
            &mut accounts.eq_lo_context,
        ),
        (
            &payment.limbs[1],
            &request.swap.amount_b.hi,
            request.expected_amount_b >> AMOUNT_LO_BITS,
            &source.keys.opening_hi,
            &mut accounts.eq_hi_context,
        ),
    ] {
        let stored =
            ElGamalCiphertext::from_bytes(stored).ok_or(ConfidentialError::AmountMismatch)?;
        let proof = build_ciphertext_ciphertext_equality_proof_data(
            &source.keys.elgamal,
            source.keys.elgamal.pubkey(),
            limb,
            &stored,
            opening,
            amount,
        )
        .map_err(proof_error)?;
        *target = session.proof(ProofInstruction::VerifyCiphertextCiphertextEquality, &proof)?;
    }
    let mut remaining = payment.remaining;
    accounts.surplus_equality_context = None;
    accounts.surplus_validity_context = None;
    accounts.surplus_range_context = None;
    let surplus_b = if let Some(surplus_recipient) = &surplus_recipient {
        let transfer = prepare_transfer(
            &mut session,
            source.keys,
            &remaining,
            surplus,
            surplus,
            surplus_recipient,
            auditor.as_ref(),
        )?;
        [
            accounts.surplus_equality_context,
            accounts.surplus_validity_context,
            accounts.surplus_range_context,
        ] = transfer.contexts.map(Some);
        remaining = transfer.remaining;
        Some(transfer.data)
    } else {
        None
    };
    let zero =
        build_zero_ciphertext_proof_data(&source.keys.elgamal, &remaining).map_err(proof_error)?;
    accounts.zero_context = session.proof(ProofInstruction::VerifyZeroCiphertext, &zero)?;
    let extras = extras(leg_a_extras, leg_b_extras)?;
    session.finish(vec![accounts.instruction_with_remaining_accounts(
        SettleConfidentialDvpInstructionArgs {
            leg_a_extras_count: leg_a_extras.len() as u8,
            payment: payment.data,
            surplus_b,
        },
        &extras,
    )])
}

#[derive(Clone, Copy, Debug)]
pub enum RefundAmount {
    None,
    Partial(u64),
    Full,
}

pub struct RefundRequest<'a> {
    pub source: TransferSource<'a>,
    pub recipient: RecipientAccount<'a>,
    /// When `None`, the key read from the recipient snapshot is trusted.
    pub expected_recipient: Option<&'a ElGamalPubkey>,
    pub amount: RefundAmount,
    /// Fetched leg B mint, the source of the optional auditor key.
    pub mint: MintAccount<'a>,
    /// `Some(None)` requires a mint without auditor. When `None`, the mint
    /// snapshot is trusted.
    pub expected_auditor: Option<Option<&'a ElGamalPubkey>>,
}

// Keep generated account sets directly usable without requiring callers to box them.
#[allow(clippy::large_enum_variant)]
pub enum RefundInstruction {
    Reclaim(ReclaimConfidentialDvp),
    Cancel(CancelConfidentialDvp),
    Reject(RejectConfidentialDvp),
    Recover(
        RecoverConfidentialDvp,
        RecoverConfidentialDvpInstructionArgs,
    ),
}

/// None requires no proof preparation (including a public leg A reclaim).
/// Funded leg B uses a request with Partial or Full. Recover's public balance
/// withdrawal is performed by the program in the same final transaction.
pub fn refund_session(
    config: &SessionConfig,
    instruction: RefundInstruction,
    request: Option<RefundRequest<'_>>,
    leg_a_extras: &[AccountMeta],
    leg_b_extras: &[AccountMeta],
) -> Result<TransactionSession, ConfidentialError> {
    let (authority, destination, mint) = match &instruction {
        RefundInstruction::Reclaim(a) => (a.signer, a.signer_dest_ata, a.mint),
        RefundInstruction::Cancel(a) => (a.settlement_authority, a.user_b_ata_b, a.mint_b),
        RefundInstruction::Reject(a) => (a.signer, a.user_b_ata_b, a.mint_b),
        RefundInstruction::Recover(a, _) => (a.signer, a.signer_dest_ata, a.mint),
    };
    let recipient = |request: &RefundRequest<'_>| {
        read_recipient(
            &request.recipient,
            &destination,
            &mint,
            request.expected_recipient,
        )
    };
    let auditor =
        |request: &RefundRequest<'_>| read_auditor(&request.mint, &mint, request.expected_auditor);
    let mut session = SessionBuilder::new(config, authority);
    let mut contexts = [None; 4];
    let refund = if let Some(request) = request {
        check_escrow_keys(request.source.state, request.source.keys)?;
        // None needs only the all-zero ciphertext; reading the balance could
        // fail on a false AE without history.
        let balance = || {
            read_available_balance(
                request.source.state,
                request.source.keys,
                request.source.history,
            )
        };
        match request.amount {
            RefundAmount::None => {
                if request.source.state.available_balance != Default::default() {
                    return Err(ConfidentialError::BalanceMismatch);
                }
                LegBRefund::None
            }
            RefundAmount::Partial(amount) => {
                if amount == 0 {
                    return Err(ConfidentialError::ZeroPartialRefund);
                }
                let available = request
                    .source
                    .state
                    .available_balance
                    .try_into()
                    .map_err(|_| ConfidentialError::Account("available ciphertext"))?;
                let transfer = prepare_transfer(
                    &mut session,
                    request.source.keys,
                    &available,
                    balance()?,
                    amount,
                    &recipient(&request)?,
                    auditor(&request)?.as_ref(),
                )?;
                for (out, key) in contexts.iter_mut().zip(transfer.contexts) {
                    *out = Some(key);
                }
                LegBRefund::Partial(transfer.data)
            }
            RefundAmount::Full => {
                let balance = balance()?;
                let available = request
                    .source
                    .state
                    .available_balance
                    .try_into()
                    .map_err(|_| ConfidentialError::Account("available ciphertext"))?;
                let transfer = prepare_transfer(
                    &mut session,
                    request.source.keys,
                    &available,
                    balance,
                    balance,
                    &recipient(&request)?,
                    auditor(&request)?.as_ref(),
                )?;
                for (out, key) in contexts.iter_mut().zip(transfer.contexts) {
                    *out = Some(key);
                }
                let zero = build_zero_ciphertext_proof_data(
                    &request.source.keys.elgamal,
                    &transfer.remaining,
                )
                .map_err(proof_error)?;
                contexts[3] = Some(session.proof(ProofInstruction::VerifyZeroCiphertext, &zero)?);
                LegBRefund::Full(transfer.data)
            }
        }
    } else {
        LegBRefund::None
    };
    let extras = extras(leg_a_extras, leg_b_extras)?;
    let ix = match instruction {
        RefundInstruction::Reclaim(mut accounts) => {
            [
                accounts.equality_context,
                accounts.validity_context,
                accounts.range_context,
                accounts.zero_context,
            ] = contexts;
            accounts.instruction_with_remaining_accounts(
                ReclaimConfidentialDvpInstructionArgs {
                    leg_b_refund: refund,
                },
                &extras,
            )
        }
        RefundInstruction::Cancel(mut accounts) => {
            [
                accounts.equality_context,
                accounts.validity_context,
                accounts.range_context,
                accounts.zero_context,
            ] = contexts;
            accounts.instruction_with_remaining_accounts(
                CancelConfidentialDvpInstructionArgs {
                    leg_a_extras_count: leg_a_extras.len() as u8,
                    leg_b_refund: refund,
                },
                &extras,
            )
        }
        RefundInstruction::Reject(mut accounts) => {
            [
                accounts.equality_context,
                accounts.validity_context,
                accounts.range_context,
                accounts.zero_context,
            ] = contexts;
            accounts.instruction_with_remaining_accounts(
                RejectConfidentialDvpInstructionArgs {
                    leg_a_extras_count: leg_a_extras.len() as u8,
                    leg_b_refund: refund,
                },
                &extras,
            )
        }
        RefundInstruction::Recover(mut accounts, mut args) => {
            [
                accounts.equality_context,
                accounts.validity_context,
                accounts.range_context,
                accounts.zero_context,
            ] = contexts;
            if !leg_a_extras.is_empty() {
                return Err(ConfidentialError::Account("Recover has only leg B extras"));
            }
            args.leg_b_refund = refund;
            accounts.instruction_with_remaining_accounts(args, &extras)
        }
    };
    session.finish(vec![ix])
}

/// The program rejects any other account set, but only after the payer has
/// funded proof contexts bound to `settlement_authority`; refuse before that.
fn check_settle_accounts(
    accounts: &SettleConfidentialDvp,
    swap: &ConfidentialSwapDvp,
) -> Result<(), ConfidentialError> {
    use crate::verify::{find_swap_dvp_address, find_swap_dvp_escrow_ata as ata};
    let base = &swap.base;
    let (swap_dvp, _) = find_swap_dvp_address(
        &base.settlement_authority,
        &base.user_a,
        &base.user_b,
        &base.mint_a,
        &base.mint_b,
        base.nonce,
    );
    let (program_a, program_b) = (&base.token_program_a, &base.token_program_b);
    for (field, matches) in [
        (
            "settlement authority",
            accounts.settlement_authority == base.settlement_authority,
        ),
        ("swap", accounts.swap_dvp == swap_dvp),
        ("mint A", accounts.mint_a == base.mint_a),
        ("mint B", accounts.mint_b == base.mint_b),
        ("token program A", accounts.token_program_a == *program_a),
        ("token program B", accounts.token_program_b == *program_b),
        (
            "escrow A",
            accounts.dvp_ata_a == ata(&swap_dvp, &base.mint_a, program_a),
        ),
        (
            "escrow B",
            accounts.dvp_ata_b == ata(&swap_dvp, &base.mint_b, program_b),
        ),
        (
            "user A destination",
            accounts.user_a_destination_ata_b
                == ata(&base.user_a_settlement_destination, &base.mint_b, program_b),
        ),
        (
            "user B destination",
            accounts.user_b_destination_ata_a
                == ata(&base.user_b_settlement_destination, &base.mint_a, program_a),
        ),
        (
            "user A surplus account",
            accounts.user_a_ata_a == ata(&base.user_a, &base.mint_a, program_a),
        ),
        (
            "user B surplus account",
            accounts.user_b_ata_b == ata(&base.user_b, &base.mint_b, program_b),
        ),
    ] {
        if !matches {
            return Err(ConfidentialError::Account(field));
        }
    }
    Ok(())
}

fn read_auditor(
    mint: &MintAccount<'_>,
    expected_mint: &Pubkey,
    expected: Option<Option<&ElGamalPubkey>>,
) -> Result<Option<ElGamalPubkey>, ConfidentialError> {
    let auditor = mint_auditor(mint, expected_mint)?;
    // An RPC must neither substitute nor add an auditor; refuse before any proof.
    if expected.is_some_and(|expected| auditor.as_ref() != expected) {
        return Err(ConfidentialError::Account("mint auditor"));
    }
    Ok(auditor)
}

fn read_recipient(
    recipient: &RecipientAccount<'_>,
    destination: &Pubkey,
    mint: &Pubkey,
    expected_key: Option<&ElGamalPubkey>,
) -> Result<ConfidentialTransferAccount, ConfidentialError> {
    if recipient.address != *destination || recipient.owner != spl_token_2022_interface::ID {
        return Err(ConfidentialError::Account("recipient address or owner"));
    }
    let account =
        StateWithExtensions::<spl_token_2022_interface::state::Account>::unpack(recipient.data)
            .map_err(|_| ConfidentialError::Account("recipient layout"))?;
    if account.base.mint != *mint {
        return Err(ConfidentialError::Account("recipient mint"));
    }
    let state = *account
        .get_extension::<ConfidentialTransferAccount>()
        .map_err(|_| ConfidentialError::Account("recipient CT extension"))?;
    // An unauthenticated key would receive the amount; refuse before any proof.
    if expected_key.is_some_and(|key| state.elgamal_pubkey != (*key).into()) {
        return Err(ConfidentialError::Account("recipient ElGamal key"));
    }
    Ok(state)
}

fn check_recipient(
    recipient: &ConfidentialTransferAccount,
    required: u64,
) -> Result<(), ConfidentialError> {
    if !bool::from(recipient.approved) {
        return Err(ConfidentialError::RecipientNotApproved);
    }
    if !bool::from(recipient.allow_confidential_credits) {
        return Err(ConfidentialError::RecipientCreditsDisabled);
    }
    if u64::from(recipient.pending_balance_credit_counter)
        .checked_add(required)
        .is_none_or(|counter| counter > u64::from(recipient.maximum_pending_balance_credit_counter))
    {
        return Err(ConfidentialError::RecipientPendingCounterFull { required });
    }
    Ok(())
}

struct PreparedTransfer {
    data: CtTransferData,
    contexts: [Pubkey; 3],
    remaining: ElGamalCiphertext,
    limbs: [ElGamalCiphertext; 2],
}

#[allow(clippy::too_many_arguments)]
fn prepare_transfer(
    session: &mut SessionBuilder<'_>,
    keys: &EscrowKeys,
    available: &ElGamalCiphertext,
    balance: u64,
    amount: u64,
    recipient: &ConfidentialTransferAccount,
    auditor: Option<&ElGamalPubkey>,
) -> Result<PreparedTransfer, ConfidentialError> {
    if amount > MAX_TRANSFER_AMOUNT {
        return Err(ConfidentialError::TransferAmountTooLarge(amount));
    }
    check_recipient(recipient, SINGLE_TRANSFER_CREDITS)?;
    let new_balance =
        balance
            .checked_sub(amount)
            .ok_or(ConfidentialError::InsufficientAvailable {
                available: balance,
                required: amount,
            })?;
    let destination = recipient
        .elgamal_pubkey
        .try_into()
        .map_err(|_| ConfidentialError::Account("recipient ElGamal key"))?;
    let proof = transfer_split_proof_data(
        available,
        &keys.ae.encrypt(balance),
        amount,
        &keys.elgamal,
        &keys.ae,
        &destination,
        auditor,
    )
    .map_err(proof_error)?;
    let ciphertext = &proof.ciphertext_validity_proof_data_with_ciphertext;
    let contexts = [
        session.proof(
            ProofInstruction::VerifyCiphertextCommitmentEquality,
            &proof.equality_proof_data,
        )?,
        session.proof(
            ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity,
            &ciphertext.proof_data,
        )?,
        session.proof(
            ProofInstruction::VerifyBatchedRangeProofU128,
            &proof.range_proof_data,
        )?,
    ];
    let context = ciphertext.proof_data.context_data();
    let lo = context
        .grouped_ciphertext_lo
        .try_extract_ciphertext(0)
        .map_err(proof_error)?
        .try_into()
        .map_err(proof_error)?;
    let hi = context
        .grouped_ciphertext_hi
        .try_extract_ciphertext(0)
        .map_err(proof_error)?
        .try_into()
        .map_err(proof_error)?;
    let transfer = try_combine_lo_hi_ciphertexts(&lo, &hi, AMOUNT_LO_BITS)
        .ok_or(ConfidentialError::Arithmetic)?;
    Ok(PreparedTransfer {
        contexts,
        limbs: [lo, hi],
        remaining: available - transfer,
        data: CtTransferData {
            new_source_decryptable_available_balance: keys.ae.encrypt(new_balance).to_bytes(),
            auditor_ciphertext_lo: ciphertext.ciphertext_lo.0,
            auditor_ciphertext_hi: ciphertext.ciphertext_hi.0,
        },
    })
}

fn extras(a: &[AccountMeta], b: &[AccountMeta]) -> Result<Vec<AccountMeta>, ConfidentialError> {
    // The program's hook account bound applies independently to each leg.
    if a.len() > MAX_HOOK_EXTRAS || b.len() > MAX_HOOK_EXTRAS {
        return Err(ConfidentialError::Account("too many hook extras"));
    }
    Ok(a.iter()
        .chain(b)
        .map(|meta| AccountMeta {
            is_signer: false,
            ..meta.clone()
        })
        .collect())
}

fn proof_error(error: impl std::fmt::Display) -> ConfidentialError {
    ConfidentialError::Proof(error.to_string())
}

/// Reads the optional auditor key of `mint`, which must be the account at
/// `expected_mint` owned by Token-2022.
pub fn mint_auditor(
    mint: &MintAccount<'_>,
    expected_mint: &Pubkey,
) -> Result<Option<ElGamalPubkey>, ConfidentialError> {
    if mint.address != *expected_mint || mint.owner != spl_token_2022_interface::ID {
        return Err(ConfidentialError::Account("mint address or owner"));
    }
    let mint = StateWithExtensions::<spl_token_2022_interface::state::Mint>::unpack(mint.data)
        .map_err(|_| ConfidentialError::Account("mint layout"))?;
    let ct = mint.get_extension::<spl_token_2022_interface::extension::confidential_transfer::ConfidentialTransferMint>().map_err(|_| ConfidentialError::Account("mint CT extension"))?;
    Option::from(ct.auditor_elgamal_pubkey)
        .map(
            |key: solana_zk_sdk_pod::encryption::elgamal::PodElGamalPubkey| {
                key.try_into().map_err(proof_error)
            },
        )
        .transpose()
}

/// Resolve Token-2022 hook extras with the hidden-amount sentinel. The caller
/// supplies an account-data fetch callback, so no RPC transport is imposed.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_confidential_hook_accounts<F, Fut>(
    hook_program: &Pubkey,
    source: &Pubkey,
    mint: &Pubkey,
    destination: &Pubkey,
    authority: &Pubkey,
    fetch: F,
) -> Result<Vec<AccountMeta>, ConfidentialError>
where
    F: Fn(Pubkey) -> Fut,
    Fut: std::future::Future<Output = spl_transfer_hook_interface::offchain::AccountDataResult>,
{
    let validation =
        spl_transfer_hook_interface::get_extra_account_metas_address(mint, hook_program);
    let validation_data = fetch(validation)
        .await
        .map_err(|e| ConfidentialError::Transaction(e.to_string()))?;
    // Validation is optional, but Token-2022 still needs the hook program.
    // Anyone can fund the PDA, so an empty account also means no validation.
    let Some(validation_data) = validation_data.filter(|data| !data.is_empty()) else {
        return Ok(vec![AccountMeta::new_readonly(*hook_program, false)]);
    };
    let mut ix = Instruction {
        program_id: spl_token_2022_interface::ID,
        accounts: [source, mint, destination, authority]
            .map(|key| AccountMeta::new_readonly(*key, false))
            .to_vec(),
        data: vec![],
    };
    spl_transfer_hook_interface::offchain::add_extra_account_metas_for_execute(
        &mut ix,
        hook_program,
        source,
        mint,
        destination,
        authority,
        u64::MAX,
        |address| {
            let fetch = &fetch;
            let validation_data = &validation_data;
            async move {
                if address == validation {
                    Ok(Some(validation_data.clone()))
                } else {
                    fetch(address).await
                }
            }
        },
    )
    .await
    .map_err(|e| ConfidentialError::Transaction(e.to_string()))?;
    extras(&[], &ix.accounts[4..])
}

#[cfg(any(test, feature = "test-utils"))]
pub(super) mod test_support {
    use super::*;
    pub struct TransferAccounts {
        pub authority: Pubkey,
        pub source: Pubkey,
        pub mint: Pubkey,
        pub destination: Pubkey,
    }

    pub struct TransferRequest<'a> {
        pub source: TransferSource<'a>,
        pub recipient: &'a ConfidentialTransferAccount,
        pub amount: u64,
        pub auditor: Option<&'a ElGamalPubkey>,
    }

    /// A regular Token-2022 transfer for funding the verified escrow. The wallet
    /// signs the final transfer and context cleanup; this does not invoke DvP.
    pub fn transfer_session(
        config: &SessionConfig,
        accounts: TransferAccounts,
        request: TransferRequest<'_>,
        hook_extras: &[AccountMeta],
    ) -> Result<TransactionSession, ConfidentialError> {
        use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;
        let source = request.source;
        let balance = read_available_balance(source.state, source.keys, source.history)?;
        let available = source
            .state
            .available_balance
            .try_into()
            .map_err(|_| ConfidentialError::Account("available ciphertext"))?;
        let mut session = SessionBuilder::new(config, accounts.authority);
        let transfer = prepare_transfer(
            &mut session,
            source.keys,
            &available,
            balance,
            request.amount,
            request.recipient,
            request.auditor,
        )?;
        let [equality, validity, range] = transfer.contexts;
        let mut instructions =
            spl_token_2022_interface::extension::confidential_transfer::instruction::transfer(
                &spl_token_2022_interface::ID,
                &accounts.source,
                &accounts.mint,
                &accounts.destination,
                &transfer
                    .data
                    .new_source_decryptable_available_balance
                    .into(),
                &transfer.data.auditor_ciphertext_lo.into(),
                &transfer.data.auditor_ciphertext_hi.into(),
                &accounts.authority,
                &[],
                ProofLocation::ContextStateAccount(&equality),
                ProofLocation::ContextStateAccount(&validity),
                ProofLocation::ContextStateAccount(&range),
            )
            .map_err(proof_error)?;
        instructions[0].accounts.extend(extras(&[], hook_extras)?);
        instructions.extend(session.close_proof_contexts());
        session.finish(instructions)
    }
}
