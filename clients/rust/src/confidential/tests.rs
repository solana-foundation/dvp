use super::test_utils::{transfer_session, TransferAccounts, TransferRequest};
use super::*;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use solana_address::Address;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_message::VersionedMessage;
use solana_rent::Rent;
use solana_zk_sdk::encryption::elgamal::ElGamalCiphertext;
use spl_token_2022_interface::extension::confidential_transfer::ConfidentialTransferAccount;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn shared_key_and_amount_vectors() {
    let vector: serde_json::Value = serde_json::from_str(include_str!(
        "../../../test-vectors/confidential-amount-b.json"
    ))
    .unwrap();
    let master = unhex(vector["master_key"].as_str().unwrap());
    let swap: Address = vector["swap_pda"].as_str().unwrap().parse().unwrap();
    let seed = derive_shared_seed(&swap, |message| -> Result<[u8; 32], ()> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&master).unwrap();
        mac.update(message);
        Ok(mac.finalize().into_bytes().into())
    })
    .unwrap();
    assert_eq!(hex(&seed), vector["shared_seed"]);
    let keys = EscrowKeys::from_seed(&seed).unwrap();
    let secret: [u8; 32] = keys.elgamal.secret().into();
    let ae: [u8; 16] = (&keys.ae).into();
    for (bytes, field) in [
        (&keys.elgamal.pubkey().to_bytes()[..], "elgamal_public_key"),
        (&secret[..], "elgamal_secret_key"),
        (&ae[..], "ae_key"),
        (&keys.opening_lo.to_bytes()[..], "opening_lo"),
        (&keys.opening_hi.to_bytes()[..], "opening_hi"),
    ] {
        assert_eq!(hex(bytes), vector[field]);
    }
    for case in vector["amounts"].as_array().unwrap() {
        let amount: u64 = case["amount"].as_str().unwrap().parse().unwrap();
        let encrypted = keys.encrypt_amount(amount).unwrap();
        assert_eq!(hex(&encrypted.lo), case["ciphertext_lo"]);
        assert_eq!(hex(&encrypted.hi), case["ciphertext_hi"]);
        for (bytes, limb) in [
            (&encrypted.lo, amount & ((1 << AMOUNT_LO_BITS) - 1)),
            (&encrypted.hi, amount >> AMOUNT_LO_BITS),
        ] {
            assert!(ciphertext_matches(
                &ElGamalCiphertext::from_bytes(bytes).unwrap(),
                &keys,
                limb
            ));
        }
    }
    for amount in [0, MAX_TRANSFER_AMOUNT + 1, u64::MAX] {
        assert!(matches!(
            keys.encrypt_amount(amount),
            Err(ConfidentialError::InvalidAmount)
        ));
    }
    let mut changed = seed;
    changed[0] ^= 1;
    assert_ne!(
        EscrowKeys::from_seed(&changed)
            .unwrap()
            .encrypt_amount(1)
            .unwrap(),
        keys.encrypt_amount(1).unwrap()
    );
}

// Reproduce the shared Rust/TypeScript fixtures using public test keys.
#[test]
#[ignore = "rewrites the shared Rust/TypeScript test vectors"]
fn regenerate_shared_vectors() {
    let master = [0x42; 32];
    let [authority, user_a, user_b, mint_a, mint_b] =
        [1, 2, 3, 4, 5].map(|b| Address::new_from_array([b; 32]));
    let swap =
        crate::verify::find_swap_dvp_address(&authority, &user_a, &user_b, &mint_a, &mint_b, 42).0;
    let seed = derive_shared_seed(&swap, |message| -> Result<[u8; 32], ()> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&master).unwrap();
        mac.update(message);
        Ok(mac.finalize().into_bytes().into())
    })
    .unwrap();
    let keys = EscrowKeys::from_seed(&seed).unwrap();
    let secret: [u8; 32] = keys.elgamal.secret().into();
    let ae: [u8; 16] = (&keys.ae).into();
    let amounts: Vec<_> = [1, 65_535, 65_536, MAX_TRANSFER_AMOUNT].into_iter().map(|amount| {
        let encrypted = keys.encrypt_amount(amount).unwrap();
        serde_json::json!({ "amount": amount.to_string(), "ciphertext_lo": hex(&encrypted.lo), "ciphertext_hi": hex(&encrypted.hi) })
    }).collect();
    std::fs::write(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-vectors/confidential-amount-b.json"), serde_json::to_string_pretty(&serde_json::json!({
        "master_key": hex(&master), "swap_pda": swap.to_string(), "shared_seed": hex(&seed),
        "elgamal_public_key": hex(&keys.elgamal.pubkey().to_bytes()),
        "elgamal_secret_key": hex(&secret), "ae_key": hex(&ae),
        "opening_lo": hex(&keys.opening_lo.to_bytes()), "opening_hi": hex(&keys.opening_hi.to_bytes()),
        "amounts": amounts
    })).unwrap() + "\n").unwrap();
}

