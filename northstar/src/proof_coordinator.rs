use {
    crate::{
        checkpoint::CheckpointArtifactV1,
        proof_jobs::ProofJobStore,
        proof_process,
        proof_upload::{expected_binding, reconcile_upload, ProofUploadAction},
        proof_verification::VerifiedProof,
        replay::{extract_replay_witness_v1, ReplayContextV1},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_portal::{
        Challenge, ChallengeStatus, ChallengeTurn, Checkpoint, CheckpointStatus, Session,
    },
    northstar_transaction_proof::{decode_witness, encode_witness, session_context_bytes_v1},
    solana_account::ReadableAccount,
    solana_keypair::{read_keypair_file, Keypair},
    solana_pubkey::Pubkey,
    solana_rpc::er_history::ErHistoryStore,
    solana_runtime::bank::Bank,
    solana_sha256_hasher::hashv,
    solana_signer::Signer,
    solana_transaction::Transaction,
    std::{
        collections::{HashMap, HashSet},
        ffi::OsString,
        fs::{self, File},
        io::{self, Read, Write},
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    },
};

const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;

pub struct CoordinatorConfig {
    pub directory: PathBuf,
    pub prover: PathBuf,
    pub challenger: Arc<Keypair>,
    pub challenge_window_slots: u64,
}

impl CoordinatorConfig {
    pub fn from_environment(validator: Pubkey) -> io::Result<Option<Self>> {
        let names = [
            "NORTHSTAR_PROOF_JOB_DIR",
            "NORTHSTAR_PROOF_PROVER",
            "NORTHSTAR_PROOF_CHALLENGER_KEYPAIR",
        ];
        let values: Vec<_> = names.iter().map(std::env::var_os).collect();
        if values.iter().all(Option::is_none)
            && std::env::var_os("NORTHSTAR_PROOF_CHALLENGE_WINDOW_SLOTS").is_none()
        {
            return Ok(None);
        }
        let paths: Vec<PathBuf> = values
            .into_iter()
            .zip(names)
            .map(|(value, name)| {
                let path = PathBuf::from(
                    value.ok_or_else(|| io::Error::other(format!("{name} is required")))?,
                );
                if !path.is_absolute() {
                    return Err(io::Error::other(format!("{name} must be absolute")));
                }
                Ok(path)
            })
            .collect::<io::Result<_>>()?;
        if fs::metadata(&paths[2])?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other("challenger key file must be private"));
        }
        let challenger = Arc::new(
            read_keypair_file(&paths[2])
                .map_err(|_| io::Error::other("cannot read challenger key"))?,
        );
        if challenger.pubkey() == validator {
            return Err(io::Error::other(
                "challenger and validator must be distinct",
            ));
        }
        Ok(Some(Self {
            directory: paths[0].clone(),
            prover: paths[1].clone(),
            challenger,
            challenge_window_slots: match std::env::var("NORTHSTAR_PROOF_CHALLENGE_WINDOW_SLOTS") {
                Ok(value) => value.parse::<u64>().map_err(error)?,
                Err(std::env::VarError::NotPresent) => 750,
                Err(_) => return Err(error("proof challenge window must be UTF-8")),
            },
        }))
    }
}

struct Preparation {
    id: [u8; 32],
    artifact: CheckpointArtifactV1,
    session: Session,
    history: Arc<ErHistoryStore>,
    prepared_after_slot: u64,
}

#[derive(BorshSerialize, BorshDeserialize)]
struct Prepared {
    version: u8,
    prepared_after_slot: u64,
    session: Session,
    artifact: CheckpointArtifactV1,
    jobs: Vec<[u8; 32]>,
}

#[derive(Default)]
struct Shared {
    ready: bool,
    needs_readiness: bool,
    prepared: HashSet<[u8; 32]>,
    failed: HashSet<[u8; 32]>,
    preparing: Option<[u8; 32]>,
    preparation: Option<Preparation>,
    banks: Option<(Arc<Bank>, Arc<Bank>)>,
    outgoing: Option<Transaction>,
}

pub struct ProofCoordinator {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    portal: Pubkey,
    pub challenge_window_slots: u64,
}

