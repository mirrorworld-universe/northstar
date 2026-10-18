use northstar_transaction_proof::{
    decode_witness, encode_witness, public_inputs_bytes,
    readiness::{
        inspect_encoded, runtime_profile_sha256, Check, ExpectedBindings, ReadinessReport,
        VRF_BLOCKERS,
    },
    replay, set_trace_hash, ReplayError, ReplayWitnessV1, SOL_MEMCPY_KEY,
};

const BASELINE: &[u8] = include_bytes!("../../zkvm-replay/fixture-v1.bin");
const HISTORY: &[u8] =
    include_bytes!("../../zkvm-replay/evidence/hardened-runner-v1/witness-v2.bin");
const HISTORY_PUBLIC: &[u8] =
    include_bytes!("../../zkvm-replay/evidence/hardened-runner-v1/northstar-sp1-public-inputs.bin");

fn inspect_against(witness: &ReplayWitnessV1, reference: &ReplayWitnessV1) -> ReadinessReport {
    let expected = public_inputs_bytes(replay(reference).unwrap());
    inspect_encoded(
        &encode_witness(witness).unwrap(),
        ExpectedBindings {
            public_inputs: &expected,
            checkpoint_transaction_effect_root: Some(
                reference.checkpoint.checkpoint_transaction_effect_root,
            ),
            runtime_profile_sha256: Some(runtime_profile_sha256(reference)),
        },
    )
    .unwrap()
}

fn check(report: &ReadinessReport, name: &str) -> Check {
    report
        .checks
        .iter()
        .find(|(key, _)| *key == name)
        .unwrap()
        .1
}

#[test]
fn retained_history_witness_matches_retained_public_inputs_without_accepting_old_proof() {
    let witness = decode_witness(HISTORY).unwrap();
    let report = inspect_encoded(
        HISTORY,
        ExpectedBindings {
            public_inputs: HISTORY_PUBLIC,
            checkpoint_transaction_effect_root: None,
            runtime_profile_sha256: None,
        },
    )
    .unwrap();
    assert!(report.conformant());
    assert!(!report.all_bindings_anchored());
    assert_eq!(report.witness_bytes, 365_709);
    assert_eq!(report.vm_rows, 208);
    assert_eq!(report.executed_units, 218);
    assert_eq!(report.syscall_rows, [(SOL_MEMCPY_KEY, 1)].into());
    assert_eq!(report.pre_accounts, 2);
    assert_eq!(report.post_accounts, 2);
    assert_eq!(report.readonly_accounts, 1);
    assert_eq!(report.vm_invocation_tags, 1);
    assert_eq!(encode_witness(&witness).unwrap(), HISTORY);
    let text = report.to_text();
    assert!(text.contains("vrf_ready=false\nproof_generated=false\n"));
    for blocker in VRF_BLOCKERS {
        assert!(text.contains(blocker));
    }
    // This is native consistency evidence from an older retained execution, not a fresh ER run.
    let repeated = inspect_encoded(
        HISTORY,
        ExpectedBindings {
            public_inputs: HISTORY_PUBLIC,
            checkpoint_transaction_effect_root: None,
            runtime_profile_sha256: None,
        },
    )
    .unwrap();
    assert_eq!(text, repeated.to_text());
}

#[test]
fn existing_baseline_route_remains_conformant_and_diagnostics_are_deterministic() {
    let witness = decode_witness(BASELINE).unwrap();
    let first = inspect_against(&witness, &witness);
    assert!(first.conformant());
    assert!(first.all_bindings_anchored());
    assert_eq!(
        first.to_text(),
        inspect_against(&witness, &witness).to_text()
    );
}

#[test]
fn canonical_encoding_rejects_truncation_trailing_bytes_and_bad_bool() {
    let expected = HISTORY_PUBLIC;
    for encoded in [
        HISTORY[..HISTORY.len() - 1].to_vec(),
        [HISTORY, &[0]].concat(),
    ] {
        assert_eq!(
            inspect_encoded(
                &encoded,
                ExpectedBindings {
                    public_inputs: expected,
                    checkpoint_transaction_effect_root: None,
                    runtime_profile_sha256: None,
                }
            ),
            Err(ReplayError::Encoding)
        );
    }
    // Account bool validation is also covered by Borsh decoding, not coercion to true.
    let mut account = decode_witness(BASELINE).unwrap().pre_accounts[0].clone();
    account.signer = true;
    let mut bytes = borsh::to_vec(&account).unwrap();
    bytes[32] = 2;
    assert!(borsh::from_slice::<northstar_transaction_proof::AccountWitnessV1>(&bytes).is_err());
}

