#![cfg(all(feature = "host", not(target_os = "zkvm")))]

use {
    ed25519_dalek::{Signer, SigningKey},
    northstar_transaction_proof::{
        decode_witness, replay,
        vrf_coordinator_profile::{
            inspect_signed_transaction, DiagnosticError, InstructionRole, COORDINATOR_ID,
            MAX_DIAGNOSTIC_TRANSACTION_BYTES, REFERENCE_VERIFY_ID, TEST_CONSUMER_ID,
        },
        ReplayError,
    },
    sha2::{Digest, Sha256},
};

// These signed shape specimens are synthetic, not execution captures or valid requests.
fn signed_shape(program: [u8; 32], name: &str, signers: usize) -> Vec<u8> {
    let keys: Vec<_> = (0..signers)
        .map(|index| {
            SigningKey::from_bytes(&[42u8.checked_add(u8::try_from(index).unwrap()).unwrap(); 32])
        })
        .collect();
    let key_count = signers.checked_add(5).unwrap();
    let mut message = vec![signers as u8, 0, 3, key_count as u8];
    for key in &keys {
        message.extend_from_slice(&key.verifying_key().to_bytes());
    }
    for index in 0..4 {
        message.extend_from_slice(&[80u8.checked_add(index).unwrap(); 32]);
    }
    message.extend_from_slice(&program);
    message.extend_from_slice(&[241; 32]);
    message.extend_from_slice(&[
        1,
        key_count.checked_sub(1).unwrap() as u8,
        2,
        0,
        signers as u8,
        8,
    ]);
    message.extend_from_slice(&Sha256::digest(format!("global:{name}"))[..8]);
    let mut wire = vec![signers as u8];
    for key in keys {
        wire.extend_from_slice(&key.sign(&message).to_bytes());
    }
    wire.extend_from_slice(&message);
    wire
}

fn retained_witness() -> northstar_transaction_proof::ReplayWitnessV1 {
    decode_witness(include_bytes!(
        "../../zkvm-replay/evidence/resolver-v1/witness-v2.bin"
    ))
    .unwrap()
}

#[test]
fn invariant_pinned_instruction_shapes_remain_outside_frozen_wire_gate() {
    for (program, name, role) in [
        (
            COORDINATOR_ID,
            "request_random",
            InstructionRole::CoordinatorRequest,
        ),
        (
            COORDINATOR_ID,
            "fulfill",
            InstructionRole::CoordinatorFulfill,
        ),
        (
            REFERENCE_VERIFY_ID,
            "verify",
            InstructionRole::ReferenceVerify,
        ),
        (
            TEST_CONSUMER_ID,
            "make_request",
            InstructionRole::TestConsumerRequest,
        ),
        (
            TEST_CONSUMER_ID,
            "on_random",
            InstructionRole::TestConsumerCallback,
        ),
    ] {
        let wire = signed_shape(program, name, 1);
        let report = inspect_signed_transaction(&wire).unwrap();
        assert_eq!(report.signatures_verified, 1);
        assert_eq!(report.account_keys.len(), 6);
        assert_eq!(report.writable_accounts, 3);
        assert!(report.account_privileges[0].signer);
        assert!(report.account_privileges[0].writable);
        assert!(!report.account_privileges[5].signer);
        assert!(!report.account_privileges[5].writable);
        assert_eq!(report.recent_blockhash, [241; 32]);
        assert_eq!(report.instructions[0].accounts, [0, 1]);
        assert_eq!(report.instructions[0].role, role);
        assert_eq!(report.frozen_wire_gate, Err(ReplayError::Wire));
        let text = report.to_text();
        assert!(text.contains("\nexecution_verified=false\n"));
        assert!(text.contains("\nvrf_ready=false\n"));
        assert!(text.contains("\nproof_generated=false\n"));
        assert_eq!(text, inspect_signed_transaction(&wire).unwrap().to_text());

        // Exercise the authoritative relation too, not a copy of its wire rules.
        let mut witness = retained_witness();
        witness.transaction_bytes = wire;
        assert_eq!(replay(&witness), Err(ReplayError::Wire));
    }
}