#[test]
fn recover_false_and_corrupt_ae_balances_above_u32() {
    let keys = EscrowKeys::from_seed(&[7; 32]).unwrap();
    let credit = (1u64 << 47) + 65_535;
    let encrypted = keys.encrypt_amount(credit).unwrap();
    let lo = ElGamalCiphertext::from_bytes(&encrypted.lo).unwrap();
    let hi = ElGamalCiphertext::from_bytes(&encrypted.hi).unwrap();
    let mut state = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        pending_balance_lo: (lo + lo + lo).into(),
        pending_balance_hi: (hi + hi + hi).into(),
        pending_balance_credit_counter: 3.into(),
        decryptable_available_balance: keys.ae.encrypt(0).into(),
        ..Default::default()
    };
    let mut history = vec![BalanceEvent::Credit { lo, hi }; 3];
    // The sum of the high limbs exceeds the SDK's u32 discrete-log limit.
    let recovered = read_escrow_balance(&state, 11, &keys, &history).unwrap();
    assert_eq!(recovered.pending, credit * 3);
    assert_eq!(recovered.public, 11);
    state.available_balance = keys.elgamal.pubkey().encrypt(credit * 3).into();
    state.pending_balance_lo = Default::default();
    state.pending_balance_hi = Default::default();
    state.pending_balance_credit_counter = 0.into();
    history.push(BalanceEvent::Apply);
    for ae in [keys.ae.encrypt(1).into(), Default::default()] {
        state.decryptable_available_balance = ae;
        assert!(matches!(
            checked_available_balance(&state, &keys),
            Err(ConfidentialError::BalanceMismatch)
        ));
        assert_eq!(
            read_escrow_balance(&state, 11, &keys, &history)
                .unwrap()
                .available,
            credit * 3
        );
        assert!(read_escrow_balance(&state, 11, &keys, &history[1..]).is_err());
        for error in [
            read_available_balance(&state, &keys, &history[1..]).unwrap_err(),
            read_escrow_balance(&state, 11, &keys, &history[1..]).unwrap_err(),
        ] {
            let cause = std::error::Error::source(&error)
                .expect("history fallback must preserve the initial balance error");
            assert_eq!(
                cause.to_string(),
                ConfidentialError::BalanceMismatch.to_string()
            );
            assert!(matches!(
                &error,
                ConfidentialError::BalanceRecoveryFailed { initial, history }
                    if matches!(initial.as_ref(), ConfidentialError::BalanceMismatch)
                        && matches!(history.as_ref(), ConfidentialError::IncompleteHistory)
            ));
            assert!(error
                .to_string()
                .contains("transaction history is incomplete"));
        }
    }
    // A correct AE value works above u32 without replay or a discrete log.
    state.decryptable_available_balance = keys.ae.encrypt(credit * 3).into();
    let foreign_keys = EscrowKeys::from_seed(&[8; 32]).unwrap();
    assert!(matches!(
        read_available_balance(&state, &foreign_keys, &history),
        Err(ConfidentialError::EscrowKeyMismatch)
    ));
    assert!(matches!(
        read_escrow_balance(&state, 0, &foreign_keys, &history),
        Err(ConfidentialError::EscrowKeyMismatch)
    ));
    assert_eq!(
        read_escrow_balance(&state, 0, &keys, &[])
            .unwrap()
            .available,
        credit * 3
    );
}

#[test]
fn history_skips_failed_transactions_and_rejects_missing_context() {
    let escrow = Address::new_unique();
    let source = Address::new_unique();
    let mint = Address::new_unique();
    let authority = Address::new_unique();
    let equality = Address::new_unique();
    let validity = Address::new_unique();
    let range = Address::new_unique();
    use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;
    let instructions =
        spl_token_2022_interface::extension::confidential_transfer::instruction::transfer(
            &spl_token_2022_interface::ID,
            &source,
            &mint,
            &escrow,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &authority,
            &[],
            ProofLocation::ContextStateAccount(&equality),
            ProofLocation::ContextStateAccount(&validity),
            ProofLocation::ContextStateAccount(&range),
        )
        .unwrap();
    let mut transaction = ExecutedTransaction {
        instructions,
        inner_instructions: Default::default(),
        succeeded: false,
    };
    let mut history = BalanceHistory::new(escrow);
    history.push(&transaction).unwrap();
    assert!(history.events().is_empty());
    transaction.succeeded = true;
    assert!(matches!(
        history.push(&transaction),
        Err(ConfidentialError::IncompleteHistory)
    ));
}