#[test]
fn each_expected_public_binding_is_checked() {
    let reference = decode_witness(HISTORY).unwrap();
    for index in 0..8 {
        let mut public = HISTORY_PUBLIC.to_vec();
        public[index * 32 + 31] ^= 1;
        let report = inspect_encoded(
            HISTORY,
            ExpectedBindings {
                public_inputs: &public,
                checkpoint_transaction_effect_root: None,
                runtime_profile_sha256: None,
            },
        )
        .unwrap();
        assert!(!report.conformant(), "public field {index}");
        assert_ne!(
            check(&report, "expected_public_inputs_binding"),
            Check::Pass
        );
        assert_eq!(check(&report, "native_relation"), Check::Pass);
    }
    assert!(inspect_against(&reference, &reference).conformant());
}

#[test]
fn expected_public_encoding_rejects_aliases_domains_and_missing_commitments() {
    let mut noncanonical = HISTORY_PUBLIC.to_vec();
    noncanonical[32..64].copy_from_slice(&northstar_zk_types::BN254_FR_MODULUS_BE);
    let mut wrong_domain = HISTORY_PUBLIC.to_vec();
    wrong_domain[31] ^= 1;
    let mut missing = HISTORY_PUBLIC.to_vec();
    missing[96..128].fill(0);
    for public in [
        noncanonical,
        wrong_domain,
        missing,
        HISTORY_PUBLIC[..255].to_vec(),
        [HISTORY_PUBLIC, &[0]].concat(),
    ] {
        let report = inspect_encoded(
            HISTORY,
            ExpectedBindings {
                public_inputs: &public,
                checkpoint_transaction_effect_root: None,
                runtime_profile_sha256: None,
            },
        )
        .unwrap();
        assert!(!report.conformant());
        assert_ne!(
            check(&report, "expected_public_inputs_encoding"),
            Check::Pass
        );
    }
}

#[test]
fn canonical_session_context_is_a_stricter_host_check_not_a_guest_change() {
    let reference = decode_witness(HISTORY).unwrap();
    let mutations: &[fn(&mut ReplayWitnessV1)] = &[
        |w| {
            w.session_context.pop();
        },
        |w| w.session_context.push(0),
        |w| w.session_context[0] ^= 1,
        |w| *w.session_context.last_mut().unwrap() = 2,
    ];
    for mutate in mutations {
        let mut changed = reference.clone();
        mutate(&mut changed);
        // Use the changed public inputs to isolate the host-only canonical shape guard.
        let report = inspect_against(&changed, &changed);
        assert_eq!(check(&report, "native_relation"), Check::Pass);
        assert_eq!(
            check(&report, "expected_public_inputs_binding"),
            Check::Pass
        );
        assert_eq!(
            check(&report, "canonical_session_context"),
            Check::Rejected(ReplayError::Domain)
        );
        assert!(!report.conformant());
    }
}

#[test]
fn external_checkpoint_root_anchor_is_not_inferred_from_leaf_public_input() {
    let witness = decode_witness(HISTORY).unwrap();
    let mut root = witness.checkpoint.checkpoint_transaction_effect_root;
    root[0] ^= 1;
    let report = inspect_encoded(
        HISTORY,
        ExpectedBindings {
            public_inputs: HISTORY_PUBLIC,
            checkpoint_transaction_effect_root: Some(root),
            runtime_profile_sha256: None,
        },
    )
    .unwrap();
    assert_eq!(check(&report, "native_relation"), Check::Pass);
    assert_eq!(check(&report, "checkpoint_root_anchor"), Check::Mismatch);
    assert!(!report.conformant());
}

#[test]
fn changed_checkpoint_and_context_bindings_reject_against_original() {
    let reference = decode_witness(HISTORY).unwrap();
    let mutations: &[fn(&mut ReplayWitnessV1)] = &[
        |w| w.session_context[0] ^= 1,
        |w| w.er_slot += 1,
        |w| w.step_index += 1,
        |w| w.checkpoint.pre_state_accounts[0].lamports += 1,
        |w| w.checkpoint.post_state_accounts[0].rent_epoch ^= 1,
        |w| w.checkpoint.transaction_effect[0] ^= 1,
        |w| w.checkpoint.transaction_effect_path.siblings[0][0] ^= 1,
        |w| w.checkpoint.transaction_effect_path.leaf_index ^= 1,
        |w| w.checkpoint.transaction_effect_path.leaf_count = 17,
        |w| w.checkpoint.checkpoint_transaction_effect_root[0] ^= 1,
        |w| w.checkpoint.readonly_l1_values[0].value.observed_l1_slot += 1,
        |w| w.checkpoint.readonly_l1_root[0] ^= 1,
        |w| w.checkpoint.settlement_effects[0].path.siblings[0][0] ^= 1,
        |w| w.checkpoint.settlement_effect_root[0] ^= 1,
        |w| w.runtime.slot += 1,
        |w| w.result.executed_units += 1,
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut changed = reference.clone();
        mutate(&mut changed);
        set_trace_hash(&mut changed);
        assert!(
            !inspect_against(&changed, &reference).conformant(),
            "binding {index}"
        );
    }
}

