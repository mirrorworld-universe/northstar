use {
    super::*,
    crate::continuity_v2::TransitionPayloadV2,
    northstar_zk_types::continuity::{IngressPayloadV2, IngressRecordV2},
};

fn identity() -> SessionIdentityV2 {
    SessionIdentityV2 {
        version: 2,
        l1_genesis: [1; 32],
        portal: [2; 32],
        session: [3; 32],
        nonce: 1,
        grid_id: 0,
        validator: [4; 32],
        profile: [5; 32],
    }
}

fn transition(state: &CommittedStateV2) -> RecordedTransitionV2 {
    let before = state.opening().unwrap();
    let count = state.ingress_count();
    let record = IngressRecordV2::new(
        before.session_context,
        count + 1,
        10,
        state.ingress_root(),
        IngressPayloadV2::SolDeposit {
            recipient: [20; 32],
            receipt: [21; 32],
            amount: 10,
            cumulative_deposited: (count + 1) * 10,
        },
        1,
        hash_parts,
    )
    .unwrap();
    let mut next = state.clone();
    next.apply_ingress(&record, &[]).unwrap();
    RecordedTransitionV2 {
        er_slot: count + 1,
        before,
        after: next.opening().unwrap(),
        payload: TransitionPayloadV2::Ingress {
            record,
            account_data: vec![],
        },
        readonly_l1_values: vec![],
        settlement_effects: vec![],
    }
}

#[test]
fn regression_lineage_store_v2_persistence_failure_requires_reopen() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    let stable = store.working_state().unwrap();
    let snapshot = directory.join(CAPSULE_FILE);
    let retained = directory.join("retained.borsh");
    std::fs::rename(&snapshot, &retained).unwrap();
    std::fs::create_dir(&snapshot).unwrap();
    let item = transition(&stable);
    assert!(store.append(item.clone()).is_err());
    assert!(store.is_poisoned());
    assert_eq!(store.working_state().unwrap(), stable);
    std::fs::remove_dir(&snapshot).unwrap();
    std::fs::rename(&retained, &snapshot).unwrap();
    assert!(store.append(item.clone()).is_err());
    assert_eq!(store.working_state().unwrap(), stable);
    drop(store);
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
            .unwrap();
    assert!(!store.is_poisoned());
    assert_eq!(store.working_state().unwrap(), stable);
    store.append(item).unwrap();
}

#[test]
fn regression_lineage_store_v2_preserves_the_accepted_slot_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    store
        .append(transition(&store.working_state().unwrap()))
        .unwrap();
    let artifact = crate::checkpoint_v2::build_checkpoint_artifact_v2(
        store.accepted_state(),
        100,
        store.unsealed(),
    )
    .unwrap();
    store
        .seal([70; 32], 100, borsh::to_vec(&artifact).unwrap())
        .unwrap();
    let head = L1CheckpointHeadV2 {
        checkpoint: [70; 32],
        er_slot: 100,
        state_root: store.working_state().unwrap().root().unwrap(),
        checkpoint_hash: artifact.binding_hash().unwrap(),
    };
    store.record_accepted(head.clone()).unwrap();
    store.record_settled(&head).unwrap();
    drop(store);
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), Some(head), StartupModeV2::Resume)
            .unwrap();
    let stable = store.working_state().unwrap();
    for slot in [2, 99, 100] {
        let mut next = transition(&stable);
        next.er_slot = slot;
        assert_eq!(
            store.append(next).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
    assert_eq!(store.working_state().unwrap(), stable);
    assert!(store.unsealed().is_empty());
    let mut next = transition(&stable);
    next.er_slot = 101;
    store.append(next).unwrap();
}

#[test]
fn regression_lineage_store_v2_rejects_unsealable_transitions_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    let state = store.working_state().unwrap();
    let opening = state.opening().unwrap();
    let item = RecordedTransitionV2 {
        er_slot: 1,
        before: opening,
        after: opening,
        payload: TransitionPayloadV2::Execution {
            transaction: vec![1],
            pre: vec![],
            post: vec![],
            execution_result: vec![],
            replay: vec![0; crate::checkpoint::MAX_TRANSACTION_EFFECT_BYTES_V1],
        },
        readonly_l1_values: vec![],
        settlement_effects: vec![],
    };
    item.apply(&mut state.clone()).unwrap();
    assert_eq!(
        store.append(item).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(store.working_state().unwrap(), state);
    assert!(store.unsealed().is_empty());
    drop(store);
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
            .unwrap();
    assert_eq!(store.working_state().unwrap(), state);
    store.append(transition(&state)).unwrap();
}

#[test]
fn regression_lineage_store_v2_recovers_an_unsealed_tail() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    let item = transition(&store.working_state().unwrap());
    store.append(item.clone()).unwrap();
    let state = store.working_state().unwrap();
    drop(store);
    let store = CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
        .unwrap();
    assert_eq!(store.working_state().unwrap(), state);
    assert_eq!(store.unsealed(), &[item]);
    assert!(
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
            .is_err()
    );
}