#[test]
fn apply_with_history_rejects_a_snapshot_that_history_contradicts() {
    use crate::generated::instructions::{
        ApplyConfidentialDvp, ApplyConfidentialDvpInstructionArgs,
    };
    let keys = EscrowKeys::from_seed(&[6; 32]).unwrap();
    let encrypt = |amount: u64| -> solana_zk_sdk_pod::encryption::elgamal::PodElGamalCiphertext {
        keys.elgamal.pubkey().encrypt(amount).into()
    };
    let snapshot = |available: u64, pending: u64, counter: u64| ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        available_balance: encrypt(available),
        decryptable_available_balance: keys.ae.encrypt(available).into(),
        pending_balance_lo: encrypt(pending),
        pending_balance_hi: encrypt(0),
        pending_balance_credit_counter: counter.into(),
        ..Default::default()
    };
    let apply = |state: &ConfidentialTransferAccount, history: &[BalanceEvent]| {
        let pubkey = Address::new_unique();
        apply_session(
            &SessionConfig::new(pubkey, TransactionFormat::V1, Default::default()),
            &ApplyConfidentialDvp {
                signer: pubkey,
                swap_dvp: pubkey,
                nonce_tombstone: pubkey,
                dvp_ata_b: pubkey,
                token_program: spl_token_2022_interface::ID,
            },
            ApplyConfidentialDvpInstructionArgs {
                expected_pending_balance_credit_counter: 0,
                new_decryptable_available_balance: [0; 36],
                settlement_authority: pubkey,
                user_a: pubkey,
                user_b: pubkey,
                mint_a: pubkey,
                mint_b: pubkey,
                nonce: 0,
            },
            TransferSource {
                state,
                keys: &keys,
                history,
            },
        )
    };
    // A consistent snapshot passes the fast path, but supplied history wins:
    // forged pending limbs (DVP-9) and a replayed zero available balance
    // (DVP-19) must both be rejected.
    let forged_pending = snapshot(0, 7, 1);
    assert!(apply(&forged_pending, &[]).is_ok());
    assert!(apply(&forged_pending, &[BalanceEvent::Deposit(7)]).is_ok());
    assert!(matches!(
        apply(&forged_pending, &[BalanceEvent::Deposit(5)]),
        Err(ConfidentialError::IncompleteHistory)
    ));
    let replayed_zero = snapshot(0, 0, 0);
    assert!(matches!(
        apply(
            &replayed_zero,
            &[BalanceEvent::Deposit(5), BalanceEvent::Apply]
        ),
        Err(ConfidentialError::IncompleteHistory)
    ));
}

#[test]
fn oversized_plan_returns_an_error() {
    let config = SessionConfig::new(
        Address::new_unique(),
        TransactionFormat::V0,
        Default::default(),
    );
    let plan = PlannedTransaction {
        instructions: vec![solana_instruction::Instruction {
            program_id: Address::new_unique(),
            accounts: vec![],
            data: vec![0; config.transaction_limit()],
        }],
        signers: vec![],
    };
    assert!(matches!(
        plan.message(&config, Default::default()),
        Err(ConfidentialError::TransactionTooLarge { .. })
    ));
}

#[test]
fn proof_accounts_reject_rent_above_the_default() {
    use solana_zk_elgamal_proof_interface::instruction::ProofInstruction;
    use solana_zk_sdk::zk_elgamal_proof_program::build_zero_ciphertext_proof_data;
    let keys = EscrowKeys::from_seed(&[5; 32]).unwrap();
    let zero =
        build_zero_ciphertext_proof_data(&keys.elgamal, &keys.elgamal.pubkey().encrypt(0u64))
            .unwrap();
    let rent = |lamports_per_byte| Rent {
        lamports_per_byte,
        ..Default::default()
    };
    let default = Rent::default().lamports_per_byte;
    // An untrusted Rent may overpay the payer's lamports to the authority,
    // or overflow the rent calculation.
    for (rent, accepted) in [
        (rent(default), true),
        (rent(default + 1), false),
        (rent(u64::MAX), false),
    ] {
        let config = SessionConfig::new(Address::new_unique(), TransactionFormat::V1, rent);
        let result = super::transaction::SessionBuilder::new(&config, Address::new_unique())
            .proof(ProofInstruction::VerifyZeroCiphertext, &zero);
        assert_eq!(result.is_ok(), accepted);
    }
}

#[test]
fn payer_and_authority_can_use_the_same_signer() {
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    let wallet = Keypair::new();
    let plan = PlannedTransaction {
        signers: vec![],
        instructions: vec![solana_system_interface::instruction::transfer(
            &wallet.pubkey(),
            &Address::new_unique(),
            1,
        )],
    };
    for format in [TransactionFormat::V1, TransactionFormat::V0] {
        let config = SessionConfig::new(wallet.pubkey(), format, Default::default());
        let tx = plan
            .sign(&config, Default::default(), &[&wallet, &wallet])
            .unwrap();
        tx.sanitize().unwrap();
        assert_eq!(tx.signatures.len(), 1);
        assert_ne!(tx.signatures[0], Default::default());
    }
}

