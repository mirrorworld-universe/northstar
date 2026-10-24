//! Opt-in atomic capture sidecars. These are inputs/results, never execution proofs.
use {
    serde::Serialize,
    serde_json::{json, Value},
    solana_pubkey::Pubkey,
    solana_runtime::bank::{
        er_vrf_candidate::{
            CandidateAccount, CandidateAccountEntry, ErVrfCandidateSnapshot,
            CANDIDATE_CAPTURE_DOMAIN,
        },
        Bank,
    },
    solana_svm::{
        transaction_commit_result::TransactionCommitResult,
        transaction_execution_result::ExecutedTransaction,
    },
    solana_transaction::versioned::VersionedTransaction,
    std::{
        fs::{self, File},
        io::{self, BufWriter, Write},
        path::{Path, PathBuf},
        str::FromStr,
        sync::{Arc, Mutex},
        time::{Instant, SystemTime, UNIX_EPOCH},
    },
};

#[derive(Debug)]
pub struct CaptureClock {
    origin: Instant,
    pub domain: String,
    pub collector_start_unix_ns: u128,
}
impl CaptureClock {
    pub fn new(domain: String) -> Arc<Self> {
        static NEXT_CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let collector_start_unix_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Arc::new(Self {
            origin: Instant::now(),
            domain: format!(
                "{domain}/{}/{collector_start_unix_ns}/{sequence}",
                std::process::id()
            ),
            collector_start_unix_ns,
        })
    }
    pub fn now_ns(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_nanos())
            .expect("collector monotonic duration fits u64")
    }
}

pub struct CandidateCaptureSink {
    directory: PathBuf,
    pub clock: Arc<CaptureClock>,
    max_records: usize,
    collector_sha256: String,
    count: Mutex<usize>,
    errors: Mutex<Vec<String>>,
    wires: Mutex<std::collections::HashMap<String, Vec<u8>>>,
}

#[derive(Serialize)]
struct ExecutionBoundary {
    accounts: Vec<CandidateAccountEntry>,
    tentative_accounts: Vec<CandidateAccountEntry>,
    rollback_accounts: Vec<CandidateAccountEntry>,
    touched_flags: Vec<bool>,
    status: Value,
    executed_units: u64,
    fee: u64,
    loaded_accounts_data_size: u32,
    compute_unit_limit: u64,
    heap_size: u32,
    execution_cost_debug: String,
    logs: Option<Vec<String>>,
    inner_instructions: Value,
    return_data_program_id: String,
    return_data: Vec<u8>,
}

pub(crate) struct PendingCapture {
    sink: Arc<CandidateCaptureSink>,
    sequence: usize,
    pub input: ErVrfCandidateSnapshot,
    start_ns: u64,
    before_commit_ns: Option<u64>,
    execution: Option<ExecutionBoundary>,
    rejection: Option<String>,
    state_keys: Vec<Pubkey>,
    checkpoint_pre_accounts: Vec<CandidateAccountEntry>,
}

fn canonical_json(value: Value) -> Vec<u8> {
    fn ordered(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.into_iter().collect();
                entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, ordered(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(ordered).collect()),
            scalar => scalar,
        }
    }
    serde_json::to_vec(&ordered(value)).unwrap()
}

fn account_entry(
    key: &Pubkey,
    account: Option<&solana_account::AccountSharedData>,
) -> CandidateAccountEntry {
    CandidateAccountEntry {
        pubkey: key.to_string(),
        account: account.map(CandidateAccount::from_shared),
    }
}
fn bank_accounts(bank: &Bank, input: &ErVrfCandidateSnapshot) -> Vec<CandidateAccountEntry> {
    input
        .accounts
        .iter()
        .map(|entry| {
            let key = Pubkey::from_str(&entry.pubkey).unwrap();
            account_entry(&key, bank.get_account(&key).as_ref())
        })
        .collect()
}
fn authoritative_prestate_matches(bank: &Bank, entries: &[CandidateAccountEntry]) -> bool {
    entries.iter().all(|entry| {
        let Ok(key) = Pubkey::from_str(&entry.pubkey) else {
            return false;
        };
        account_entry(&key, bank.get_account(&key).as_ref()) == *entry
    })
}

fn selected(tx: &VersionedTransaction) -> bool {
    let message = &tx.message;
    message.instructions().iter().any(|ix| {
        let Some(program) = message
            .static_account_keys()
            .get(ix.program_id_index as usize)
        else {
            return false;
        };
        [
            (
                &solana_runtime::bank::er_vrf_candidate::PROGRAMS[2].0,
                b"global:make_request".as_slice(),
            ),
            (
                &solana_runtime::bank::er_vrf_candidate::PROGRAMS[0].0,
                b"global:fulfill".as_slice(),
            ),
        ]
        .iter()
        .any(|(id, name)| {
            program == *id
                && ix.data.get(..8) == Some(&solana_sha256_hasher::hashv(&[name]).to_bytes()[..8])
        })
    })
}