fn session_origin(session: &Session) -> [u8; 32] {
    hashv(&[
        b"northstar-proof-session-origin-v1",
        &borsh::to_vec(&(
            session.grid_id,
            session.nonce,
            session.validator,
            session.created_at,
            session.authority,
            session.ttl_slots,
            session.fee_cap,
            session.settlement_interval_slots,
        ))
        .expect("fixed session origin"),
    ])
    .to_bytes()
}

fn checkpoint_matches(record: &Prepared, checkpoint: &Checkpoint) -> bool {
    let expected = &record.artifact.checkpoint;
    checkpoint.session == expected.session
        && checkpoint.proposer == record.session.validator
        && checkpoint.er_slot == expected.er_slot
        && checkpoint.step_count == u64::from(expected.step_count)
        && checkpoint.previous_state_root == expected.previous_state_root
        && checkpoint.new_state_root == expected.new_state_root
        && checkpoint.trace_root == expected.trace_root
        && checkpoint.tx_effect_root == expected.transaction_effect_root
        && checkpoint.readonly_l1_root == expected.readonly_l1_root
        && checkpoint.da_commitment == expected.da_commitment
        && checkpoint.effect_commitment == expected.effect_commitment
}

fn key(portal: Pubkey, session: &Session, artifact: &CheckpointArtifactV1) -> [u8; 32] {
    hashv(&[
        b"northstar-prepared-checkpoint-v1",
        &session_origin(session),
        &borsh::to_vec(&artifact.checkpoint).expect("fixed checkpoint encoding"),
        &session_context_bytes_v1(
            portal.to_bytes(),
            artifact.checkpoint.session.to_bytes(),
            session.grid_id,
            session.nonce,
            session.validator.to_bytes(),
        ),
    ])
    .to_bytes()
}

fn name(id: &[u8; 32]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn error(message: impl ToString) -> io::Error {
    io::Error::other(message.to_string())
}

fn bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(error("coordinator artifact type or size"));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(error("coordinator artifact size"));
    }
    Ok(bytes)
}

fn atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| error("coordinator artifact parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()
}

impl ProofCoordinator {
    pub fn start(config: CoordinatorConfig, portal: Pubkey) -> io::Result<Self> {
        if !(crate::DEFAULT_CHECKPOINT_CHALLENGE_WINDOW_SLOTS
            ..=northstar_portal::MAX_CHALLENGE_WINDOW_SLOTS)
            .contains(&config.challenge_window_slots)
        {
            return Err(error("proof challenge window is outside Portal bounds"));
        }
        let challenge_window_slots = config.challenge_window_slots;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.directory)?;
        let metadata = fs::symlink_metadata(&config.directory)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(error("coordinator directory must be private"));
        }
        let store = ProofJobStore::open(&config.directory.join("jobs"))?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(config.directory.join("prepared"))
            .or_else(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(error)
                }
            })?;
        let metadata = fs::symlink_metadata(config.directory.join("prepared"))?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(error(
                "prepared directory must be private and not a symlink",
            ));
        }
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_shared = shared.clone();
        let thread_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("northstar-proof".into())
            .spawn(move || {
                if let Err(error) = coordinate(config, portal, store, &thread_shared, &thread_stop)
                {
                    log::error!("Proof coordinator stopped: {error}");
                    thread_shared.lock().unwrap().ready = false;
                }
            })?;
        Ok(Self {
            shared,
            stop,
            thread: Some(thread),
            portal,
            challenge_window_slots,
        })
    }

    pub fn update_banks(&self, latest: Arc<Bank>, root: Arc<Bank>) {
        self.shared.lock().unwrap().banks = Some((latest, root));
    }

    pub fn take_transaction(&self) -> Option<Transaction> {
        self.shared.lock().unwrap().outgoing.take()
    }

    pub fn prepare(
        &self,
        artifact: CheckpointArtifactV1,
        session: Session,
        history: Arc<ErHistoryStore>,
        prepared_after_slot: u64,
    ) -> bool {
        let id = key(self.portal, &session, &artifact);
        let mut shared = self.shared.lock().unwrap();
        if shared.ready && shared.prepared.contains(&id) {
            return true;
        }
        if !shared.ready {
            shared.needs_readiness = true;
        }
        if !shared.prepared.contains(&id)
            && !shared.failed.contains(&id)
            && shared.preparing.is_none()
        {
            shared.ready = false;
            shared.needs_readiness = true;
            shared.preparing = Some(id);
            shared.preparation = Some(Preparation {
                prepared_after_slot,
                id,
                artifact,
                session,
                history,
            });
        }
        false
    }
}