#[test]
fn truncated_record_history_returns_an_error() {
    use solana_instruction::{AccountMeta, Instruction};
    let write = spl_record::instruction::RecordInstruction::Write {
        offset: 0,
        data: &[7; 8],
    }
    .pack();
    for len in [1, 8, write.len() - 1] {
        let mut history = BalanceHistory::new(Address::new_unique());
        let transaction = ExecutedTransaction {
            instructions: vec![Instruction {
                program_id: RECORD_PROGRAM_ID,
                accounts: vec![AccountMeta::new(Address::new_unique(), false)],
                data: write[..len].to_vec(),
            }],
            inner_instructions: Default::default(),
            succeeded: true,
        };
        assert!(matches!(
            history.push(&transaction),
            Err(ConfidentialError::IncompleteHistory)
        ));
    }
}

#[test]
fn history_decodes_inline_and_record_validity_proofs() {
    use solana_instruction::{AccountMeta, Instruction};
    use solana_zk_elgamal_proof_interface::instruction::{ContextStateInfo, ProofInstruction};
    use solana_zk_sdk::{
        encryption::grouped_elgamal::GroupedElGamal,
        zk_elgamal_proof_program::build_batched_grouped_ciphertext_3_handles_validity_proof_data,
    };
    use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;
    let source_keys = EscrowKeys::from_seed(&[1; 32]).unwrap();
    let keys = EscrowKeys::from_seed(&[2; 32]).unwrap();
    let auditor = EscrowKeys::from_seed(&[3; 32]).unwrap();
    let pubkeys = [
        source_keys.elgamal.pubkey(),
        keys.elgamal.pubkey(),
        auditor.elgamal.pubkey(),
    ];
    let grouped_lo = GroupedElGamal::encrypt_with(pubkeys, 42u64, &keys.opening_lo);
    let grouped_hi = GroupedElGamal::encrypt_with(pubkeys, 3u64, &keys.opening_hi);
    let proof = build_batched_grouped_ciphertext_3_handles_validity_proof_data(
        pubkeys[0],
        pubkeys[1],
        pubkeys[2],
        &grouped_lo,
        &grouped_hi,
        42,
        3,
        &keys.opening_lo,
        &keys.opening_hi,
    )
    .unwrap();
    let escrow = Address::new_unique();
    let source = Address::new_unique();
    let mint = Address::new_unique();
    let authority = Address::new_unique();
    let equality = Address::new_unique();
    let validity = Address::new_unique();
    let range = Address::new_unique();
    let transfer = |location| {
        spl_token_2022_interface::extension::confidential_transfer::instruction::transfer(
            &spl_token_2022_interface::ID,
            &source,
            &mint,
            &escrow,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &authority,
            &[],
            ProofLocation::ContextStateAccount(&equality),
            location,
            ProofLocation::ContextStateAccount(&range),
        )
        .unwrap()
    };
    let inline = transfer(ProofLocation::InstructionOffset(
        std::num::NonZeroI8::new(1).unwrap(),
        &proof,
    ));
    let record = Address::new_unique();
    let record_ix = |instruction: spl_record::instruction::RecordInstruction<'_>| Instruction {
        program_id: RECORD_PROGRAM_ID,
        accounts: vec![AccountMeta::new(record, false)],
        data: instruction.pack(),
    };
    let bytes = bytemuck::bytes_of(&proof);
    let split = bytes.len() / 2;
    let mut staged = vec![
        record_ix(spl_record::instruction::RecordInstruction::Initialize),
        record_ix(spl_record::instruction::RecordInstruction::Write {
            offset: 0,
            data: &bytes[..split],
        }),
        record_ix(spl_record::instruction::RecordInstruction::Write {
            offset: split as u64,
            data: &bytes[split..],
        }),
        ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity
            .encode_verify_proof_from_account(
                Some(ContextStateInfo {
                    context_state_account: &validity,
                    context_state_authority: &authority,
                }),
                &record,
                spl_record::state::RecordData::WRITABLE_START_INDEX as u32,
            ),
    ];
    staged.extend(transfer(ProofLocation::ContextStateAccount(&validity)));
    staged.push(record_ix(
        spl_record::instruction::RecordInstruction::CloseAccount,
    ));
    let state = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        pending_balance_lo: grouped_lo.to_elgamal_ciphertext(1).unwrap().into(),
        pending_balance_hi: grouped_hi.to_elgamal_ciphertext(1).unwrap().into(),
        pending_balance_credit_counter: 1.into(),
        decryptable_available_balance: keys.ae.encrypt(0).into(),
        ..Default::default()
    };
    // Missing initialization is harmless until the Record is needed by a proof.
    // It must never be reconstructed with invented zero bytes.
    let missing_init = ExecutedTransaction {
        instructions: staged[1..].to_vec(),
        inner_instructions: Default::default(),
        succeeded: true,
    };
    assert!(matches!(
        BalanceHistory::new(escrow).push(&missing_init),
        Err(ConfidentialError::IncompleteHistory)
    ));
    let mut resolved = BalanceHistory::new(escrow);
    resolved
        .push_with_proof_accounts(&missing_init, |_| {
            let mut data = vec![0; spl_record::state::RecordData::WRITABLE_START_INDEX];
            data.extend_from_slice(bytes);
            Some(data)
        })
        .unwrap();
    assert_eq!(
        recover_escrow_balance(&state, 0, &keys, resolved.events())
            .unwrap()
            .pending,
        (3 << AMOUNT_LO_BITS) + 42
    );
    let unrelated = record_ix(spl_record::instruction::RecordInstruction::Write {
        offset: 0,
        data: &[42],
    });
    for mut instructions in [inline, staged] {
        instructions.insert(0, unrelated.clone());
        let mut history = BalanceHistory::new(escrow);
        history
            .push(&ExecutedTransaction {
                instructions,
                inner_instructions: Default::default(),
                succeeded: true,
            })
            .unwrap();
        assert_eq!(
            recover_escrow_balance(&state, 0, &keys, history.events())
                .unwrap()
                .pending,
            (3 << AMOUNT_LO_BITS) + 42
        );
    }
}

