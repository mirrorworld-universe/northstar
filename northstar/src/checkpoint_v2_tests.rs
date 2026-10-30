use {
    super::*,
    northstar_zk_types::continuity::{IngressPayloadV2, IngressRecordV2, SessionIdentityV2},
};

fn state() -> CommittedStateV2 {
    CommittedStateV2::new(SessionIdentityV2 {
        version: 2,
        l1_genesis: [1; 32],
        portal: [2; 32],
        session: [3; 32],
        nonce: 1,
        grid_id: 0,
        validator: [4; 32],
        profile: [5; 32],
    })
    .unwrap()
}

fn ingress(state: &CommittedStateV2, payload: IngressPayloadV2, slot: u64) -> RecordedTransitionV2 {
    let before = state.opening().unwrap();
    let record = IngressRecordV2::new(
        before.session_context,
        state.ingress_count() + 1,
        10,
        state.ingress_root(),
        payload,
        1,
        hash_parts,
    )
    .unwrap();
    let mut next = state.clone();
    next.apply_ingress(&record, &[]).unwrap();
    RecordedTransitionV2 {
        er_slot: slot,
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
fn regression_v2_checkpoint_boundaries_include_late_delegation() {
    let genesis = state();
    let first = ingress(
        &genesis,
        IngressPayloadV2::SolDeposit {
            recipient: [20; 32],
            receipt: [21; 32],
            amount: 100_000_000,
            cumulative_deposited: 100_000_000,
        },
        100,
    );
    let artifact =
        build_checkpoint_artifact_v2(&genesis, 100, std::slice::from_ref(&first)).unwrap();
    artifact.validate().unwrap();
    let mut accepted = genesis;
    first.apply(&mut accepted).unwrap();
    let late = ingress(
        &accepted,
        IngressPayloadV2::Delegation {
            account: [30; 32],
            record: [31; 32],
            owner: [0; 32],
            lamports: 100_000_000,
            data_len: 0,
            data_hash: hash_parts(&[&[]]),
            executable: false,
            rent_epoch: 0,
        },
        101,
    );
    let second = build_checkpoint_artifact_v2(&accepted, 101, &[late]).unwrap();
    second.validate().unwrap();
    assert_eq!(
        second.checkpoint.previous_state_root,
        artifact.checkpoint.new_state_root
    );
    assert_ne!(
        second.checkpoint.new_state_root,
        artifact.checkpoint.new_state_root
    );
    assert_eq!(second.pre.ingress_count, 1);
    assert_eq!(second.post.ingress_count, 2);
    assert_eq!(second.checkpoint.version, 2);
    assert_ne!(second.checkpoint.readonly_l1_root, second.readonly_l1_root);
}

#[test]
fn regression_v2_checkpoint_batches_require_ordered_bounded_transitions() {
    let genesis = state();
    assert!(matches!(
        build_checkpoint_artifact_v2(&genesis, 100, &[]),
        Err(ArtifactErrorV2::InvalidBatch)
    ));
    let first = ingress(
        &genesis,
        IngressPayloadV2::SolDeposit {
            recipient: [20; 32],
            receipt: [21; 32],
            amount: 100,
            cumulative_deposited: 100,
        },
        100,
    );
    assert!(matches!(
        build_checkpoint_artifact_v2(&genesis, 99, std::slice::from_ref(&first)),
        Err(ArtifactErrorV2::InvalidBatch)
    ));
    assert!(matches!(
        build_checkpoint_artifact_v2(
            &genesis,
            100,
            &vec![first.clone(); MAX_CHECKPOINT_TRANSITIONS_V2 + 1]
        ),
        Err(ArtifactErrorV2::InvalidBatch)
    ));
    let mut next = genesis.clone();
    first.apply(&mut next).unwrap();
    let second = ingress(
        &next,
        IngressPayloadV2::SolDeposit {
            recipient: [20; 32],
            receipt: [21; 32],
            amount: 50,
            cumulative_deposited: 150,
        },
        99,
    );
    assert!(matches!(
        build_checkpoint_artifact_v2(&genesis, 100, &[first.clone(), second]),
        Err(ArtifactErrorV2::InvalidBatch)
    ));
    assert!(matches!(
        build_checkpoint_artifact_v2(&genesis, 100, &[first.clone(), first]),
        Err(ArtifactErrorV2::State(_))
    ));
}

#[test]
fn regression_v2_da_mutations_do_not_validate() {
    let genesis = state();
    let first = ingress(
        &genesis,
        IngressPayloadV2::SolDeposit {
            recipient: [20; 32],
            receipt: [21; 32],
            amount: 100,
            cumulative_deposited: 100,
        },
        100,
    );
    let artifact = build_checkpoint_artifact_v2(&genesis, 100, &[first]).unwrap();
    for variant in 0..5 {
        let mut changed = artifact.clone();
        match variant {
            0 => changed.checkpoint.previous_state_root[0] ^= 1,
            1 => changed.checkpoint.da_commitment[0] ^= 1,
            2 => changed.pages[0].effect_leaf[0] ^= 1,
            3 => changed.pages[0].post_state_path.leaf_index = 0,
            _ => changed.post.ingress_count += 1,
        }
        assert!(changed.validate().is_err());
    }
}
