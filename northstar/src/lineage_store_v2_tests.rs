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
fn invariant_lineage_lock_releases_when_owner_drops_with_an_inherited_descriptor() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    let inherited = store._lock.0.try_clone().unwrap();
    assert!(
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume,)
            .is_err()
    );
    drop(store);
    let resumed =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
            .unwrap();
    assert_eq!(resumed.accepted_state().identity(), &identity());
    drop(inherited);
    assert!(
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume,)
            .is_err()
    );
}

#[test]
fn regression_lineage_store_v2_settlement_plan_is_durable_and_immutable() {
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
        10,
        store.unsealed(),
    )
    .unwrap();
    store
        .seal([70; 32], 10, borsh::to_vec(&artifact).unwrap())
        .unwrap();
    let mut plan = crate::settlement::SettlementPlan {
        er_slot: 10,
        checksum: [0; 32],
        chunks: vec![],
        owner_changes: vec![],
        lamport_changes: vec![],
        receipt_balances: vec![],
        token_withdrawals: vec![],
        unsupported_changes: vec![],
    };
    plan.checksum = plan.recomputed_checksum();
    store.bind_settlement_plan(&plan).unwrap();
    store.bind_settlement_plan(&plan).unwrap();
    drop(store);
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
            .unwrap();
    assert_eq!(store.settlement_plan().unwrap(), Some(plan.clone()));
    let mut changed = plan.clone();
    changed
        .owner_changes
        .push(crate::settlement::AccountOwnerSettlement {
            account: solana_pubkey::Pubkey::new_unique(),
            owner: solana_pubkey::Pubkey::new_unique(),
        });
    changed.checksum = changed.recomputed_checksum();
    assert!(store.bind_settlement_plan(&changed).is_err());
    changed.er_slot += 1;
    assert!(store.bind_settlement_plan(&changed).is_err());
    assert_eq!(store.settlement_plan().unwrap(), Some(plan));
}

#[test]
fn invariant_v2_reopen_rechecks_entitlement_in_a_preexisting_capsule() {
    use solana_pubkey::Pubkey;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    store.append(transition(store.accepted_state())).unwrap();
    let artifact =
        build_checkpoint_artifact_v2(store.accepted_state(), 1, store.unsealed()).unwrap();
    store
        .seal([70; 32], 1, borsh::to_vec(&artifact).unwrap())
        .unwrap();
    let bridge = Pubkey::new_unique();
    let registry = Pubkey::new_unique();
    let mut plan = crate::settlement::SettlementPlan {
        er_slot: 1,
        checksum: [0; 32],
        chunks: vec![],
        owner_changes: vec![],
        lamport_changes: vec![],
        receipt_balances: vec![],
        token_withdrawals: vec![crate::settlement::TokenWithdrawalSettlement {
            bridge_program: bridge,
            session_bridge: registry,
            er_token_account: Pubkey::new_unique(),
            vault: northstar_token_bridge::find_token_vault_pda(&bridge, &registry).0,
            vault_token_account: Pubkey::new_unique(),
            l1_destination_token_account: Pubkey::new_unique(),
            mint: Pubkey::new_unique(),
            token_program: spl_token_interface::id(),
            amount: 1,
            withdrawn: 1,
            decimals: 6,
        }],
        unsupported_changes: vec![],
    };
    plan.checksum = plan.recomputed_checksum();
    // Exercise the reader for an older, self-consistent capsule: the artifact has no token effects.
    let mut retained = store.capsule.clone();
    retained.sealed.as_mut().unwrap().settlement_plan =
        Some(borsh::to_vec(&crate::DurableSettlementPlan::from(&plan)).unwrap());
    store.install(&retained).unwrap();
    drop(store);
    let before = std::fs::read(directory.join(CAPSULE_FILE)).unwrap();
    let error = CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::Resume)
        .err()
        .expect("unmatched plan must not resume");
    assert!(
        error.to_string().contains("authorized occurrences"),
        "{error}"
    );
    assert_eq!(std::fs::read(directory.join(CAPSULE_FILE)).unwrap(), before);
}

