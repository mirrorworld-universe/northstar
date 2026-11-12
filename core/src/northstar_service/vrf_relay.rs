// Sonic: Native discovery and delivery without a VRF credential or local prover.
use {
    anyhow::{Context, Result, ensure},
    northstar::vrf_commitment::VrfCommitmentSource,
    northstar_vrf_relay::{
        remote_prover::{CommitmentSource, RelaySettings},
        service::Worker,
    },
    solana_rpc::er_history::ErHistoryObserver,
    solana_runtime::bank::Bank,
    solana_transaction_status::VersionedTransactionWithStatusMeta,
    std::{
        fs::{self, File},
        io::Write,
        net::SocketAddr,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread::{self, JoinHandle},
        time::Duration,
    },
};

const _: () = assert!(northstar_vrf_relay::LOCAL_PROVING_DISABLED);

struct Fanout(Vec<Arc<dyn ErHistoryObserver>>);
impl ErHistoryObserver for Fanout {
    fn transaction_recorded(
        &self,
        bank: &Bank,
        record: &VersionedTransactionWithStatusMeta,
        index: u32,
    ) {
        for observer in &self.0 {
            observer.transaction_recorded(bank, record, index);
        }
    }
    fn slot_finalized(&self, bank: &Bank, count: u64) {
        for observer in &self.0 {
            observer.slot_finalized(bank, count);
        }
    }
}

pub(super) fn combined(
    first: Option<Arc<dyn ErHistoryObserver>>,
    second: Option<Arc<dyn ErHistoryObserver>>,
) -> Option<Arc<dyn ErHistoryObserver>> {
    match (first, second) {
        (None, None) => None,
        (Some(observer), None) | (None, Some(observer)) => Some(observer),
        (Some(first), Some(second)) => Some(Arc::new(Fanout(vec![first, second]))),
    }
}

struct WakeObserver(crossbeam_channel::Sender<()>);
impl ErHistoryObserver for WakeObserver {
    fn transaction_recorded(
        &self,
        _bank: &Bank,
        _record: &VersionedTransactionWithStatusMeta,
        _index: u32,
    ) {
    }
    fn slot_finalized(&self, _bank: &Bank, _count: u64) {
        let _ = self.0.try_send(());
    }
}

fn validate_endpoint(config: &RelaySettings, listen: SocketAddr) -> Result<()> {
    config.validate()?;
    let text = config
        .worker_settings()
        .rpc_url
        .strip_prefix("http://")
        .context("Native relay requires local HTTP ER RPC")?;
    let endpoint: SocketAddr = text.trim_end_matches('/').parse()?;
    ensure!(
        endpoint.ip().is_loopback() && endpoint.port() == listen.port(),
        "relay ER endpoint differs from this Native runtime"
    );
    Ok(())
}

pub(super) struct Bootstrap {
    config: RelaySettings,
    wake: crossbeam_channel::Sender<()>,
    receiver: crossbeam_channel::Receiver<()>,
}
impl Bootstrap {
    pub(super) fn load(path: Option<&Path>, listen: SocketAddr) -> Result<Option<Self>> {
        let Some(path) = path else {
            return Ok(None);
        };
        let config = RelaySettings::load(path)?;
        validate_endpoint(&config, listen)?;
        let (wake, receiver) = crossbeam_channel::bounded(1);
        Ok(Some(Self {
            config,
            wake,
            receiver,
        }))
    }
    pub(super) fn observer(&self) -> Arc<dyn ErHistoryObserver> {
        Arc::new(WakeObserver(self.wake.clone()))
    }
    pub(super) fn start_when_ready(
        pending: &mut Option<Self>,
        source: Option<VrfCommitmentSource>,
        portal: solana_pubkey::Pubkey,
        exit: Arc<AtomicBool>,
    ) -> Result<Option<Service>> {
        let Some(source) = source else {
            return Ok(None);
        };
        let Some(boot) = pending.take() else {
            return Ok(None);
        };
        boot.start(source, portal, exit).map(Some)
    }