#[test]
fn validity_verification_without_authority_keeps_the_real_context() {
    use solana_instruction::AccountMeta;
    use solana_zk_elgamal_proof_interface::instruction::{ContextStateInfo, ProofInstruction};
    use solana_zk_sdk::{
        encryption::grouped_elgamal::GroupedElGamal,
        zk_elgamal_proof_program::build_batched_grouped_ciphertext_3_handles_validity_proof_data,
    };
    use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;
    let proof = |keys: [&EscrowKeys; 3], lo: u64, hi: u64| {
        let pubkeys = keys.map(|k| k.elgamal.pubkey());
        let grouped_lo = GroupedElGamal::encrypt_with(pubkeys, lo, &keys[1].opening_lo);
        let grouped_hi = GroupedElGamal::encrypt_with(pubkeys, hi, &keys[1].opening_hi);
        let data = build_batched_grouped_ciphertext_3_handles_validity_proof_data(
            pubkeys[0],
            pubkeys[1],
            pubkeys[2],
            &grouped_lo,
            &grouped_hi,
            lo,
            hi,
            &keys[1].opening_lo,
            &keys[1].opening_hi,
        )
        .unwrap();
        (data, grouped_lo, grouped_hi)
    };
    let keys = EscrowKeys::from_seed(&[2; 32]).unwrap();
    let (real, grouped_lo, grouped_hi) = proof(
        [
            &EscrowKeys::from_seed(&[1; 32]).unwrap(),
            &keys,
            &EscrowKeys::from_seed(&[3; 32]).unwrap(),
        ],
        42,
        3,
    );
    let attacker = EscrowKeys::from_seed(&[4; 32]).unwrap();
    let (forged, _, _) = proof([&attacker, &attacker, &attacker], 7, 1);
    let escrow = Address::new_unique();
    let authority = Address::new_unique();
    let equality = Address::new_unique();
    let validity = Address::new_unique();
    let range = Address::new_unique();
    let record = Address::new_unique();
    let verify = ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity;
    // Without the authority account the proof program verifies and creates
    // nothing, so these must not replace the real context.
    let mut inline = verify.encode_verify_proof(None, &forged);
    inline.accounts.push(AccountMeta::new(validity, false));
    let mut from_record = verify.encode_verify_proof_from_account(
        None,
        &record,
        spl_record::state::RecordData::WRITABLE_START_INDEX as u32,
    );
    from_record.accounts.push(AccountMeta::new(validity, false));
    let state = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        pending_balance_lo: grouped_lo.to_elgamal_ciphertext(1).unwrap().into(),
        pending_balance_hi: grouped_hi.to_elgamal_ciphertext(1).unwrap().into(),
        pending_balance_credit_counter: 1.into(),
        decryptable_available_balance: keys.ae.encrypt(0).into(),
        ..Default::default()
    };
    for poison in [inline, from_record] {
        let mut instructions = vec![verify.encode_verify_proof(
            Some(ContextStateInfo {
                context_state_account: &validity,
                context_state_authority: &authority,
            }),
            &real,
        )];
        instructions.push(poison);
        instructions.extend(
            spl_token_2022_interface::extension::confidential_transfer::instruction::transfer(
                &spl_token_2022_interface::ID,
                &Address::new_unique(),
                &Address::new_unique(),
                &escrow,
                &Default::default(),
                &Default::default(),
                &Default::default(),
                &authority,
                &[],
                ProofLocation::ContextStateAccount(&equality),
                ProofLocation::ContextStateAccount(&validity),
                ProofLocation::ContextStateAccount(&range),
            )
            .unwrap(),
        );
        let mut history = BalanceHistory::new(escrow);
        history
            .push_with_proof_accounts(
                &ExecutedTransaction {
                    instructions,
                    inner_instructions: Default::default(),
                    succeeded: true,
                },
                |_| {
                    let mut data = vec![0; spl_record::state::RecordData::WRITABLE_START_INDEX];
                    data.extend_from_slice(bytemuck::bytes_of(&forged));
                    Some(data)
                },
            )
            .unwrap();
        assert_eq!(
            recover_escrow_balance(&state, 0, &keys, history.events())
                .unwrap()
                .pending,
            (3 << AMOUNT_LO_BITS) + 42
        );
    }
}

