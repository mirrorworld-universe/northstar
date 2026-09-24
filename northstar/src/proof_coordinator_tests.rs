use {super::*, solana_account::Account};

const PORTAL: Pubkey = solana_pubkey::pubkey!("5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf");

fn config(directory: &Path) -> CoordinatorConfig {
    let prover = directory.join("prover");
    fs::write(&prover, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&prover, fs::Permissions::from_mode(0o700)).unwrap();
    CoordinatorConfig {
        directory: directory.join("state"),
        prover,
        challenger: Arc::new(Keypair::new()),
        challenge_window_slots: 750,
    }
}

fn capture() -> (Session, CheckpointArtifactV1, Arc<ErHistoryStore>) {
    let (_, _, accounts): (u64, u64, Vec<(Pubkey, Account)>) = bincode::deserialize(
        include_bytes!("../zkvm-replay/evidence/resolver-v1/resolver-fixture.bin"),
    )
    .unwrap();
    let session = Session::try_from_slice(&accounts[0].1.data).unwrap();
    let (artifact, history) =
        crate::ephemeral_tx_client::tests::supported_sbf_checkpoint_with_steps(
            0,
            northstar_portal::find_session_pda(&PORTAL).0,
            1,
        );
    (session, artifact, history)
}

fn wait(mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "coordinator condition timed out"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn durable_preparation_gates_admission_and_survives_restart_without_history() {
    let directory = tempfile::tempdir().unwrap();
    let (session, artifact, history) = capture();
    let coordinator = ProofCoordinator::start(config(directory.path()), PORTAL).unwrap();
    assert!(!coordinator.prepare(artifact.clone(), session, history.clone(), 0));
    wait(|| coordinator.prepare(artifact.clone(), session, history.clone(), 0));
    assert!(ProofCoordinator::start(config(directory.path()), PORTAL).is_err());
    drop(coordinator);
    let store = ProofJobStore::open(&directory.path().join("state/jobs")).unwrap();
    let jobs = store.ids().unwrap();
    assert_eq!(jobs.len(), 1);
    let job = store.load(&jobs[0]).unwrap();
    assert_eq!(
        northstar_transaction_proof::public_inputs_bytes(
            northstar_transaction_proof::replay(&decode_witness(&job.witness).unwrap()).unwrap()
        ),
        job.binding.public_inputs
    );
    drop(store);
    let coordinator = ProofCoordinator::start(config(directory.path()), PORTAL).unwrap();
    let empty_history = Arc::new(ErHistoryStore::new(10));
    wait(|| coordinator.prepare(artifact.clone(), session, empty_history.clone(), 0));
    assert!(coordinator.take_transaction().is_none());
}

#[test]
fn completed_proof_resumes_without_starting_an_unavailable_worker() {
    use northstar_portal::StepProofAccount;
    agave_logger::setup_with_default("northstar=info,solana_runtime=error");
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path());
    fs::write(&config.prover, b"#!/bin/sh\ntouch \"$0.called\"\nexit 73\n").unwrap();
    let called = config.prover.with_extension("called");
    let challenger = config.challenger.pubkey();
    let (slot, _, mut accounts): (u64, u64, Vec<(Pubkey, Account)>) = bincode::deserialize(
        include_bytes!("../zkvm-replay/evidence/proof-coordinator-v1/proving/resolver-fixture.bin"),
    )
    .unwrap();
    let session = Session::try_from_slice(&accounts[0].1.data).unwrap();
    let mut checkpoint = Checkpoint::try_from_slice(&accounts[1].1.data).unwrap();
    checkpoint.challenger = challenger;
    accounts[1].1.data = borsh::to_vec(&checkpoint).unwrap();
    let mut challenge = Challenge::try_from_slice(&accounts[2].1.data).unwrap();
    challenge.challenger = challenger;
    accounts[2].1.data = borsh::to_vec(&challenge).unwrap();
    let mut upload = StepProofAccount::try_from_slice(&accounts[4].1.data).unwrap();
    upload.authority = challenger;
    accounts[4].1.data = borsh::to_vec(&upload).unwrap();
    accounts[5].0 = challenger;
    let genesis = solana_runtime::genesis_utils::create_genesis_config(1_000_000);
    let bank = Arc::new(Bank::new_from_parent(
        Arc::new(Bank::new_for_tests(&genesis.genesis_config)),
        solana_leader_schedule::SlotLeader::default(),
        slot,
    ));
    for (key, account) in accounts {
        bank.store_account(&key, &solana_account::AccountSharedData::from(account));
    }
    let artifact = CheckpointArtifactV1::decode_verified(include_bytes!(
        "../zkvm-replay/evidence/proof-coordinator-v1/proving/checkpoint-artifact.borsh"
    ))
    .unwrap();
    let binding = expected_binding(PORTAL, &session, &artifact, 0).unwrap();
    let store = ProofJobStore::open(&config.directory.join("jobs")).unwrap();
    let id = store
        .enqueue(
            binding.clone(),
            include_bytes!("../zkvm-replay/evidence/proof-coordinator-v1/proving/witness-v2.bin"),
        )
        .unwrap();
    store
        .complete(
            &id,
            include_bytes!(
                "../zkvm-replay/evidence/proof-coordinator-v1/proving/\
                 northstar-sp1-groth16-onchain.bin"
            ),
            &binding.public_inputs,
        )
        .unwrap();
    let record = Prepared {
        version: 1,
        prepared_after_slot: 0,
        session,
        artifact,
        jobs: vec![id],
    };
    fs::DirBuilder::new()
        .mode(0o700)
        .create(config.directory.join("prepared"))
        .unwrap();
    let body = borsh::to_vec(&record).unwrap();
    let mut encoded = hashv(&[&body]).to_bytes().to_vec();
    encoded.extend(body);
    atomic(
        &config
            .directory
            .join("prepared")
            .join(name(&key(PORTAL, &session, &record.artifact))),
        &encoded,
    )
    .unwrap();
    drop(store);
    let coordinator = ProofCoordinator::start(config, PORTAL).unwrap();
    coordinator.update_banks(bank.clone(), bank);
    wait(|| {
        assert!(
            !coordinator.thread.as_ref().unwrap().is_finished(),
            "coordinator stopped before cached upload"
        );
        assert!(
            !called.exists(),
            "cached upload must not require GPU preflight"
        );
        let Some(transaction) = coordinator.take_transaction() else {
            return false;
        };
        assert_eq!(transaction.message.account_keys[0], challenger);
        assert!(matches!(
            northstar_portal::PortalInstruction::try_from_slice(
                &transaction.message.instructions[0].data
            ),
            Ok(northstar_portal::PortalInstruction::ResolveChallenge(_))
        ));
        true
    });
}