impl CandidateCaptureSink {
    pub fn new(
        directory: &Path,
        clock: Arc<CaptureClock>,
        max_records: usize,
        identities: Value,
    ) -> io::Result<Arc<Self>> {
        if !(1..=512).contains(&max_records) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "capture bound must be 1..512",
            ));
        }
        fs::create_dir(directory)?;
        let mut sink = Self {
            directory: directory.to_owned(),
            clock,
            max_records,
            collector_sha256: String::new(),
            count: Mutex::new(0),
            errors: Mutex::new(Vec::new()),
            wires: Mutex::new(std::collections::HashMap::new()),
        };
        sink.write_json("collector.json", &json!({"domain":CANDIDATE_CAPTURE_DOMAIN, "clock_id":sink.clock.domain, "clock_domain":sink.clock.domain,
            "collector_start_unix_ns":sink.clock.collector_start_unix_ns, "start_ns":0, "observer_installed_ns":sink.clock.now_ns(),
            "max_records":max_records, "identities":identities, "execution_verified":false, "production_acceptance":false}))?;
        sink.collector_sha256 = solana_runtime::bank::er_vrf_candidate::digest(&fs::read(
            directory.join("collector.json"),
        )?);
        Ok(Arc::new(sink))
    }
    fn write_json(&self, filename: &str, value: &Value) -> io::Result<()> {
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(self.directory.join(filename))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, value)?;
        writer.write_all(b"\n")?;
        writer.flush()
    }
    fn error(&self, error: String) {
        let mut errors = self.errors.lock().unwrap();
        if errors.len() < 32 {
            errors.push(error);
        }
    }
    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }
    pub fn records(&self) -> usize {
        *self.count.lock().unwrap()
    }
    pub fn finish(&self) -> io::Result<()> {
        self.write_json("collector-end.json", &json!({"domain":CANDIDATE_CAPTURE_DOMAIN, "clock_id":self.clock.domain, "clock_domain":self.clock.domain,
            "end_ns":self.clock.now_ns(), "records":self.records(), "errors":self.errors(), "execution_verified":false}))
    }
    pub(crate) fn observe_wire(&self, wire: &[u8]) {
        let Ok(tx) = bincode::deserialize::<VersionedTransaction>(wire) else {
            return;
        };
        if !selected(&tx) {
            return;
        }
        if wire.len() > 1232 {
            self.error("signed wire byte bound".into());
            return;
        }
        let Some(signature) = tx.signatures.first() else {
            self.error("missing transaction signature".into());
            return;
        };
        let mut wires = self.wires.lock().unwrap();
        if wires.len() >= self.max_records && !wires.contains_key(&signature.to_string()) {
            self.error("pending wire bound".into());
            return;
        }
        wires.insert(signature.to_string(), wire.to_vec());
    }

    pub(crate) fn begin(
        self: &Arc<Self>,
        bank: &Bank,
        txs: &[VersionedTransaction],
        transaction_max_age: usize,
        mut state_keys: Vec<Pubkey>,
    ) -> Option<PendingCapture> {
        if !txs.iter().any(selected) {
            return None;
        }
        let mut count = self.count.lock().unwrap();
        if *count >= self.max_records {
            self.error("capture capacity exceeded".into());
            return None;
        }
        let sequence = *count;
        *count += 1;
        drop(count);
        let start_ns = self.clock.now_ns();
        let result = if txs.len() == 1 {
            let signature = txs[0]
                .signatures
                .first()
                .map(ToString::to_string)
                .unwrap_or_default();
            self.wires
                .lock()
                .unwrap()
                .remove(&signature)
                .ok_or("exact submitted wire absent")
                .and_then(|wire| {
                    let input = bank.er_vrf_candidate_snapshot(&txs[0], transaction_max_age)?;
                    if input.signed_transaction != wire {
                        return Err("noncanonical submitted wire");
                    }
                    Ok(input)
                })
        } else {
            Err("multi-transaction batch is not atomic per-step input")
        };
        match result {
            Ok(input) => {
                state_keys.sort_unstable();
                state_keys.dedup();
                if state_keys.len() > 1024 {
                    self.error("checkpoint state account bound".into());
                    return None;
                }
                let checkpoint_pre_accounts = state_keys
                    .iter()
                    .map(|key| account_entry(key, bank.get_account(key).as_ref()))
                    .collect();
                Some(PendingCapture {
                    sink: Arc::clone(self),
                    sequence,
                    input,
                    start_ns,
                    before_commit_ns: None,
                    execution: None,
                    rejection: None,
                    state_keys,
                    checkpoint_pre_accounts,
                })
            }
            Err(reason) => {
                self.error(reason.into());
                if let Err(error) = self.write_json(&format!("{sequence:05}-rejected.json"), &json!({"domain":CANDIDATE_CAPTURE_DOMAIN,
                    "clock_domain":self.clock.domain, "start_ns":start_ns, "end_ns":self.clock.now_ns(), "capture_rejected":reason, "execution_verified":false})) { self.error(error.to_string()); }
                None
            }
        }
    }
}