#[test]
fn runtime_profile_pin_covers_metadata_not_enforced_by_current_public_inputs() {
    let reference = decode_witness(HISTORY).unwrap();
    let mutations: &[fn(&mut ReplayWitnessV1)] = &[
        |w| w.runtime.agave_revision[0] ^= 1,
        |w| w.runtime.northstar_revision[0] ^= 1,
        |w| w.runtime.feature_set_hash[0] ^= 1,
        |w| w.runtime.vm_config_hash[0] ^= 1,
        |w| w.runtime.syscall_registry_hash[0] ^= 1,
        |w| w.runtime.programdata_id[0] ^= 1,
    ];
    for mutate in mutations {
        let mut changed = reference.clone();
        mutate(&mut changed);
        let report = inspect_against(&changed, &reference);
        assert_eq!(check(&report, "native_relation"), Check::Pass);
        assert_eq!(
            check(&report, "expected_public_inputs_binding"),
            Check::Pass
        );
        assert_eq!(check(&report, "runtime_profile_anchor"), Check::Mismatch);
        assert!(!report.conformant());
    }
}

#[test]
fn excluded_transaction_account_and_result_shapes_stay_rejected_after_rehash() {
    let reference = decode_witness(BASELINE).unwrap();
    let mutations: &[fn(&mut ReplayWitnessV1)] = &[
        |w| w.pre_accounts.push(w.pre_accounts[1].clone()),
        |w| w.post_accounts.push(w.post_accounts[1].clone()),
        |w| w.rollback_accounts.push(w.pre_accounts[1].clone()),
        |w| w.pre_accounts[1].key = w.pre_accounts[0].key,
        |w| w.pre_accounts[1].owner[0] ^= 1,
        |w| w.instruction_data = vec![2],
        |w| w.result.executed_success = false,
        |w| w.runtime.sbpf_version = 1,
        |w| w.runtime.recent_blockhashes.clear(),
        |w| w.transaction_bytes[65] |= 0x80,
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut changed = reference.clone();
        mutate(&mut changed);
        set_trace_hash(&mut changed);
        let report = inspect_against(&changed, &reference);
        assert!(!report.conformant(), "shape {index}");
        assert_ne!(
            check(&report, "native_relation"),
            Check::Pass,
            "shape {index}"
        );
    }
}

#[test]
fn curve_cpi_and_unknown_syscalls_are_diagnosed_but_not_allowed() {
    let reference = decode_witness(BASELINE).unwrap();
    for key in [
        0xaa26_07ca,
        0xdd1c_41a6,
        0x60a4_0880,
        0x080c_98b0,
        0xf111_a47e,
        0xa22b_9c85,
        0xd744_9092,
        1,
    ] {
        let mut changed = reference.clone();
        changed
            .vm_rows
            .iter_mut()
            .find(|row| row.syscall_key == SOL_MEMCPY_KEY)
            .unwrap()
            .syscall_key = key;
        set_trace_hash(&mut changed);
        let report = inspect_against(&changed, &reference);
        assert_eq!(check(&report, "trace"), Check::Rejected(ReplayError::Trace));
        assert!(!report.conformant());
        assert_eq!(report.syscall_rows, [(key, 1)].into());
        assert!(report.to_text().contains("unsupported"));
    }
    // These are exclusion mutations, never evidence of successful curve/CPI execution.
}

#[test]
fn syscall_count_remains_exactly_one() {
    let reference = decode_witness(BASELINE).unwrap();
    for extra in [false, true] {
        let mut changed = reference.clone();
        if extra {
            changed.vm_rows[0].syscall_key = SOL_MEMCPY_KEY;
        } else {
            changed
                .vm_rows
                .iter_mut()
                .for_each(|row| row.syscall_key = 0);
        }
        set_trace_hash(&mut changed);
        let report = inspect_against(&changed, &reference);
        assert_eq!(check(&report, "trace"), Check::Rejected(ReplayError::Trace));
        assert!(!report.conformant());
    }
}