#[test]
fn preparation_origin_distinguishes_session_reopening_without_changing_public_inputs() {
    let (session, artifact, _) = capture();
    let original = key(PORTAL, &session, &artifact);
    let mut progressed = session;
    progressed.last_settled_l1_slot += 1;
    assert_eq!(key(PORTAL, &progressed, &artifact), original);
    let mut reopened = session;
    reopened.created_at += 1;
    assert_ne!(key(PORTAL, &reopened, &artifact), original);
    assert_eq!(
        expected_binding(PORTAL, &session, &artifact, 0).unwrap(),
        expected_binding(PORTAL, &reopened, &artifact, 0).unwrap()
    );
}

#[test]
fn fresh_preparation_requires_a_new_readiness_check() {
    let (session, artifact, history) = capture();
    let coordinator = ProofCoordinator {
        shared: Arc::new(Mutex::new(Shared {
            ready: true,
            ..Shared::default()
        })),
        stop: Arc::new(AtomicBool::new(false)),
        thread: None,
        portal: PORTAL,
        challenge_window_slots: 750,
    };
    assert!(!coordinator.prepare(artifact, session, history, 17));
    let state = coordinator.shared.lock().unwrap();
    assert!(!state.ready && state.needs_readiness);
    assert_eq!(state.preparation.as_ref().unwrap().prepared_after_slot, 17);
}

#[test]
fn invalid_challenge_window_never_starts_a_coordinator() {
    let directory = tempfile::tempdir().unwrap();
    for window in [0, northstar_portal::MAX_CHALLENGE_WINDOW_SLOTS + 1] {
        let mut config = config(directory.path());
        config.challenge_window_slots = window;
        assert!(ProofCoordinator::start(config, PORTAL).is_err());
        assert!(!directory.path().join("state").exists());
    }
}

#[test]
fn prepared_directory_rejects_symlinks() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path());
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&config.directory)
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), config.directory.join("prepared")).unwrap();
    let error = ProofCoordinator::start(config, PORTAL).err().unwrap();
    assert!(error.to_string().contains("prepared directory"));
}

#[test]
fn missing_capture_never_publishes_a_prepared_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let (session, artifact, _) = capture();
    let coordinator = ProofCoordinator::start(config(directory.path()), PORTAL).unwrap();
    let missing = Arc::new(ErHistoryStore::new(10));
    let id = key(PORTAL, &session, &artifact);
    wait(|| {
        assert!(!coordinator.prepare(artifact.clone(), session, missing.clone(), 0));
        coordinator.shared.lock().unwrap().failed.contains(&id)
    });
    assert_eq!(
        fs::read_dir(directory.path().join("state/prepared"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn corrupted_preparation_stops_recovery_before_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let (session, artifact, history) = capture();
    let coordinator = ProofCoordinator::start(config(directory.path()), PORTAL).unwrap();
    wait(|| coordinator.prepare(artifact.clone(), session, history.clone(), 0));
    drop(coordinator);
    let path = directory
        .path()
        .join("state/prepared")
        .join(name(&key(PORTAL, &session, &artifact)));
    fs::write(path, b"incomplete").unwrap();
    let coordinator = ProofCoordinator::start(config(directory.path()), PORTAL).unwrap();
    wait(|| coordinator.thread.as_ref().unwrap().is_finished());
    assert!(!coordinator.shared.lock().unwrap().ready);
    assert!(coordinator.take_transaction().is_none());
}