#[test]
fn invariant_source_callback_key_maps_remain_rejected() {
    // Exact logical message maps from MakeRequest/Fulfill, with synthetic state keys.
    // Requester aliases the top-level consumer program; payer aliases fee payer.
    for (program, name, readonly, accounts, writable) in [
        (
            TEST_CONSUMER_ID,
            "make_request",
            6,
            vec![0, 1, 3, 8, 4, 2, 5, 6, 7],
            3,
        ),
        (
            COORDINATOR_ID,
            "fulfill",
            5,
            vec![0, 4, 1, 2, 5, 6, 7, 3],
            4,
        ),
    ] {
        let signer = SigningKey::from_bytes(&[42; 32]);
        let mut message = vec![1, 0, readonly, 9];
        message.extend_from_slice(&signer.verifying_key().to_bytes());
        for index in 0..7u8 {
            message.extend_from_slice(&[index; 32]);
        }
        message.extend_from_slice(&program);
        message.extend_from_slice(&[241; 32]);
        message.extend_from_slice(&[1, 8, accounts.len() as u8]);
        message.extend_from_slice(&accounts);
        message.push(8);
        message.extend_from_slice(&Sha256::digest(format!("global:{name}"))[..8]);
        let mut wire = vec![1];
        wire.extend_from_slice(&signer.sign(&message).to_bytes());
        wire.extend_from_slice(&message);
        let report = inspect_signed_transaction(&wire).unwrap();
        assert_eq!(report.account_keys.len(), 9);
        assert_eq!(report.writable_accounts, writable);
        assert_eq!(report.instructions[0].accounts, accounts);
        assert_eq!(report.frozen_wire_gate, Err(ReplayError::Wire));
        let mut witness = retained_witness();
        witness.transaction_bytes = wire;
        assert_eq!(replay(&witness), Err(ReplayError::Wire));
    }
}

#[test]
#[ignore = "requires independently retained signed runtime captures"]
fn invariant_captured_request_and_fulfillment_remain_rejected() {
    for (variable, expected_role) in [
        (
            "NORTHSTAR_VRF_REQUEST_TRANSACTION",
            InstructionRole::TestConsumerRequest,
        ),
        (
            "NORTHSTAR_VRF_FULFILL_TRANSACTION",
            InstructionRole::CoordinatorFulfill,
        ),
    ] {
        let path = std::env::var_os(variable).expect("set both capture paths");
        let wire = std::fs::read(path).expect("capture must be readable");
        let report = inspect_signed_transaction(&wire).expect("capture signatures must verify");
        assert!(report
            .instructions
            .iter()
            .any(|instruction| instruction.role == expected_role));
        assert_eq!(report.frozen_wire_gate, Err(ReplayError::Wire));
        let mut witness = retained_witness();
        witness.transaction_bytes = wire;
        assert_eq!(replay(&witness), Err(ReplayError::Wire));
        println!("{}", report.to_text());
    }
}

#[test]
fn invariant_all_message_signatures_are_verified() {
    let wire = signed_shape(COORDINATOR_ID, "request_random", 2);
    let report = inspect_signed_transaction(&wire).unwrap();
    assert_eq!(report.signatures_verified, 2);
    assert_eq!(report.frozen_wire_gate, Err(ReplayError::Wire));
    for (index, offset) in [1, 65].into_iter().enumerate() {
        let mut changed = wire.clone();
        changed[offset] ^= 1;
        assert_eq!(
            inspect_signed_transaction(&changed),
            Err(DiagnosticError::Signature { index })
        );
    }
}

#[test]
fn invariant_message_change_invalidates_signature() {
    let mut wire = signed_shape(COORDINATOR_ID, "fulfill", 1);
    *wire.last_mut().unwrap() ^= 1;
    assert_eq!(
        inspect_signed_transaction(&wire),
        Err(DiagnosticError::Signature { index: 0 })
    );
}

#[test]
fn invariant_roles_require_pinned_program_identity() {
    let wire = signed_shape([99; 32], "fulfill", 1);
    let report = inspect_signed_transaction(&wire).unwrap();
    assert_eq!(report.instructions[0].role, InstructionRole::Other);
    assert_eq!(report.frozen_wire_gate, Err(ReplayError::Wire));
}

