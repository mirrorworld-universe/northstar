mod support;

use {
    northstar_vrf_oracle::{Error, Ingest, Oracle, Status},
    std::fs,
    support::{key, metadata, other_key, policy, request},
};

fn open(path: &std::path::Path) -> Oracle {
    Oracle::open(path, metadata(), policy()).unwrap()
}

#[test]
fn duplicate_is_idempotent_but_conflicting_binding_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let mut oracle = open(directory.path());
    let request = request();
    assert_eq!(oracle.ingest(request.clone(), 0).unwrap(), Ingest::New);
    assert_eq!(
        oracle.ingest(request.clone(), 0).unwrap(),
        Ingest::Duplicate
    );
    for field in 0..5 {
        let mut changed = request.clone();
        match field {
            0 => changed.input.push(1),
            1 => changed.public_key = other_key().public_key(),
            2 => changed.context_digest[0] ^= 1,
            3 => changed.not_before += 1,
            _ => changed.expires_at += 1,
        }
        assert!(matches!(oracle.ingest(changed, 0), Err(Error::Conflict)));
    }
    assert!(oracle.prepare(request.id, 9, &key()).unwrap().is_none());
    let attempt = oracle.prepare(request.id, 10, &key()).unwrap().unwrap();
    assert_eq!(
        oracle.ingest(request.clone(), 10).unwrap(),
        Ingest::Duplicate
    );
    assert!(oracle.prepare(request.id, 10, &key()).unwrap().is_none());
    assert!(oracle.confirm_fixture(&attempt, 11).unwrap());
    assert!(!oracle.confirm_fixture(&attempt, 11).unwrap());
    drop(oracle);
    let mut oracle = open(directory.path());
    assert_eq!(
        oracle.ingest(request.clone(), 12).unwrap(),
        Ingest::Duplicate
    );
    assert!(oracle.prepare(request.id, 12, &key()).unwrap().is_none());
    assert_eq!(
        oracle.status(request.id).unwrap(),
        Status::Confirmed { confirmed_at: 11 }
    );
}

#[test]
fn pending_intent_survives_restart_and_never_implies_confirmation() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    let first = oracle.prepare(request.id, 10, &key()).unwrap().unwrap();
    assert_eq!(
        oracle.status(request.id).unwrap(),
        Status::Pending {
            submitted_at: 10,
            retry_at: 15
        }
    );
    drop(oracle);
    let mut oracle = open(directory.path());
    assert_eq!(oracle.pending(request.id).unwrap(), Some(first.clone()));
    assert!(oracle.prepare(request.id, 14, &key()).unwrap().is_none());
    let second = oracle.prepare(request.id, 15, &key()).unwrap().unwrap();
    assert_ne!(first.attempt_id, second.attempt_id);
    assert_eq!(first.binding, second.binding);
    assert_eq!(first.result, second.result);
    assert!(matches!(
        oracle.confirm_fixture(&first, 15),
        Err(Error::Receipt)
    ));
    drop(oracle);
    let mut oracle = open(directory.path());
    assert_eq!(oracle.pending(request.id).unwrap(), Some(second.clone()));
    assert!(oracle.confirm_fixture(&second, 16).unwrap());
}

#[test]
fn retries_are_bounded_across_restarts() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    for tick in [10, 15, 20] {
        assert!(oracle.prepare(request.id, tick, &key()).unwrap().is_some());
        drop(oracle);
        oracle = open(directory.path());
    }
    assert!(oracle.prepare(request.id, 24, &key()).unwrap().is_none());
    assert!(oracle.prepare(request.id, 25, &key()).unwrap().is_none());
    assert_eq!(oracle.status(request.id).unwrap(), Status::Exhausted);
    drop(oracle);
    let mut oracle = open(directory.path());
    assert!(oracle.prepare(request.id, 999, &key()).unwrap().is_none());
    assert_eq!(oracle.status(request.id).unwrap(), Status::Exhausted);
}

#[test]
fn definite_rejection_obeys_backoff_and_attempt_cap() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    for tick in [10, 12, 14] {
        let attempt = oracle.prepare(request.id, tick, &key()).unwrap().unwrap();
        oracle.rejected_fixture(&attempt, tick).unwrap();
        assert!(
            oracle
                .prepare(request.id, tick + 1, &key())
                .unwrap()
                .is_none()
        );
        drop(oracle);
        oracle = open(directory.path());
    }
    assert_eq!(oracle.status(request.id).unwrap(), Status::Exhausted);
}

#[test]
fn expiry_is_terminal_and_cannot_be_confirmed_or_reset_by_duplicate() {
    let directory = tempfile::tempdir().unwrap();
    let mut request = request();
    request.expires_at = 15;
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    let attempt = oracle.prepare(request.id, 10, &key()).unwrap().unwrap();
    assert!(oracle.confirm_fixture(&attempt, 15).is_err());
    assert!(oracle.prepare(request.id, 15, &key()).unwrap().is_none());
    assert_eq!(oracle.status(request.id).unwrap(), Status::Expired);
    assert_eq!(
        oracle.ingest(request.clone(), 16).unwrap(),
        Ingest::Duplicate
    );
    assert!(oracle.confirm_fixture(&attempt, 16).is_err());
    drop(oracle);
    assert_eq!(
        open(directory.path()).status(request.id).unwrap(),
        Status::Expired
    );
}

#[test]
fn already_expired_request_never_generates_a_proof() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 1000).unwrap();
    assert!(oracle.prepare(request.id, 1000, &key()).unwrap().is_none());
    let json = fs::read_to_string(directory.path().join("public-state.json")).unwrap();
    assert!(json.contains("\"attempts\":0"));
    assert!(json.contains("\"result\":null"));
}