impl Drop for ProofCoordinator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn prepare(portal: Pubkey, store: &ProofJobStore, work: Preparation) -> io::Result<Prepared> {
    work.artifact.verify().map_err(error)?;
    let profile = decode_witness(include_bytes!("../zkvm-replay/fixture-v1.bin"))
        .map_err(|error| io::Error::other(format!("frozen profile: {error:?}")))?;
    let context = session_context_bytes_v1(
        portal.to_bytes(),
        work.artifact.checkpoint.session.to_bytes(),
        work.session.grid_id,
        work.session.nonce,
        work.session.validator.to_bytes(),
    );
    let mut jobs = Vec::new();
    for step in 0..work.artifact.checkpoint.step_count {
        let binding =
            expected_binding(portal, &work.session, &work.artifact, step).map_err(error)?;
        let witness = extract_replay_witness_v1(
            &work.history,
            &work.artifact,
            step as usize,
            ReplayContextV1 {
                session_context: context.clone(),
                agave_revision: profile.runtime.agave_revision,
                northstar_revision: profile.runtime.northstar_revision,
                vm_config_hash: profile.runtime.vm_config_hash,
                syscall_registry_hash: profile.runtime.syscall_registry_hash,
            },
            &binding.public_inputs,
        )
        .map_err(error)?;
        let bytes = encode_witness(&witness)
            .map_err(|error| io::Error::other(format!("witness encoding: {error:?}")))?;
        jobs.push(store.enqueue(binding, &bytes)?);
    }
    Ok(Prepared {
        version: 1,
        prepared_after_slot: work.prepared_after_slot,
        session: work.session,
        artifact: work.artifact,
        jobs,
    })
}

#[cfg(test)]
pub(crate) fn copy_test_evidence(source: &Path, destination: &Path, checkpoint: Pubkey) {
    let portal = solana_pubkey::pubkey!("5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf");
    let record = fs::read_dir(source.join("prepared"))
        .unwrap()
        .filter_map(Result::ok)
        .find_map(|entry| {
            let bytes = bounded(&entry.path(), MAX_RECORD_BYTES).ok()?;
            let record = Prepared::try_from_slice(bytes.get(32..)?).ok()?;
            (northstar_portal::find_checkpoint_pda(
                &portal,
                &record.artifact.checkpoint.session,
                record.artifact.checkpoint.er_slot,
            )
            .0 == checkpoint)
                .then_some(record)
        })
        .expect("prepared runtime checkpoint");
    assert_eq!(record.jobs.len(), 1);
    let job = source.join("jobs").join(name(&record.jobs[0]));
    let witness = bounded(
        &job.join("witness"),
        crate::proof_jobs::MAX_PROOF_WITNESS_BYTES,
    )
    .unwrap();
    let proof = bounded(&job.join("proof"), 388).unwrap();
    assert_eq!(proof.len(), 388);
    assert_eq!(
        hashv(&[&record.jobs[0], &proof[..356]]).as_ref(),
        &proof[356..]
    );
    let measurements = bounded(&job.join("measurements.json"), 256 * 1024).unwrap();
    let resolver = bounded(&job.join("resolver-fixture.bin"), 256 * 1024).unwrap();
    let binding = expected_binding(portal, &record.session, &record.artifact, 0).unwrap();
    VerifiedProof::verify(binding.clone(), &proof[..356]).unwrap();
    let decoded = decode_witness(&witness).unwrap();
    assert_eq!(
        decoded.transaction_bytes,
        record.artifact.da.pages[0].transaction
    );
    assert_eq!(
        northstar_transaction_proof::public_inputs_bytes(
            northstar_transaction_proof::replay(&decoded).unwrap()
        ),
        binding.public_inputs
    );
    for (file, bytes) in [
        ("witness-v2.bin", witness.as_slice()),
        ("northstar-sp1-groth16-onchain.bin", &proof[..356]),
        (
            "northstar-sp1-public-inputs.bin",
            binding.public_inputs.as_slice(),
        ),
        ("measurements.json", measurements.as_slice()),
        ("resolver-fixture.bin", resolver.as_slice()),
    ] {
        atomic(&destination.join(file), bytes).unwrap();
    }
}

#[cfg(test)]
#[path = "proof_coordinator_tests.rs"]
mod tests;