#[test]
fn transfer_with_fee_replays_as_transfer_in_both_directions() {
    use solana_zk_sdk::{
        encryption::grouped_elgamal::GroupedElGamal,
        zk_elgamal_proof_program::build_batched_grouped_ciphertext_3_handles_validity_proof_data,
    };
    use spl_token_2022_interface::extension::confidential_transfer::instruction::{
        ConfidentialTransferInstruction, TransferInstructionData, TransferWithFeeInstructionData,
    };
    use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;
    let keys = [1, 2, 3].map(|seed| EscrowKeys::from_seed(&[seed; 32]).unwrap());
    let pubkeys = [0, 1, 2].map(|i| keys[i].elgamal.pubkey());
    let grouped_lo = GroupedElGamal::encrypt_with(pubkeys, 42u64, &keys[1].opening_lo);
    let grouped_hi = GroupedElGamal::encrypt_with(pubkeys, 3u64, &keys[1].opening_hi);
    let proof = build_batched_grouped_ciphertext_3_handles_validity_proof_data(
        pubkeys[0],
        pubkeys[1],
        pubkeys[2],
        &grouped_lo,
        &grouped_hi,
        42,
        3,
        &keys[1].opening_lo,
        &keys[1].opening_hi,
    )
    .unwrap();
    let (source, destination) = (Address::new_unique(), Address::new_unique());
    let transfer =
        spl_token_2022_interface::extension::confidential_transfer::instruction::transfer(
            &spl_token_2022_interface::ID,
            &source,
            &Address::new_unique(),
            &destination,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Address::new_unique(),
            &[],
            ProofLocation::ContextStateAccount(&Address::new_unique()),
            ProofLocation::InstructionOffset(std::num::NonZeroI8::new(1).unwrap(), &proof),
            ProofLocation::ContextStateAccount(&Address::new_unique()),
        )
        .unwrap();
    // On a mint without TransferFeeConfig, Token-2022 runs TransferWithFee
    // with the Transfer accounts and proofs, ignoring the fee offsets.
    let mut with_fee = transfer.clone();
    let token = with_fee.iter_mut().find(|ix| ix.data.len() > 2).unwrap();
    let data: TransferInstructionData = *bytemuck::from_bytes(&token.data[2..]);
    token.data = vec![
        token.data[0],
        ConfidentialTransferInstruction::TransferWithFee as u8,
    ];
    token
        .data
        .extend_from_slice(bytemuck::bytes_of(&TransferWithFeeInstructionData {
            new_source_decryptable_available_balance: data.new_source_decryptable_available_balance,
            transfer_amount_auditor_ciphertext_lo: data.transfer_amount_auditor_ciphertext_lo,
            transfer_amount_auditor_ciphertext_hi: data.transfer_amount_auditor_ciphertext_hi,
            equality_proof_instruction_offset: data.equality_proof_instruction_offset,
            transfer_amount_ciphertext_validity_proof_instruction_offset: data
                .ciphertext_validity_proof_instruction_offset,
            fee_sigma_proof_instruction_offset: 0,
            fee_ciphertext_validity_proof_instruction_offset: 0,
            range_proof_instruction_offset: data.range_proof_instruction_offset,
        }));
    let replay = |escrow: Address, instructions: &Vec<Instruction>| {
        let mut history = BalanceHistory::new(escrow);
        history
            .push(&ExecutedTransaction {
                instructions: instructions.clone(),
                inner_instructions: Default::default(),
                succeeded: true,
            })
            .unwrap();
        format!("{:?}", history.events())
    };
    for escrow in [source, destination] {
        let expected = replay(escrow, &transfer);
        assert_ne!(expected, "[]");
        assert_eq!(replay(escrow, &with_fee), expected);
    }
}