#[test]
fn wrong_signer_does_not_consume_attempt_budget() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    assert!(matches!(
        oracle.prepare(request.id, 10, &other_key()),
        Err(Error::Key)
    ));
    assert!(oracle.prepare(request.id, 10, &key()).unwrap().is_some());
}

#[test]
fn confirmation_is_bound_to_request_input_key_proof_output_and_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    let attempt = oracle.prepare(request.id, 10, &key()).unwrap().unwrap();
    let mut changes = Vec::new();
    let mut changed = attempt.clone();
    changed.binding[0] ^= 1;
    changes.push(changed);
    let mut changed = attempt.clone();
    changed.attempt_id.push('0');
    changes.push(changed);
    let mut changed = attempt.clone();
    changed.result.output.replace_range(0..2, "ff");
    changes.push(changed);
    let mut changed = attempt.clone();
    changed.result.proof.replace_range(0..2, "ff");
    changes.push(changed);
    let mut changed = attempt.clone();
    changed.request_id[0] ^= 1;
    changes.push(changed);
    for changed in changes {
        assert!(oracle.confirm_fixture(&changed, 11).is_err());
        assert!(oracle.rejected_fixture(&changed, 11).is_err());
    }
    assert!(matches!(
        oracle.status(request.id).unwrap(),
        Status::Pending { .. }
    ));
    oracle.confirm_fixture(&attempt, 11).unwrap();
}

#[test]
fn clock_cannot_regress_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    oracle.prepare(request.id, 10, &key()).unwrap();
    drop(oracle);
    let mut oracle = open(directory.path());
    assert!(matches!(
        oracle.prepare(request.id, 9, &key()),
        Err(Error::Clock)
    ));
    assert!(matches!(oracle.ingest(request, 9), Err(Error::Clock)));
}

#[test]
fn journal_has_one_writer_and_contains_only_public_state() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    assert!(matches!(
        Oracle::open(directory.path(), metadata(), policy()),
        Err(Error::Locked)
    ));
    oracle.ingest(request.clone(), 0).unwrap();
    oracle.prepare(request.id, 10, &key()).unwrap();
    let state = fs::read_to_string(directory.path().join("public-state.json")).unwrap();
    assert!(!state.contains(&support::Vector::load().public_test_scalar));
    assert!(!state.contains("secret"));
    assert!(state.contains("public_key"));
    drop(oracle);
    open(directory.path());
}

#[test]
fn incompatible_metadata_policy_or_corrupt_state_never_resets_journal() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    oracle.prepare(request.id, 10, &key()).unwrap();
    drop(oracle);
    let path = directory.path().join("public-state.json");
    let saved = fs::read(&path).unwrap();
    let mut changed = metadata();
    changed.application_fixture_digest[0] ^= 1;
    assert!(Oracle::open(directory.path(), changed, policy()).is_err());
    let mut changed = policy();
    changed.max_attempts = 4;
    assert!(Oracle::open(directory.path(), metadata(), changed).is_err());
    assert_eq!(fs::read(&path).unwrap(), saved);
    for mutate in 0..4 {
        let mut value: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        let id = hex::encode(request.id);
        match mutate {
            0 => value["records"][&id]["result"]["proof"] = "00".into(),
            1 => value["records"][&id]["request"]["input"][0] = 255.into(),
            2 => value["records"][&id]["attempts"] = 0.into(),
            _ => value["unexpected_field"] = true.into(),
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(Oracle::open(directory.path(), metadata(), policy()).is_err());
    }
    fs::write(&path, b"{").unwrap();
    assert!(Oracle::open(directory.path(), metadata(), policy()).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{");
    fs::write(&path, saved).unwrap();
    assert!(
        open(directory.path())
            .pending(request.id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn missing_snapshot_never_resets_attempt_budget() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    oracle.prepare(request.id, 10, &key()).unwrap();
    drop(oracle);
    fs::remove_file(directory.path().join("public-state.json")).unwrap();
    assert!(matches!(
        Oracle::open(directory.path(), metadata(), policy()),
        Err(Error::Journal)
    ));
    assert!(!directory.path().join("public-state.json").exists());
}

#[test]
fn orphan_temporary_file_does_not_override_durable_pending_intent() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    let intent = oracle.prepare(request.id, 10, &key()).unwrap().unwrap();
    drop(oracle);
    fs::write(
        directory.path().join(".tmp-interrupted"),
        b"incomplete snapshot",
    )
    .unwrap();
    assert_eq!(
        open(directory.path()).pending(request.id).unwrap(),
        Some(intent)
    );
}

#[test]
fn persistence_failure_returns_no_intent_and_requires_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let mut oracle = open(directory.path());
    oracle.ingest(request.clone(), 0).unwrap();
    let path = directory.path().join("public-state.json");
    let saved = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(oracle.prepare(request.id, 10, &key()).is_err());
    assert!(matches!(
        oracle.prepare(request.id, 10, &key()),
        Err(Error::Poisoned)
    ));
    assert!(matches!(oracle.status(request.id), Err(Error::Poisoned)));
    drop(oracle);
    fs::remove_dir(&path).unwrap();
    fs::write(&path, saved).unwrap();
    let mut oracle = open(directory.path());
    assert!(oracle.pending(request.id).unwrap().is_none());
    assert!(oracle.prepare(request.id, 10, &key()).unwrap().is_some());
}

#[test]
fn policy_rejects_unbounded_or_zero_retry_parameters() {
    let directory = tempfile::tempdir().unwrap();
    for field in 0..4 {
        let mut changed = policy();
        match field {
            0 => changed.max_attempts = 0,
            1 => changed.max_attempts = 17,
            2 => changed.retry_delay = 0,
            _ => changed.pending_timeout = u64::MAX,
        }
        assert!(Oracle::open(directory.path(), metadata(), changed).is_err());
    }
}