// Fixed fixture expectations test CLI pin comparison, not independently trusted capture.
const TEST_CHECKPOINT_ROOT: &str =
    "0d0dd7a14483bd2afe1ec6050ca6f35ea112531f5f6b539878efd485d1953c77";
const TEST_RUNTIME_PROFILE: &str =
    "8a8a3920abdab3ae44c401f433c9fc9107e432cd7ec71344764e857a432a60a5";
const MISMATCHED_PIN: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn cli_report(args: &[&str], expected_code: i32) -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../zkvm-replay/evidence/hardened-runner-v1");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_vrf-proof-readiness"))
        .arg(root.join("witness-v2.bin"))
        .arg(root.join("northstar-sp1-public-inputs.bin"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "args={args:?}, stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn cli_rejects_supplied_anchor_mismatches() {
    for flag in ["--checkpoint-root", "--runtime-profile"] {
        assert!(cli_report(&[flag, MISMATCHED_PIN], 2).contains("conformant=false"));
    }
}

#[test]
fn cli_distinguishes_native_conformance_from_vrf_readiness() {
    for (args, code) in [(&[][..], 0), (&["--require-vrf"][..], 3)] {
        assert!(cli_report(args, code).contains("vrf_ready=false"));
    }
}

#[test]
fn cli_strict_anchors_rejects_both_missing() {
    let report = cli_report(&["--require-anchors"], 4);
    assert!(report.contains("conformant=true\nall_bindings_anchored=false"));
    assert!(report.contains("check.checkpoint_root_anchor=Missing"));
    assert!(report.contains("check.runtime_profile_anchor=Missing"));
}

#[test]
fn cli_strict_anchors_rejects_either_missing() {
    for (flag, pin, missing, supplied) in [
        (
            "--checkpoint-root",
            TEST_CHECKPOINT_ROOT,
            "runtime_profile_anchor",
            "checkpoint_root_anchor",
        ),
        (
            "--runtime-profile",
            TEST_RUNTIME_PROFILE,
            "checkpoint_root_anchor",
            "runtime_profile_anchor",
        ),
    ] {
        let report = cli_report(&["--require-anchors", flag, pin], 4);
        assert!(report.contains("all_bindings_anchored=false"));
        assert!(report.contains(&format!("check.{missing}=Missing")));
        assert!(report.contains(&format!("check.{supplied}=Pass")));
    }
}

#[test]
fn cli_strict_anchors_rejects_either_mismatch() {
    for (root, profile, mismatched) in [
        (
            MISMATCHED_PIN,
            TEST_RUNTIME_PROFILE,
            "checkpoint_root_anchor",
        ),
        (
            TEST_CHECKPOINT_ROOT,
            MISMATCHED_PIN,
            "runtime_profile_anchor",
        ),
    ] {
        let report = cli_report(
            &[
                "--require-anchors",
                "--checkpoint-root",
                root,
                "--runtime-profile",
                profile,
            ],
            2,
        );
        assert!(report.contains("conformant=false"));
        assert!(report.contains(&format!("check.{mismatched}=Mismatch")));
    }
}

#[test]
fn cli_strict_anchors_accepts_matching_supplied_pins_only_as_native_conformance() {
    let report = cli_report(
        &[
            "--require-anchors",
            "--checkpoint-root",
            TEST_CHECKPOINT_ROOT,
            "--runtime-profile",
            TEST_RUNTIME_PROFILE,
        ],
        0,
    );
    assert!(report.contains("conformant=true\nall_bindings_anchored=true"));
    assert!(report.contains("vrf_ready=false\nproof_generated=false"));
}

#[test]
fn cli_combined_flags_prioritize_rejection_then_missing_anchors_then_vrf_refusal() {
    let cases: &[(&[&str], i32)] = &[
        (&[], 4),
        (&["--checkpoint-root", TEST_CHECKPOINT_ROOT], 4),
        (&["--runtime-profile", TEST_RUNTIME_PROFILE], 4),
        (&["--checkpoint-root", MISMATCHED_PIN], 2),
        (&["--runtime-profile", MISMATCHED_PIN], 2),
        (
            &[
                "--checkpoint-root",
                TEST_CHECKPOINT_ROOT,
                "--runtime-profile",
                TEST_RUNTIME_PROFILE,
            ],
            3,
        ),
    ];
    for flags in [
        ["--require-anchors", "--require-vrf"],
        ["--require-vrf", "--require-anchors"],
    ] {
        for (pins, code) in cases {
            let mut args = flags.to_vec();
            args.extend_from_slice(pins);
            let report = cli_report(&args, *code);
            assert!(report.contains("vrf_ready=false\nproof_generated=false"));
        }
    }
}