#[test]
fn v1_rejects_too_many_addresses_before_signing() {
    use solana_instruction::{AccountMeta, Instruction};
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    let payer = Keypair::new();
    let config = SessionConfig::new(payer.pubkey(), TransactionFormat::V1, Default::default());
    let mut plan = PlannedTransaction {
        instructions: vec![Instruction {
            program_id: Address::new_unique(),
            accounts: (0..64)
                .map(|_| AccountMeta::new_readonly(Address::new_unique(), false))
                .collect(),
            data: vec![],
        }],
        signers: vec![],
    };
    assert!(matches!(
        plan.wire_size(&config),
        Err(ConfidentialError::Transaction(_))
    ));
    assert!(matches!(
        plan.sign(&config, Default::default(), &[&payer]),
        Err(ConfidentialError::Transaction(_))
    ));
    plan.instructions[0].accounts.truncate(62);
    let tx = plan.sign(&config, Default::default(), &[&payer]).unwrap();
    assert_eq!(tx.message.static_account_keys().len(), 64);
    tx.sanitize().unwrap();
}

#[test]
fn outgoing_preflight_reports_balance_and_recipient_errors() {
    let keys = EscrowKeys::from_seed(&[21; 32]).unwrap();
    let config = SessionConfig::new(
        Address::new_unique(),
        TransactionFormat::V1,
        Default::default(),
    );
    let source = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        available_balance: keys.elgamal.pubkey().encrypt(10u64).into(),
        decryptable_available_balance: keys.ae.encrypt(10).into(),
        ..Default::default()
    };
    let recipient = ConfidentialTransferAccount {
        elgamal_pubkey: (*keys.elgamal.pubkey()).into(),
        approved: true.into(),
        allow_confidential_credits: true.into(),
        maximum_pending_balance_credit_counter: 1.into(),
        ..Default::default()
    };
    let build = |recipient: &ConfidentialTransferAccount, amount| {
        transfer_session(
            &config,
            TransferAccounts {
                authority: config.payer,
                source: Address::new_unique(),
                mint: Address::new_unique(),
                destination: Address::new_unique(),
            },
            TransferRequest {
                source: TransferSource {
                    state: &source,
                    keys: &keys,
                    history: &[],
                },
                recipient,
                amount,
                auditor: None,
            },
            &[],
        )
    };
    assert!(matches!(
        build(&recipient, 11),
        Err(ConfidentialError::InsufficientAvailable {
            available: 10,
            required: 11
        })
    ));
    assert!(matches!(
        build(&recipient, MAX_TRANSFER_AMOUNT + 1),
        Err(ConfidentialError::TransferAmountTooLarge(_))
    ));
    let mut invalid = recipient;
    invalid.approved = false.into();
    assert!(matches!(
        build(&invalid, 1),
        Err(ConfidentialError::RecipientNotApproved)
    ));
    invalid = recipient;
    invalid.allow_confidential_credits = false.into();
    assert!(matches!(
        build(&invalid, 1),
        Err(ConfidentialError::RecipientCreditsDisabled)
    ));
    invalid = recipient;
    invalid.pending_balance_credit_counter = 1.into();
    assert!(matches!(
        build(&invalid, 1),
        Err(ConfidentialError::RecipientPendingCounterFull { required: 1 })
    ));
}