#[test]
fn regression_v2_retains_successor_while_only_accepting_sealed_prefix() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&directory, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    store.append(transition(store.accepted_state())).unwrap();
    let artifact =
        build_checkpoint_artifact_v2(store.accepted_state(), 1, store.unsealed()).unwrap();
    store
        .seal([70; 32], 1, borsh::to_vec(&artifact).unwrap())
        .unwrap();
    let head = L1CheckpointHeadV2 {
        checkpoint: [70; 32],
        er_slot: 1,
        state_root: artifact.checkpoint.new_state_root,
        checkpoint_hash: artifact.binding_hash().unwrap(),
    };
    let next = transition(&store.working_state().unwrap());
    assert_eq!(
        store.append(next.clone()).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let mut plan = crate::settlement::SettlementPlan {
        er_slot: 1,
        checksum: [0; 32],
        chunks: vec![],
        owner_changes: vec![],
        lamport_changes: vec![],
        receipt_balances: vec![],
        token_withdrawals: vec![],
        unsupported_changes: vec![],
    };
    plan.checksum = plan.recomputed_checksum();
    store.bind_settlement_plan(&plan).unwrap();
    let mut same_slot = next.clone();
    same_slot.er_slot = 1;
    assert!(store.append(same_slot).is_err());
    store
        .append(next)
        .expect("durably record successor without waiting for settlement");
    let successor = store.working_state().unwrap();
    assert_eq!(successor.ingress_count(), 2);
    assert!(store.accepted_head().is_none());
    let mut wrong = head.clone();
    wrong.state_root = successor.root().unwrap();
    assert!(store.record_accepted(wrong).is_err());
    drop(store);

    let mut store = CheckpointLineageStoreV2::open(
        &directory,
        identity(),
        Some(head.clone()),
        StartupModeV2::Resume,
    )
    .unwrap();
    assert_eq!(store.accepted_state().ingress_count(), 1);
    assert_eq!(store.working_state().unwrap(), successor);
    assert_eq!(store.settlement_plan().unwrap(), Some(plan));
    assert_eq!(store.pending_transitions().len(), 1);
    let second = build_checkpoint_artifact_v2(store.accepted_state(), 2, store.unsealed()).unwrap();
    assert!(store
        .seal([71; 32], 2, borsh::to_vec(&second).unwrap())
        .is_err());
    store.record_settled(&head).unwrap();
    assert_eq!(store.working_state().unwrap(), successor);
    store
        .seal([71; 32], 2, borsh::to_vec(&second).unwrap())
        .unwrap();
    assert_eq!(second.pre, artifact.post);
    assert_eq!(store.accepted_head(), Some(&head));
}

#[test]
fn regression_lineage_store_v2_plan_persistence_failure_prevents_publication() {
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
        10,
        store.unsealed(),
    )
    .unwrap();
    store
        .seal([70; 32], 10, borsh::to_vec(&artifact).unwrap())
        .unwrap();
    let mut plan = crate::settlement::SettlementPlan {
        er_slot: 10,
        checksum: [0; 32],
        chunks: vec![],
        owner_changes: vec![],
        lamport_changes: vec![],
        receipt_balances: vec![],
        token_withdrawals: vec![],
        unsupported_changes: vec![],
    };
    plan.checksum = plan.recomputed_checksum();
    let snapshot = directory.join(CAPSULE_FILE);
    std::fs::rename(&snapshot, directory.join("retained.borsh")).unwrap();
    std::fs::create_dir(&snapshot).unwrap();
    assert!(store.bind_settlement_plan(&plan).is_err());
    assert!(store.is_poisoned());
    assert!(store.sealed().unwrap().settlement_plan.is_none());
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
            transaction: crate::continuity_v2::test_execution_wire(1).0,
            pre: vec![],
            post: vec![],
            execution_result: crate::continuity_v2::test_execution_wire(1).1,
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