fn coordinate(
    config: CoordinatorConfig,
    portal: Pubkey,
    store: ProofJobStore,
    shared: &Mutex<Shared>,
    stop: &AtomicBool,
) -> io::Result<()> {
    let records_dir = config.directory.join("prepared");
    let mut records = HashMap::new();
    let mut referenced = HashSet::new();
    for entry in fs::read_dir(&records_dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let bytes = bounded(&entry.path(), MAX_RECORD_BYTES)?;
        if bytes.len() < 32 || hashv(&[&bytes[32..]]).as_ref() != &bytes[..32] {
            return Err(error("prepared record checksum"));
        }
        let record = Prepared::try_from_slice(&bytes[32..])?;
        let id = key(portal, &record.session, &record.artifact);
        if entry.file_name() != name(&id).as_str()
            || record.version != 1
            || record.jobs.len() != record.artifact.checkpoint.step_count as usize
        {
            return Err(error("prepared record binding"));
        }
        record.artifact.verify().map_err(error)?;
        for (step, job) in record.jobs.iter().enumerate() {
            if store.load(job)?.binding
                != expected_binding(portal, &record.session, &record.artifact, step as u32)
                    .map_err(error)?
            {
                return Err(error("prepared job binding"));
            }
            referenced.insert(*job);
        }
        shared.lock().unwrap().prepared.insert(id);
        records.insert(id, record);
    }
    // No checkpoint can be proposed until its complete prepared record is durable.
    for orphan in store
        .ids()?
        .into_iter()
        .filter(|id| !referenced.contains(id))
    {
        store.retire(&orphan)?;
    }
    let mut verified = HashMap::<[u8; 32], VerifiedProof>::new();
    let mut paused = HashSet::new();
    let mut pending: Option<(Transaction, [u8; 32], Instant)> = None;
    let mut retry_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
        if Instant::now() < retry_at {
            continue;
        }
        let needs_preflight = {
            let state = shared.lock().unwrap();
            state.needs_readiness && !state.ready
        };
        if needs_preflight {
            let directory = tempfile::Builder::new()
                .prefix(".stage-preflight-")
                .tempdir_in(config.directory.join("jobs"))?;
            match proof_process::run(
                &config.prover,
                &["preflight".into()],
                directory.path(),
                Duration::from_secs(300),
                stop,
            ) {
                Ok(elapsed) => {
                    {
                        let mut state = shared.lock().unwrap();
                        state.ready = true;
                        state.needs_readiness = false;
                    }
                    log::info!(
                        "Proof coordinator ready: preflight_ms={}",
                        elapsed.as_millis()
                    );
                }
                Err(error) => {
                    log::warn!("Proof preflight unavailable: {error}");
                    retry_at = Instant::now() + Duration::from_secs(2);
                    continue;
                }
            }
        }
        let work = {
            let mut state = shared.lock().unwrap();
            if state.ready {
                state.preparation.take()
            } else {
                None
            }
        };
        if let Some(work) = work {
            let id = work.id;
            match prepare(portal, &store, work) {
                Ok(record) => {
                    let encoded = borsh::to_vec(&record)?;
                    if encoded.len() + 32 > MAX_RECORD_BYTES {
                        return Err(error("prepared record too large"));
                    }
                    let mut bytes = hashv(&[&encoded]).to_bytes().to_vec();
                    bytes.extend(encoded);
                    atomic(&records_dir.join(name(&id)), &bytes)?;
                    log::info!(
                        "Proof checkpoint prepared: er_slot={} steps={}",
                        record.artifact.checkpoint.er_slot,
                        record.jobs.len()
                    );
                    records.insert(id, record);
                    shared.lock().unwrap().prepared.insert(id);
                }
                Err(error) => {
                    log::error!("Proof checkpoint preparation rejected: {error}");
                    let mut state = shared.lock().unwrap();
                    if state.failed.len() >= crate::proof_jobs::MAX_PENDING_PROOF_JOBS {
                        let evicted = *state.failed.iter().next().unwrap();
                        state.failed.remove(&evicted);
                    }
                    state.failed.insert(id);
                }
            }
            shared.lock().unwrap().preparing = None;
        }
        let Some((bank, root)) = shared.lock().unwrap().banks.clone() else {
            continue;
        };
        if let Some((transaction, job, sent)) = pending.as_mut() {
            match bank.get_signature_status(&transaction.signatures[0]) {
                Some(Ok(())) => {
                    pending = None;
                }
                Some(Err(error)) => {
                    log::error!("Proof submission paused: job={} error={error:?}", name(job));
                    paused.insert(*job);
                    pending = None;
                }
                None if !bank.is_hash_valid_for_age(
                    &transaction.message.recent_blockhash,
                    solana_clock::MAX_PROCESSING_AGE,
                ) =>
                {
                    pending = None;
                }
                None => {
                    if sent.elapsed() >= Duration::from_secs(2) {
                        shared.lock().unwrap().outgoing = Some(transaction.clone());
                        *sent = Instant::now();
                    }
                    continue;
                }
            }
        }
        let mut retired = Vec::new();
        for (record_id, record) in &records {
            let read_session = |bank: &Bank| {
                bank.get_account(&record.artifact.checkpoint.session)
                    .filter(|account| account.owner() == &portal)
                    .and_then(|account| Session::try_from_slice(account.data()).ok())
                    .filter(Session::is_valid)
            };
            let root_session = read_session(&root);
            if root_session
                .as_ref()
                .is_some_and(|session| session.created_at > record.session.created_at)
            {
                retired.push(*record_id);
                continue;
            }
            let Some(session) = read_session(&bank) else {
                continue;
            };
            if session_origin(&session) != session_origin(&record.session) {
                continue;
            }
            let checkpoint_key = northstar_portal::find_checkpoint_pda(
                &portal,
                &record.artifact.checkpoint.session,
                record.artifact.checkpoint.er_slot,
            )
            .0;
            if root
                .get_account(&checkpoint_key)
                .filter(|account| account.owner() == &portal)
                .and_then(|account| Checkpoint::try_from_slice(account.data()).ok())
                .is_some_and(|checkpoint| {
                    checkpoint.is_valid()
                        && root_session.as_ref().is_some_and(|session| {
                            session_origin(session) == session_origin(&record.session)
                        })
                        && checkpoint_matches(record, &checkpoint)
                        && checkpoint.proposed_at_l1_slot > record.prepared_after_slot
                        && matches!(
                            checkpoint.status,
                            CheckpointStatus::Settled
                                | CheckpointStatus::Cancelled
                                | CheckpointStatus::Invalid
                        )
                })
            {
                retired.push(*record_id);
                continue;
            }
            let challenge_key = northstar_portal::find_challenge_pda(&portal, &checkpoint_key).0;
            let Some(challenge) = bank
                .get_account(&challenge_key)
                .filter(|account| account.owner() == &portal)
                .and_then(|account| Challenge::try_from_slice(account.data()).ok())
            else {
                continue;
            };
            if !challenge.is_valid()
                || challenge.status != ChallengeStatus::Active
                || challenge.turn != ChallengeTurn::Prove
                || challenge.challenger != config.challenger.pubkey()
            {
                continue;
            }
            let Some(id) = record.jobs.get(challenge.start_step as usize) else {
                continue;
            };
            if paused.contains(id) {
                continue;
            }
            let job = store.load(id)?;
            let action = match reconcile_upload(
                &bank,
                &config.challenger.pubkey(),
                &job.binding,
                &record.artifact,
                verified.get(id),
            ) {
                Ok(action) => action,
                Err(error) => {
                    log::error!("Proof job paused: job={} reason={error}", name(id));
                    paused.insert(*id);
                    continue;
                }
            };
            match action {
                ProofUploadAction::Prove => {
                    if job.proof.is_none() {
                        let mut state = shared.lock().unwrap();
                        if !state.ready {
                            state.needs_readiness = true;
                            continue;
                        }
                    }
                    let proof = if let Some(proof) = job.proof {
                        proof.to_vec()
                    } else {
                        let directory = tempfile::Builder::new()
                            .prefix(".stage-prove-")
                            .tempdir_in(config.directory.join("jobs"))?;
                        let witness = directory.path().join("witness.bin");
                        atomic(&witness, &job.witness)?;
                        let measurements = directory.path().join("measurements.json");
                        log::info!(
                            "Proof job proving: job={} er_slot={} step={}",
                            name(id),
                            job.binding.er_slot,
                            job.binding.step
                        );
                        let args: Vec<OsString> = vec![
                            "groth16".into(),
                            witness.into_os_string(),
                            measurements.clone().into_os_string(),
                            "manager-coordinator".into(),
                        ];
                        if let Err(error) = proof_process::run(
                            &config.prover,
                            &args,
                            directory.path(),
                            Duration::from_secs(130),
                            stop,
                        ) {
                            log::warn!("Proof request retry: job={} reason={error}", name(id));
                            {
                                let mut state = shared.lock().unwrap();
                                state.ready = false;
                                state.needs_readiness = true;
                            }
                            retry_at = Instant::now() + Duration::from_secs(2);
                            break;
                        }
                        if bounded(
                            &directory.path().join("northstar-sp1-public-inputs.bin"),
                            256,
                        )? != job.binding.public_inputs
                        {
                            return Err(error("prover public inputs mismatch"));
                        }
                        let proof = bounded(
                            &directory.path().join("northstar-sp1-groth16-onchain.bin"),
                            356,
                        )?;
                        atomic(
                            &store.job_directory(id).join("measurements.json"),
                            &bounded(&measurements, 256 * 1024)?,
                        )?;
                        proof
                    };
                    let started = Instant::now();
                    let result =
                        VerifiedProof::verify(job.binding.clone(), &proof).map_err(error)?;
                    store.complete(
                        id,
                        result.bytes_for(&job.binding).map_err(error)?,
                        &job.binding.public_inputs,
                    )?;
                    log::info!(
                        "Proof job verified: job={} host_verify_ms={}",
                        name(id),
                        started.elapsed().as_millis()
                    );
                    verified.insert(*id, result);
                    // A proof may outlive the bank used to select it. Reconcile again on a fresh bank.
                    break;
                }
                ProofUploadAction::Submit(instruction) => {
                    #[cfg(feature = "proof-coordinator-test-hooks")]
                    if let Some(path) =
                        std::env::var_os("NORTHSTAR_PROOF_TEST_UPLOAD_FENCE").map(PathBuf::from)
                    {
                        if matches!(northstar_portal::PortalInstruction::try_from_slice(&instruction.data), Ok(northstar_portal::PortalInstruction::WriteStepProof(write)) if write.offset > 0)
                            && !path.with_extension("resume").exists()
                        {
                            if !path.exists() {
                                atomic(&path, bank.slot().to_string().as_bytes())?;
                            }
                            continue;
                        }
                    }
                    #[cfg(feature = "proof-coordinator-test-hooks")]
                    if matches!(
                        northstar_portal::PortalInstruction::try_from_slice(&instruction.data),
                        Ok(northstar_portal::PortalInstruction::ResolveChallenge(_))
                    ) {
                        let accounts = instruction
                            .accounts
                            .iter()
                            .skip(1)
                            .map(|meta| {
                                bank.get_account(&meta.pubkey)
                                    .map(|account| {
                                        (meta.pubkey, solana_account::Account::from(account))
                                    })
                                    .ok_or_else(|| error("resolver snapshot account missing"))
                            })
                            .collect::<io::Result<Vec<_>>>()?;
                        atomic(
                            &store.job_directory(id).join("resolver-fixture.bin"),
                            &bincode::serialize(&(bank.slot(), job.binding.er_slot, accounts))
                                .map_err(error)?,
                        )?;
                    }
                    let transaction = Transaction::new_signed_with_payer(
                        &[instruction],
                        Some(&config.challenger.pubkey()),
                        &[config.challenger.as_ref()],
                        bank.last_blockhash(),
                    );
                    log::info!(
                        "Proof upload submit: job={} signature={}",
                        name(id),
                        transaction.signatures[0]
                    );
                    shared.lock().unwrap().outgoing = Some(transaction.clone());
                    pending = Some((transaction, *id, Instant::now()));
                    break;
                }
                ProofUploadAction::Wait | ProofUploadAction::Finished { .. } => (),
            }
        }
        for id in retired {
            let record = records.remove(&id).unwrap();
            // Remove the record first: a crash leaves only orphan jobs, cleaned at startup.
            fs::remove_file(records_dir.join(name(&id)))?;
            File::open(&records_dir)?.sync_all()?;
            for job in record.jobs {
                if records.values().any(|record| record.jobs.contains(&job)) {
                    continue;
                }
                store.retire(&job)?;
                verified.remove(&job);
                paused.remove(&job);
            }
            shared.lock().unwrap().prepared.remove(&id);
        }
    }
    Ok(())
}