#[cfg(feature = "fetch")]
#[test]
fn rpc_history_rejects_incomplete_or_malformed_metadata() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use solana_instruction::{AccountMeta, Instruction};
    use solana_message::AddressLookupTableAccount;
    use solana_transaction::versioned::VersionedTransaction;
    use solana_transaction_status_client_types::EncodedTransactionWithStatusMeta;
    let looked_up = Address::new_unique();
    let program = Address::new_unique();
    let mut config = SessionConfig::new(
        Address::new_unique(),
        TransactionFormat::V0,
        Default::default(),
    );
    config.lookup_tables.push(AddressLookupTableAccount {
        key: Address::new_unique(),
        addresses: vec![looked_up],
    });
    let message = PlannedTransaction {
        instructions: vec![Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new(looked_up, false)],
            data: vec![],
        }],
        signers: vec![],
    }
    .message(&config, Default::default())
    .unwrap();
    let program_index = message
        .static_account_keys()
        .iter()
        .position(|p| *p == program)
        .unwrap();
    let loaded_index = message.static_account_keys().len();
    let tx = VersionedTransaction {
        signatures: vec![Default::default(); message.header().num_required_signatures as usize],
        message,
    };
    let base = serde_json::json!({
        "transaction": [STANDARD.encode(wincode::serialize(&tx).unwrap()), "base64"],
        "meta": {"err": null, "status": {"Ok": null}, "fee": 0, "preBalances": [], "postBalances": [],
            "loadedAddresses": {"writable": [looked_up.to_string()], "readonly": []},
            "innerInstructions": [{"index": 2, "instructions": [{"programIdIndex": program_index, "accounts": [loaded_index], "data": ""}]}]}
    });
    let decode = |value| {
        ExecutedTransaction::from_rpc(
            &serde_json::from_value::<EncodedTransactionWithStatusMeta>(value).unwrap(),
        )
    };
    assert!(decode(base.clone()).is_ok());
    let mut cases = vec![];
    for field in ["loadedAddresses", "innerInstructions"] {
        let mut value = base.clone();
        value["meta"].as_object_mut().unwrap().remove(field);
        cases.push(value);
    }
    let mut value = base.clone();
    value["meta"] = serde_json::Value::Null;
    cases.push(value);
    let mut value = base.clone();
    value["meta"]["loadedAddresses"]["writable"] = serde_json::json!([]);
    cases.push(value);
    let mut value = base.clone();
    value["meta"]["loadedAddresses"]["writable"][0] = serde_json::json!("invalid address");
    cases.push(value);
    let mut value = base.clone();
    value["meta"]["innerInstructions"][0]["index"] = serde_json::json!(3);
    cases.push(value);
    let mut value = base.clone();
    let group = value["meta"]["innerInstructions"][0].clone();
    value["meta"]["innerInstructions"]
        .as_array_mut()
        .unwrap()
        .push(group);
    cases.push(value);
    let mut value = base.clone();
    value["meta"]["innerInstructions"][0]["instructions"][0]["accounts"] = serde_json::json!([255]);
    cases.push(value);
    let mut value = base.clone();
    value["meta"]["innerInstructions"][0]["instructions"][0] = serde_json::json!({"program": "system", "programId": program.to_string(), "parsed": {"type": "transfer"}});
    cases.push(value);
    for value in cases {
        assert!(matches!(
            decode(value),
            Err(ConfidentialError::IncompleteHistory)
        ));
    }
    let mut failed = base;
    failed["meta"]["err"] = serde_json::json!("AccountNotFound");
    assert!(!decode(failed).unwrap().succeeded);
}

fn plan(payload: usize) -> PlannedTransaction {
    PlannedTransaction {
        instructions: vec![Instruction {
            program_id: Address::new_unique(),
            accounts: vec![],
            data: vec![0; payload],
        }],
        signers: vec![],
    }
}

#[test]
fn priority_fee_uses_micro_lamports_in_v0_and_total_lamports_in_v1() {
    for format in [TransactionFormat::V0, TransactionFormat::V1] {
        let mut config = SessionConfig::new(Address::new_unique(), format, Rent::default());
        // 400k CU at 1 micro-lamport rounds up to one lamport, not zero.
        for (price, total) in [
            (None, None),
            (Some(0), Some(0)),
            (Some(1), Some(1)),
            (Some(250_001), Some(100_001)),
        ] {
            config.compute_unit_price = price;
            let message = plan(0).message(&config, Hash::default()).unwrap();
            match message {
                VersionedMessage::V1(message) => assert_eq!(message.config.priority_fee, total),
                VersionedMessage::V0(message) => {
                    assert_eq!(
                        message.instructions.len(),
                        if price.is_some() { 4 } else { 3 }
                    );
                    if let Some(price) = price {
                        let ix = &message.instructions[2];
                        let expected = ComputeBudgetInstruction::set_compute_unit_price(price);
                        assert_eq!(
                            message.account_keys[ix.program_id_index as usize],
                            expected.program_id
                        );
                        assert_eq!(ix.data, expected.data);
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn v1_priority_fee_rejects_total_lamport_overflow() {
    let mut config = SessionConfig::new(
        Address::new_unique(),
        TransactionFormat::V1,
        Rent::default(),
    );
    config.compute_unit_limit = 1_400_000;
    config.compute_unit_price = Some(u64::MAX);
    assert!(matches!(
        plan(0).message(&config, Hash::default()),
        Err(ConfidentialError::Arithmetic)
    ));
}

#[test]
fn priority_fee_counts_toward_the_transaction_size_limit() {
    for format in [TransactionFormat::V0, TransactionFormat::V1] {
        let mut config = SessionConfig::new(Address::new_unique(), format, Rent::default());
        // Find the largest payload accepted without a fee, including length-prefix changes.
        let mut plan = plan(0);
        while plan.wire_size(&config).is_ok() {
            plan.instructions[0].data.push(0);
        }
        plan.instructions[0].data.pop();
        assert_eq!(plan.wire_size(&config).unwrap(), config.transaction_limit());
        config.compute_unit_price = Some(1);
        assert!(matches!(
            plan.wire_size(&config),
            Err(ConfidentialError::TransactionTooLarge { .. })
        ));
    }
}