    fn start(
        self,
        source: VrfCommitmentSource,
        portal: solana_pubkey::Pubkey,
        exit: Arc<AtomicBool>,
    ) -> Result<Service> {
        let identity = source
            .identity()?
            .context("relay requires an active continuity lineage")?;
        let worker = self.config.worker_settings();
        ensure!(
            identity.portal == portal.to_bytes()
                && worker
                    .er_session
                    .is_some_and(|session| session.to_bytes() == identity.session)
                && identity.commitment(northstar::continuity_v2::hash_parts)?
                    == northstar_vrf_relay::portal::parse_context(
                        worker
                            .er_session_context
                            .as_deref()
                            .context("relay context missing")?
                    )?,
            "relay session differs from the active Native lineage"
        );
        let source = Arc::new(Source {
            source,
            coordinator: worker.coordinator,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let wake = self.wake.clone();
        let config = self.config;
        let receiver = self.receiver;
        let join = thread::Builder::new()
            .name("solVrfRelay".into())
            .spawn(move || {
                let mut worker: Option<Worker> = None;
                let mut blocked = false;
                while !thread_stop.load(Ordering::Relaxed) && !exit.load(Ordering::Relaxed) {
                    let result = if let Some(worker) = worker.as_mut() {
                        worker.tick_until(|| {
                            thread_stop.load(Ordering::Relaxed) || exit.load(Ordering::Relaxed)
                        })
                    } else {
                        Worker::open_relay(config.clone(), source.clone()).map(|opened| {
                            worker = Some(opened);
                        })
                    };
                    if result.is_err() && !blocked {
                        log::warn!(
                            "VRF relay is not qualified; retaining journal and remote-only policy"
                        );
                    }
                    blocked = result.is_err();
                    if publish_status(
                        &config,
                        worker.as_ref(),
                        if blocked { "blocked" } else { "running" },
                    )
                    .is_err()
                    {
                        log::error!("VRF relay status persistence failed; stopping relay");
                        break;
                    }
                    match receiver
                        .recv_timeout(Duration::from_millis(config.worker_settings().poll_ms))
                    {
                        Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    }
                }
                let _ = publish_status(&config, worker.as_ref(), "stopped");
                drop(worker);
            })?;
        Ok(Service {
            stop,
            wake,
            join: Some(join),
        })
    }
}

struct Source {
    source: VrfCommitmentSource,
    coordinator: solana_pubkey::Pubkey,
}
impl CommitmentSource for Source {
    fn candidates(&self) -> Result<Vec<solana_pubkey::Pubkey>> {
        Ok(self.source.candidates(
            &self.coordinator,
            &northstar_vrf_relay::accounts::discriminator("account", "RequestAccount"),
        )?)
    }
    fn witness(
        &self,
        address: &solana_pubkey::Pubkey,
    ) -> Result<Option<northstar_vrf_relay::commitment_protocol::RequestWitness>> {
        Ok(self.source.witness(address)?)
    }
}

fn publish_status(config: &RelaySettings, worker: Option<&Worker>, phase: &str) -> Result<()> {
    let settings = config.worker_settings();
    let directory = &settings.state_dir;
    if !directory.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
    }
    let metadata = fs::symlink_metadata(directory)?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
        "relay status directory must be private"
    );
    let summary = worker.map(Worker::summary);
    let records = summary
        .as_ref()
        .and_then(|value| value["records"].as_array());
    let bytes = serde_json::to_vec(
        &serde_json::json!({"version":1,"phase":phase,"proof_mode":"remote-only",
        "session":settings.er_session.map(|key|key.to_string()),"session_context":settings.er_session_context,
        "fulfiller":settings.fulfiller.to_string(),"coordinator":settings.coordinator.to_string(),
        "signer_policy_sha256":config.signer.policy_sha256,
        "records":records.map_or(0,Vec::len),"confirmed":records.map_or(0,|records|records.iter().filter(|record|record["status"]=="confirmed").count())}),
    )?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(directory.join("native-relay-status.json"))?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

pub(super) struct Service {
    stop: Arc<AtomicBool>,
    wake: crossbeam_channel::Sender<()>,
    join: Option<JoinHandle<()>>,
}
impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.try_send(());
        if self.join.take().is_some_and(|join| join.join().is_err()) {
            log::error!("VRF relay shutdown did not join cleanly");
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, northstar_vrf_relay::remote_prover::RelaySettings, serde_json::json,
        solana_pubkey::Pubkey,
    };