#[test]
fn regression_lineage_store_v2_reconciles_acceptance_without_resigning() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    store
        .append(transition(&store.working_state().unwrap()))
        .unwrap();
    let artifact = crate::checkpoint_v2::build_checkpoint_artifact_v2(
        store.accepted_state(),
        100,
        store.unsealed(),
    )
    .unwrap();
    store
        .seal([70; 32], 100, borsh::to_vec(&artifact).unwrap())
        .unwrap();
    assert!(store
        .append(transition(&store.working_state().unwrap()))
        .is_err());
    let head = L1CheckpointHeadV2 {
        checkpoint: [70; 32],
        er_slot: 100,
        state_root: store.working_state().unwrap().root().unwrap(),
        checkpoint_hash: artifact.binding_hash().unwrap(),
    };
    drop(store);
    let mut store = CheckpointLineageStoreV2::open(
        &directory,
        identity(),
        Some(head.clone()),
        StartupModeV2::Resume,
    )
    .unwrap();
    assert_eq!(store.accepted_head(), Some(&head));
    assert!(store.sealed().unwrap().accepted);
    store.record_accepted(head.clone()).unwrap();
    store.record_settled(&head).unwrap();
    let prior = store.working_state().unwrap();
    assert!(store.sealed().is_none());
    let mut next = transition(&prior);
    next.er_slot = 101;
    store.append(next).unwrap();
    assert_ne!(
        store.working_state().unwrap().root().unwrap(),
        head.state_root
    );
    drop(store);
    assert!(
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
            .is_err()
    );
    let recovered =
        CheckpointLineageStoreV2::open(&directory, identity(), Some(head), StartupModeV2::Resume)
            .unwrap();
    assert_eq!(recovered.unsealed().len(), 1);
}

#[test]
fn regression_lineage_store_v2_rejects_missing_or_changed_established_inputs() {
    for variant in 0..4 {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let store = CheckpointLineageStoreV2::open(
            &directory,
            identity(),
            None,
            StartupModeV2::InitializeNew,
        )
        .unwrap();
        drop(store);
        let mut expected = identity();
        match variant {
            0 => fs::remove_file(directory.join(CAPSULE_FILE)).unwrap(),
            1 => fs::remove_file(directory.join(MARKER_FILE)).unwrap(),
            2 => fs::write(directory.join(CAPSULE_FILE), b"partial").unwrap(),
            _ => expected.nonce += 1,
        }
        assert!(
            CheckpointLineageStoreV2::open(&directory, expected, None, StartupModeV2::Resume)
                .is_err()
        );
        assert!(CheckpointLineageStoreV2::open(
            &directory,
            expected,
            None,
            StartupModeV2::InitializeNew
        )
        .is_err());
    }
    let root = tempfile::tempdir().unwrap();
    let absent = root.path().join("absent");
    assert!(
        CheckpointLineageStoreV2::open(&absent, identity(), None, StartupModeV2::Resume).is_err()
    );
    assert!(!absent.exists());
}

#[test]
fn regression_lineage_store_v2_rejects_unrelated_l1_acceptance() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    store
        .append(transition(&store.working_state().unwrap()))
        .unwrap();
    let artifact = crate::checkpoint_v2::build_checkpoint_artifact_v2(
        store.accepted_state(),
        100,
        store.unsealed(),
    )
    .unwrap();
    store
        .seal([70; 32], 100, borsh::to_vec(&artifact).unwrap())
        .unwrap();
    let head = L1CheckpointHeadV2 {
        checkpoint: [71; 32],
        er_slot: 100,
        state_root: store.working_state().unwrap().root().unwrap(),
        checkpoint_hash: artifact.binding_hash().unwrap(),
    };
    assert!(store.record_accepted(head).is_err());
    assert!(!store.sealed().unwrap().accepted);
}

#[test]
fn regression_lineage_store_v2_rejects_symlink_and_nonregular_inputs() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    drop(store);
    fs::rename(directory.join(CAPSULE_FILE), directory.join("original")).unwrap();
    symlink(directory.join("original"), directory.join(CAPSULE_FILE)).unwrap();
    assert!(
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
            .is_err()
    );
    fs::remove_file(directory.join(CAPSULE_FILE)).unwrap();
    fs::create_dir(directory.join(CAPSULE_FILE)).unwrap();
    assert!(
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
            .is_err()
    );
}

#[test]
fn regression_lineage_store_v2_rejects_a_disconnected_transition() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    let mut item = transition(&store.working_state().unwrap());
    item.before.ingress_count = 8;
    let state = store.working_state().unwrap();
    assert!(store.append(item).is_err());
    assert_eq!(store.working_state().unwrap(), state);
    drop(store);
    let store = CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
        .unwrap();
    assert_eq!(store.working_state().unwrap(), state);
}
