use {
    super::*,
    crate::withdrawal_audit_v2::reconstruction::{
        audit_from_full_snapshot, ordered_cut_digest, ReplayPins, MAX_PREFIX_TRANSACTIONS,
    },
    agave_snapshots::{
        snapshot_archive_info::SnapshotArchiveInfoGetter, snapshot_config::SnapshotConfig,
    },
    solana_runtime::snapshot_bank_utils::bank_to_full_snapshot_archive,
};

pub(super) fn check(
    genesis: &GenesisConfig,
    base: Bank,
    prefix: &[Vec<u8>],
    artifact: &CheckpointArtifactV2,
    pins: &AuditPins,
    reference: &VerifiedStep,
) {
    let archive_dir = tempfile::tempdir().unwrap();
    let bank_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let config = SnapshotConfig {
        bank_snapshots_dir: bank_dir.path().into(),
        full_snapshot_archives_dir: archive_dir.path().into(),
        use_direct_io: false,
        use_registered_io_uring_buffers: false,
        ..SnapshotConfig::default()
    };
    let earlier_hash = base.hash().to_bytes();
    let archive = bank_to_full_snapshot_archive(&config, &base).unwrap();
    let archive_bytes = std::fs::read(archive.path()).unwrap();
    let TransitionPayloadV2::Execution {
        transaction: target,
        ..
    } = &artifact.pages[0].transition.payload
    else {
        unreachable!()
    };
    let replay = ReplayPins {
        genesis_hash: genesis.hash().to_bytes(),
        archive_bytes: archive_bytes.len() as u64,
        archive_sha256: hash_parts(&[&archive_bytes]),
        parent_slot: base.slot(),
        parent_bank_hash: earlier_hash,
        ordered_cut: ordered_cut_digest(prefix, target).unwrap(),
    };
    drop(base);
    let run = |rows: &[Vec<u8>], rp: &ReplayPins, ap: &AuditPins| {
        audit_from_full_snapshot(
            genesis,
            archive.path(),
            scratch.path(),
            rows,
            rp,
            artifact,
            ap,
        )
    };
    assert_eq!(&run(prefix, &replay, pins).unwrap(), reference);
    let mut reversed = prefix.to_vec();
    reversed.reverse();
    for other in [&reversed[..], &prefix[1..]] {
        assert_eq!(
            run(other, &replay, pins),
            Err(Unavailable(
                "ordered prefix or target does not match cut pin"
            ))
        );
    }
    // A digest consistent with the shortened list still cannot open the independent bank pin.
    assert_eq!(
        run(
            &prefix[1..],
            &ReplayPins {
                ordered_cut: ordered_cut_digest(&prefix[1..], target).unwrap(),
                ..replay
            },
            pins
        ),
        Err(Unavailable("frozen bank slot/hash does not match pin"))
    );
    let repinned = |rows: &[Vec<u8>]| {
        run(
            rows,
            &ReplayPins {
                ordered_cut: ordered_cut_digest(rows, target).unwrap(),
                ..replay
            },
            pins,
        )
    };
    for rows in [
        vec![target.clone()],
        vec![prefix[0].clone(), prefix[0].clone()],
    ] {
        assert_eq!(
            repinned(&rows),
            Err(Unavailable(
                "prefix repeats a signature or includes the target"
            ))
        );
    }
    let mut bad_sig = signed_transaction(&prefix[0]).unwrap();
    bad_sig.signatures[0] = Signature::default();
    assert_eq!(
        repinned(&[bincode::serialize(&bad_sig).unwrap()]),
        Err(Unavailable("invalid transaction signatures"))
    );
    assert_eq!(
        run(
            prefix,
            &ReplayPins {
                parent_bank_hash: [0; 32],
                ..replay
            },
            pins
        ),
        Err(Unavailable("restored parent does not match bank pin"))
    );
    assert_eq!(
        run(
            prefix,
            &ReplayPins {
                archive_sha256: [0; 32],
                ..replay
            },
            pins
        ),
        Err(Unavailable("full archive digest does not match pin"))
    );
    assert_eq!(
        run(
            prefix,
            &ReplayPins {
                archive_bytes: replay.archive_bytes - 1,
                ..replay
            },
            pins
        ),
        Err(Unavailable("full archive size does not match pin"))
    );
    assert_eq!(
        run(
            prefix,
            &ReplayPins {
                genesis_hash: [0; 32],
                ..replay
            },
            pins
        ),
        Err(Unavailable("genesis or selected cut does not match pins"))
    );
    assert!(scratch.path().read_dir().unwrap().next().is_none());
    assert_eq!(std::fs::read(archive.path()).unwrap(), archive_bytes);
}

#[test]
fn invariant_v2_cut_digest_frames_count_order_and_target() {
    let rows = vec![vec![1], vec![2, 3]];
    let expected = hash_parts(&[
        b"northstar:v2:cpi-audit:first-er-slot:cut:v1",
        &[2, 0, 0, 0, 1, 0, 0, 0, 1, 2, 0, 0, 0, 2, 3, 1, 0, 0, 0, 4],
    ]);
    assert_eq!(ordered_cut_digest(&rows, &[4]), Ok(expected));
    assert_ne!(
        ordered_cut_digest(&[vec![1, 2], vec![3]], &[4]),
        Ok(expected)
    );
    assert_ne!(ordered_cut_digest(&rows, &[5]), Ok(expected));
    assert!(ordered_cut_digest(&[], &[]).is_err());
    assert!(ordered_cut_digest(&rows, &vec![0; MAX_TRANSACTION_BYTES_V1 + 1]).is_err());
    assert!(ordered_cut_digest(&vec![vec![1]; MAX_PREFIX_TRANSACTIONS + 1], &[4]).is_err());
}