impl PendingCapture {
    pub(crate) fn observe_before_commit(&mut self, bank: &Bank, executed: &ExecutedTransaction) {
        self.before_commit_ns = Some(self.sink.clock.now_ns());
        if !authoritative_prestate_matches(bank, &self.input.accounts)
            || !authoritative_prestate_matches(bank, &self.checkpoint_pre_accounts)
        {
            self.rejection = Some("authoritative prestate changed before commit".into());
            return;
        }
        let Some(boundary) = executed.execution_details.candidate.as_ref() else {
            self.rejection = Some("execution boundary capture absent".into());
            return;
        };
        if boundary.compute_unit_limit != self.input.compute_unit_limit
            || executed.loaded_transaction.fee_details.total_fee() != 0
        {
            self.rejection = Some("captured fee/compute policy mismatch".into());
            return;
        }
        let loaded = &executed.loaded_transaction;
        let details = &executed.execution_details;
        let inner = details.inner_instructions.as_ref().map(|groups| {
            groups
                .iter()
                .map(|group| {
                    group.iter().map(|ix| json!({
            "program_id_index":ix.instruction.program_id_index, "accounts":ix.instruction.accounts,
            "data":ix.instruction.data, "stack_height":ix.stack_height,
        })).collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });
        self.execution = Some(ExecutionBoundary {
            accounts: boundary
                .accounts
                .iter()
                .map(|(key, account)| account_entry(key, Some(account)))
                .collect(),
            tentative_accounts: loaded
                .accounts
                .iter()
                .map(|(key, account)| account_entry(key, Some(account)))
                .collect(),
            rollback_accounts: loaded
                .rollback_accounts
                .iter()
                .map(|(key, account)| account_entry(key, Some(account)))
                .collect(),
            touched_flags: loaded.touched_flags.to_vec(),
            status: serde_json::to_value(&details.status).unwrap(),
            executed_units: details.executed_units,
            fee: loaded.fee_details.total_fee(),
            loaded_accounts_data_size: loaded.loaded_accounts_data_size,
            compute_unit_limit: boundary.compute_unit_limit,
            heap_size: boundary.heap_size,
            execution_cost_debug: boundary.execution_cost_debug.clone(),
            logs: details.log_messages.clone(),
            inner_instructions: json!(inner),
            return_data_program_id: boundary.return_data.program_id.to_string(),
            return_data: boundary.return_data.data.clone(),
        });
    }
    pub(crate) fn finish(mut self, bank: &Bank, result: Option<&TransactionCommitResult>) {
        let end_ns = self.sink.clock.now_ns();
        let post = bank_accounts(bank, &self.input);
        let checkpoint_committed_accounts: Vec<_> = self
            .state_keys
            .iter()
            .map(|key| account_entry(key, bank.get_account(key).as_ref()))
            .collect();
        if let (Some(execution), Some(Ok(committed))) = (&self.execution, result) {
            if serde_json::to_value(&committed.status).unwrap() != execution.status
                || committed.executed_units != execution.executed_units
                || committed.fee_details.total_fee() != execution.fee
                || committed.loaded_account_stats.loaded_accounts_data_size
                    != execution.loaded_accounts_data_size
            {
                self.rejection = Some("execution/commit result mismatch".into());
            }
        } else {
            self.rejection
                .get_or_insert("transaction not fully executed and committed".into());
        }
        if let Some(reason) = &self.rejection {
            self.sink.error(reason.clone());
        }
        let commit_metadata = result.and_then(|value|value.as_ref().ok()).map(|committed| json!({
            "loaded_accounts_data_size":committed.loaded_account_stats.loaded_accounts_data_size,
            "loaded_accounts_count":committed.loaded_account_stats.loaded_accounts_count,
            "fee_payer_post_balance":committed.fee_payer_post_balance,
        }));
        let signed_transaction_sha256 =
            solana_runtime::bank::er_vrf_candidate::digest(&self.input.signed_transaction);
        let input_sha256 = solana_runtime::bank::er_vrf_candidate::digest(&canonical_json(
            serde_json::to_value(&self.input).unwrap(),
        ));
        let output_sha256 = solana_runtime::bank::er_vrf_candidate::digest(&canonical_json(
            json!({"execution":self.execution,"committed_accounts":post}),
        ));
        let value = json!({"domain":CANDIDATE_CAPTURE_DOMAIN, "version":1, "sequence":self.sequence,
            "collector_sha256":self.sink.collector_sha256, "signed_transaction_sha256":signed_transaction_sha256,
            "input_sha256":input_sha256, "output_sha256":output_sha256,
            "digest_encoding":"sorted-key-utf8-json-no-whitespace-v1", "output_digest_scope":["execution","committed_accounts"], "candidate_proof_kind":3,"candidate_proof_version":1,
            "candidate_guest_sha256":null,"candidate_key_sha256":null,
            "clock_id":self.sink.clock.domain, "clock_domain":self.sink.clock.domain,
            "observation_id":format!("{}:transaction:{}",self.sink.clock.domain,self.sequence), "start_ns":self.start_ns, "before_commit_ns":self.before_commit_ns, "end_ns":end_ns,
            "atomic_bank_operation_lock":true, "prestate_rechecked_before_commit":self.rejection.is_none(),
            "capture_rejected":self.rejection, "input":self.input, "execution":self.execution, "committed_accounts":post,"commit_metadata":commit_metadata,
            "checkpoint_state_scope":"ER delegated/touched plus current writable keys",
            "checkpoint_pre_accounts":self.checkpoint_pre_accounts,"checkpoint_committed_accounts":checkpoint_committed_accounts,
            "execution_verified":false, "legacy_proof_input":false});
        if let Err(error) = self
            .sink
            .write_json(&format!("{:05}.json", self.sequence), &value)
        {
            self.sink.error(error.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_digests_preserve_large_integers_and_key_order() {
        let value = json!({"z":u64::MAX,"a":{"y":0,"b":[1,2]}});
        assert_eq!(
            canonical_json(value),
            br#"{"a":{"b":[1,2],"y":0},"z":18446744073709551615}"#
        );
    }

    #[test]
    fn authoritative_absence_and_prestate_drift_are_rejected() {
        let bank = Bank::new_for_tests(&solana_genesis_config::GenesisConfig::new(&[], &[]));
        let present = Pubkey::new_unique();
        let absent = Pubkey::new_unique();
        let owner = solana_sdk_ids::system_program::id();
        bank.store_account(
            &present,
            &solana_account::AccountSharedData::new(1_000_000, 0, &owner),
        );
        let entries = vec![
            account_entry(&present, bank.get_account(&present).as_ref()),
            account_entry(&absent, None),
        ];
        assert!(authoritative_prestate_matches(&bank, &entries));
        bank.store_account(
            &absent,
            &solana_account::AccountSharedData::new(1_000_000, 0, &owner),
        );
        assert!(!authoritative_prestate_matches(&bank, &entries));
        let updated = vec![
            account_entry(&present, bank.get_account(&present).as_ref()),
            account_entry(&absent, bank.get_account(&absent).as_ref()),
        ];
        assert!(authoritative_prestate_matches(&bank, &updated));
        bank.store_account(
            &present,
            &solana_account::AccountSharedData::new(2_000_000, 0, &owner),
        );
        assert!(!authoritative_prestate_matches(&bank, &updated));
    }

    #[test]
    fn candidate_clock_and_sink_are_explicit_bounded_and_no_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("capture");
        assert_ne!(
            CaptureClock::new("same-prefix".into()).domain,
            CaptureClock::new("same-prefix".into()).domain
        );
        let clock = CaptureClock::new("test-monotonic".into());
        let sink = CandidateCaptureSink::new(
            &path,
            Arc::clone(&clock),
            2,
            json!({"synthetic_unit_test":true}),
        )
        .unwrap();
        let first = clock.now_ns();
        assert!(clock.now_ns() >= first);
        assert!(CandidateCaptureSink::new(&path, clock, 2, json!({})).is_err());
        assert!(CandidateCaptureSink::new(
            &temp.path().join("invalid"),
            CaptureClock::new("other".into()),
            513,
            json!({})
        )
        .is_err());
        assert!(sink.errors().is_empty());
        sink.finish().unwrap();
        assert!(sink.finish().is_err());
    }
}