#[test]
fn invariant_malformed_and_unsupported_wire_rejects() {
    let wire = signed_shape(COORDINATOR_ID, "fulfill", 1);
    for length in 0..wire.len() {
        assert!(inspect_signed_transaction(&wire[..length]).is_err());
    }
    let mut trailing = wire.clone();
    trailing.push(0);
    assert_eq!(
        inspect_signed_transaction(&trailing),
        Err(DiagnosticError::Encoding)
    );
    let mut noncanonical = vec![0x81, 0];
    noncanonical.extend_from_slice(&wire[1..]);
    assert_eq!(
        inspect_signed_transaction(&noncanonical),
        Err(DiagnosticError::Encoding)
    );
    let mut versioned = wire;
    versioned[65] = 0x80;
    assert_eq!(
        inspect_signed_transaction(&versioned),
        Err(DiagnosticError::VersionedMessageUnsupported)
    );
    assert_eq!(
        inspect_signed_transaction(&vec![0; MAX_DIAGNOSTIC_TRANSACTION_BYTES + 1]),
        Err(DiagnosticError::ResourceLimit)
    );
}

#[test]
fn invariant_diagnostic_cli_does_not_report_execution_admission() {
    let binary = env!("CARGO_BIN_EXE_vrf-coordinator-shape");
    let usage = std::process::Command::new(binary).output().unwrap();
    assert_eq!(usage.status.code(), Some(2));

    let path = std::env::temp_dir().join(format!("northstar-vrf-shape-{}.bin", std::process::id()));
    let mut wire = signed_shape(COORDINATOR_ID, "fulfill", 1);
    std::fs::write(&path, &wire).unwrap();
    let valid = std::process::Command::new(binary)
        .arg(&path)
        .output()
        .unwrap();
    *wire.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &wire).unwrap();
    let invalid = std::process::Command::new(binary)
        .arg(&path)
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();

    assert_eq!(valid.status.code(), Some(0));
    let text = String::from_utf8(valid.stdout).unwrap();
    assert!(text.contains("\nfrozen_wire_gate=Err(Wire)\n"));
    assert!(text.contains("\nexecution_verified=false\n"));
    assert!(text.contains("\nproof_generated=false\n"));
    assert_eq!(invalid.status.code(), Some(1));
}

#[test]
fn invariant_retained_old_relation_still_replays() {
    let witness = retained_witness();
    let expected = replay(&witness).unwrap();
    let report = inspect_signed_transaction(&witness.transaction_bytes).unwrap();
    assert_eq!(report.frozen_wire_gate, Ok(()));
    assert_eq!(report.signatures_verified, 1);
    assert_eq!(report.instructions[0].role, InstructionRole::Other);
    assert_eq!(replay(&witness).unwrap(), expected);
}

#[test]
fn invariant_curve_and_cpi_rows_remain_rejected() {
    for key in [
        0xaa26_07ca,
        0xdd1c_41a6,
        0x60a4_0880,
        0xa22b_9c85,
        0xd744_9092,
    ] {
        let mut witness = retained_witness();
        let row = witness
            .vm_rows
            .iter_mut()
            .find(|row| row.syscall_key != 0)
            .unwrap();
        row.syscall_key = key;
        northstar_transaction_proof::set_trace_hash(&mut witness);
        assert_eq!(replay(&witness), Err(ReplayError::Trace));
    }
}

#[test]
fn invariant_initialization_close_and_rollback_are_not_enabled() {
    let mut initialized = retained_witness();
    initialized.pre_accounts[1].owner = [0; 32];
    assert_eq!(replay(&initialized), Err(ReplayError::Accounts));

    let mut closed = retained_witness();
    closed.post_accounts[1].lamports = 0;
    assert_eq!(replay(&closed), Err(ReplayError::Accounts));

    let mut rollback = retained_witness();
    rollback
        .rollback_accounts
        .push(rollback.pre_accounts[1].clone());
    assert_eq!(replay(&rollback), Err(ReplayError::Accounts));
}