    fn config() -> RelaySettings {
        let portal = Pubkey::new_from_array([2; 32]);
        let session = northstar_portal::find_session_pda(&portal).0;
        serde_json::from_value(json!({"version":1,"worker":{
            "network":"local","rpc_url":"http://127.0.0.1:18910","expected_genesis":"11111111111111111111111111111111",
            "l1_rpc_url":"http://127.0.0.1:18899","er_session":session.to_string(),"er_session_context":"11".repeat(32),
            "coordinator":Pubkey::new_from_array([3;32]).to_string(),"verifier":Pubkey::new_from_array([4;32]).to_string(),
            "coordinator_programdata_sha256":"22".repeat(32),"verifier_programdata_sha256":"33".repeat(32),
            "vrf_public_key":Pubkey::new_from_array([5;32]).to_string(),"fulfiller":Pubkey::new_from_array([6;32]).to_string(),
            "signer_file":"fulfiller.json","state_dir":"state","commitment":"confirmed",
            "allowed_consumers":[Pubkey::new_from_array([7;32]).to_string()],"allow_store":false,"poll_ms":2000,"max_attempts":2,"max_records":64
        },"signer":{"url":"http://127.0.0.1:19030","bearer_token_file":"bearer","policy_sha256":"44".repeat(32)}})).unwrap()
    }

    #[test]
    fn regression_startup_waits_for_active_lineage_without_reading_credentials() {
        let (wake, receiver) = crossbeam_channel::bounded(1);
        let mut pending = Some(Bootstrap {
            config: config(),
            wake,
            receiver,
        });
        let exit = Arc::new(AtomicBool::new(false));
        assert!(
            Bootstrap::start_when_ready(&mut pending, None, Pubkey::default(), exit.clone())
                .unwrap()
                .is_none()
        );
        assert!(pending.is_some());
        assert!(!exit.load(Ordering::Relaxed));
    }

    #[test]
    fn guard_native_relay_requires_its_own_loopback_er_endpoint() {
        let good = config();
        validate_endpoint(&good, "0.0.0.0:18910".parse().unwrap()).unwrap();
        assert!(validate_endpoint(&good, "0.0.0.0:18911".parse().unwrap()).is_err());
        let mut value = serde_json::to_value(good).unwrap();
        value["worker"]["rpc_url"] = json!("https://other.example.invalid");
        let other: RelaySettings = serde_json::from_value(value).unwrap();
        assert!(validate_endpoint(&other, "0.0.0.0:18910".parse().unwrap()).is_err());
    }

    #[test]
    fn guard_finalization_wakes_are_bounded_and_preserve_existing_observers() {
        use std::sync::atomic::AtomicUsize;
        struct Counter(Arc<AtomicUsize>);
        impl ErHistoryObserver for Counter {
            fn transaction_recorded(
                &self,
                _bank: &Bank,
                _record: &VersionedTransactionWithStatusMeta,
                _index: u32,
            ) {
            }
            fn slot_finalized(&self, _bank: &Bank, _count: u64) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let count = Arc::new(AtomicUsize::new(0));
        let observer = combined(
            Some(Arc::new(Counter(count.clone()))),
            Some(Arc::new(WakeObserver(sender))),
        )
        .unwrap();
        let genesis = solana_runtime::genesis_utils::create_genesis_config(1).genesis_config;
        let bank = Bank::new_for_tests(&genesis);
        observer.slot_finalized(&bank, 1);
        observer.slot_finalized(&bank, 1);
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(receiver.len(), 1);
    }
}
