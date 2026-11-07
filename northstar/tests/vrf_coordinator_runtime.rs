//! Opt-in local ER diagnostic. Genesis programs and credited balances are synthetic setup;
//! requests, verifier CPI, callbacks, and closes execute through actual EphemeralRuntime RPC.
use {
    northstar::{EphemeralRollupSettings, EphemeralRuntime},
    serde_json::{json, Value},
    solana_account::{
        state_traits::StateMutWincode, AccountSharedData, ReadableAccount, WritableAccount,
    },
    solana_commitment_config::CommitmentConfig,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_genesis_config::GenesisConfig,
    solana_gossip::{cluster_info::ClusterInfo, contact_info::ContactInfo},
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_loader_v3_interface::state::UpgradeableLoaderState,
    solana_net_utils::SocketAddrSpace,
    solana_pubkey::Pubkey,
    solana_rpc_client::rpc_client::RpcClient,
    solana_rpc_client_types::config::RpcSendTransactionConfig,
    solana_runtime::bank::Bank,
    solana_sdk_ids::{bpf_loader_upgradeable, system_program, sysvar},
    solana_sha256_hasher::hashv,
    solana_signer::Signer,
    solana_transaction::Transaction,
    solana_transaction_status::UiTransactionEncoding,
    std::{
        fs::{self, File},
        io::{BufWriter, Write},
        net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
        path::{Path, PathBuf},
        process::Command,
        sync::Arc,
        time::{Duration, Instant},
    },
};

const COORDINATOR: Pubkey = Pubkey::from_str_const("FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N");
const VERIFY: Pubkey = Pubkey::from_str_const("FUBz1nvSSoHnMqFoK2CA3jF5ckpYPdF3ZT4SoLzDNzh1");
const CONSUMER: Pubkey = Pubkey::from_str_const("HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt");
const SOURCE_REVISION: &str = "d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd";

struct RunningEr(EphemeralRuntime);
impl Drop for RunningEr {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

struct Journal {
    file: BufWriter<File>,
    transactions: PathBuf,
    capture_sequence: u64,
    #[cfg(not(feature = "vrf-candidate-capture"))]
    origin: Instant,
    #[cfg(feature = "vrf-candidate-capture")]
    clock: Arc<northstar::vrf_candidate_capture::CaptureClock>,
}
impl Journal {
    fn new(path: &Path) -> Self {
        let transactions = path.with_extension("transactions");
        fs::create_dir(&transactions).unwrap();
        Self {
            file: BufWriter::new(
                File::options()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .unwrap(),
            ),
            transactions,
            capture_sequence: 0,
            #[cfg(not(feature = "vrf-candidate-capture"))]
            origin: Instant::now(),
            #[cfg(feature = "vrf-candidate-capture")]
            clock: northstar::vrf_candidate_capture::CaptureClock::new(format!(
                "northstar.er-vrf.monotonic.v1/{}",
                Pubkey::new_unique()
            )),
        }
    }

    fn now_ns(&self) -> u64 {
        #[cfg(feature = "vrf-candidate-capture")]
        {
            self.clock.now_ns()
        }
        #[cfg(not(feature = "vrf-candidate-capture"))]
        {
            u64::try_from(self.origin.elapsed().as_nanos()).unwrap()
        }
    }

    fn record(&mut self, value: Value) {
        let mut value = value;
        value["capture_sequence"] = json!(self.capture_sequence);
        value["monotonic_ns"] = json!(self.now_ns());
        self.capture_sequence += 1;
        #[cfg(feature = "vrf-candidate-capture")]
        let value = {
            let mut value = value;
            value["clock_id"] = json!(self.clock.domain);
            value["observation_id"] = json!(format!(
                "{}:journal:{}",
                self.clock.domain,
                value["capture_sequence"].as_u64().unwrap()
            ));
            value["clock_domain"] = json!(self.clock.domain);
            value["observed_ns"] = json!(self.clock.now_ns());
            value
        };
        serde_json::to_writer(&mut self.file, &value).unwrap();
        writeln!(self.file).unwrap();
        self.file.flush().unwrap();
    }

    fn transaction(&mut self, label: &str, tx: &Transaction) {
        let wire = bincode::serialize(tx).unwrap();
        let name = format!("{}.bin", label.replace(':', "-"));
        let path = self.transactions.join(&name);
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.write_all(&wire).unwrap();
        let digest: String = hashv(&[&wire])
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        self.record(
            json!({"kind":"wire_transaction", "label":label, "file":name,
            "sha256":digest, "bytes":wire.len(), "signature":tx.signatures[0].to_string()}),
        );
    }
}

fn instruction(
    program_id: Pubkey,
    name: &str,
    accounts: Vec<AccountMeta>,
    args: &[u8],
) -> Instruction {
    let mut data = hashv(&[format!("global:{name}").as_bytes()]).to_bytes()[..8].to_vec();
    data.extend_from_slice(args);
    Instruction {
        program_id,
        accounts,
        data,
    }
}

fn address(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

fn curve_enabled_genesis() -> GenesisConfig {
    let mut genesis = GenesisConfig::new(&[], &[]);
    solana_runtime::genesis_utils::activate_feature(
        &mut genesis,
        agave_feature_set::curve25519_syscall_enabled::id(),
    );
    genesis
}

fn load_program(bank: &Bank, id: Pubkey, file: &Path, authority: Pubkey, journal: &mut Journal) {
    let elf = fs::read(file).unwrap();
    let digest: String = hashv(&[&elf])
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    journal.record(json!({"kind":"program", "program_id":id.to_string(), "elf_sha256":digest,
        "elf_bytes":elf.len(), "loader":bpf_loader_upgradeable::id().to_string(), "synthetic_genesis_setup":true}));
    let data_address = address(&[id.as_ref()], &bpf_loader_upgradeable::id());
    let mut program = AccountSharedData::new(
        10_000_000,
        UpgradeableLoaderState::size_of_program(),
        &bpf_loader_upgradeable::id(),
    );
    program
        .set_state(&UpgradeableLoaderState::Program {
            programdata_address: data_address,
        })
        .unwrap();
    program.set_executable(true);
    bank.store_account(&id, &program);
    let mut data = AccountSharedData::new(
        100_000_000,
        UpgradeableLoaderState::size_of_programdata(elf.len()),
        &bpf_loader_upgradeable::id(),
    );
    data.set_state(&UpgradeableLoaderState::ProgramData {
        slot: 0,
        upgrade_authority_address: Some(authority),
    })
    .unwrap();
    data.data_as_mut_slice()[UpgradeableLoaderState::size_of_programdata_metadata()..]
        .copy_from_slice(&elf);
    bank.store_account(&data_address, &data);
}

fn ports() -> [SocketAddr; 3] {
    let start = 22_000 + (std::process::id() % 500) as u16;
    let mut result = Vec::new();
    for port in start..25_000 {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        if let (Ok(tcp), Ok(udp)) = (
            TcpListener::bind(addr),
            solana_net_utils::sockets::bind_to(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        ) {
            drop((tcp, udp));
            result.push(addr);
            if result.len() == 3 {
                return result.try_into().unwrap();
            }
        }
    }
    panic!("No local diagnostic ports available");
}

fn wait_for(label: &str, mut check: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if check() {
            return;
        }
        assert!(Instant::now() < until, "Timed out: {label}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn send(client: &RpcClient, signer: &Keypair, label: &str, ix: Instruction, journal: &mut Journal) {
    send_expected(client, signer, label, ix, journal, None);
}

fn send_expected(
    client: &RpcClient,
    signer: &Keypair,
    label: &str,
    ix: Instruction,
    journal: &mut Journal,
    expected_error: Option<u64>,
) {
    let mut keys = vec![
        signer.pubkey(),
        ix.program_id,
        sysvar::clock::id(),
        sysvar::rent::id(),
    ];
    keys.extend(ix.accounts.iter().map(|meta| meta.pubkey));
    keys.sort_unstable();
    keys.dedup();
    let before = client.get_multiple_accounts(&keys).unwrap();
    journal.record(json!({"kind":"accounts_before", "label":label,
        "addresses":keys.iter().map(ToString::to_string).collect::<Vec<_>>(), "accounts":before,
        "consistency":"non-atomic RPC observations; not an execution witness"}));
    let blockhash = client.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
            ix,
        ],
        Some(&signer.pubkey()),
        &[signer],
        blockhash,
    );
    journal.transaction(label, &tx);
    let started = Instant::now();
    let submitted_ns = journal.now_ns();
    let result = client.send_transaction_with_config(
        &tx,
        RpcSendTransactionConfig {
            skip_preflight: expected_error.is_some(),
            preflight_commitment: Some(CommitmentConfig::processed().commitment),
            ..Default::default()
        },
    );
    journal.record(json!({"kind":"submission", "label":label, "submitted_ns":submitted_ns, "signature":tx.signatures[0].to_string(), "result":format!("{result:?}")}));
    let signature = result.unwrap_or_else(|e| panic!("{label}: {e:?}"));
    wait_for(label, || {
        client.get_signature_status(&signature).unwrap().is_some()
    });
    let mut observation = None;
    wait_for("transaction history", || {
        observation = client
            .get_transaction(&signature, UiTransactionEncoding::Base64)
            .ok();
        observation.is_some()
    });
    let observation = observation.unwrap();
    journal.record(json!({"kind":"transaction", "label":label, "elapsed_ms":started.elapsed().as_millis(), "signature":signature.to_string(), "expected_error":expected_error, "observation":observation}));
    let after = client.get_multiple_accounts(&keys).unwrap();
    journal.record(json!({"kind":"accounts_after", "label":label,
        "addresses":keys.iter().map(ToString::to_string).collect::<Vec<_>>(), "accounts":after,
        "consistency":"non-atomic RPC observations; not an execution witness"}));
    let error = &observation.transaction.meta.as_ref().unwrap().err;
    match expected_error {
        Some(code) => {
            assert_eq!(
                serde_json::to_value(error).unwrap()["InstructionError"][1]["Custom"],
                code
            );
            let expected_name = match code {
                6003 => "InvalidProofChallenge",
                6001 => "DeliberateFailure",
                _ => panic!("Unsupported diagnostic error: {code}"),
            };
            assert!(
                serde_json::to_string(&observation.transaction.meta)
                    .unwrap()
                    .contains(expected_name),
                "Expected named program rejection {expected_name}"
            );
        }
        None => assert!(error.is_none(), "{label}: {error:?}"),
    }
}

fn assert_capture_source_identity(compiled: &[u8], observed: &[u8]) {
    assert_eq!(
        hashv(&[compiled]),
        hashv(&[observed]),
        "capture harness source drift: rebuild after formatting/editing"
    );
}

#[test]
fn vrf_coordinator_runtime_source_drift_is_rejected() {
    assert_capture_source_identity(b"same", b"same");
    assert!(
        std::panic::catch_unwind(|| assert_capture_source_identity(b"compiled", b"changed"))
            .is_err()
    );
}

fn reference_proof(alpha: &[u8]) -> Value {
    let hex: String = alpha.iter().map(|b| format!("{b:02x}")).collect();
    let output = Command::new("bun")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/vrf-coordinator-runtime-proof.ts"))
        .arg(hex)
        .output()
        .expect("Existing Bun required");
    assert!(
        output.status.success(),
        "Reference helper: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

struct PortalCaptureSession {
    #[cfg(feature = "vrf-candidate-capture")]
    identity: Value,
    #[cfg(feature = "vrf-candidate-capture")]
    identity_sha256: String,
    session: Pubkey,
    portal: Pubkey,
    grid_id: u64,
    ttl_slots: u64,
    fee_cap: u64,
    manager: Arc<Keypair>,
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn vrf_coordinator_runtime_session_hex_encoding_is_exact() {
    assert_eq!(decode_hex("0001feff"), vec![0, 1, 254, 255]);
    assert!(std::panic::catch_unwind(|| decode_hex("0")).is_err());
}

fn portal_capture_session() -> Option<PortalCaptureSession> {
    let path = std::env::var_os("VRF_PORTAL_SESSION_JSON")?;
    let raw = fs::read(path).unwrap();
    let identity: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(
        identity["schema"],
        "northstar.vrf-candidate-portal-session.v1"
    );
    let key = |name: &str| identity[name].as_str().unwrap().parse::<Pubkey>().unwrap();
    let portal = key("program_id");
    let session = key("session");
    let validator = key("validator");
    let grid_id = identity["grid_id"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let ttl_slots = identity["ttl_slots"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let nonce = identity["nonce"].as_str().unwrap().parse::<u128>().unwrap();
    assert_eq!(
        session,
        Pubkey::find_program_address(&[b"session"], &portal).0
    );
    let context = northstar_transaction_proof::session_context_bytes_v1(
        portal.to_bytes(),
        session.to_bytes(),
        grid_id,
        nonce,
        validator.to_bytes(),
    );
    assert_eq!(
        decode_hex(identity["session_context_preimage_hex"].as_str().unwrap()),
        context
    );
    let manager = Arc::new(
        solana_keypair::read_keypair_file(
            std::env::var_os("VRF_PORTAL_SESSION_SIGNER")
                .expect("explicit local throwaway session signer required"),
        )
        .unwrap(),
    );
    assert_eq!(
        manager.pubkey(),
        validator,
        "actual session validator signer mismatch"
    );
    let wire = fs::read(
        std::env::var_os("VRF_PORTAL_SESSION_OPEN_WIRE")
            .expect("signed local OpenSession wire required"),
    )
    .unwrap();
    let opened: solana_transaction::versioned::VersionedTransaction =
        bincode::deserialize(&wire).unwrap();
    opened
        .verify_and_hash_message()
        .expect("OpenSession signature verification");
    assert_eq!(
        bincode::serialize(&opened).unwrap(),
        wire,
        "canonical OpenSession wire required"
    );
    let instructions: Vec<_> = opened
        .message
        .instructions()
        .iter()
        .filter(|ix| opened.message.static_account_keys()[ix.program_id_index as usize] == portal)
        .collect();
    assert_eq!(instructions.len(), 1);
    let instruction = instructions[0];
    let data = &instruction.data;
    assert_eq!(data.len(), 65);
    assert_eq!(data[0], 0);
    assert_eq!(u64::from_le_bytes(data[1..9].try_into().unwrap()), grid_id);
    assert_eq!(
        u64::from_le_bytes(data[9..17].try_into().unwrap()),
        ttl_slots
    );
    let fee_cap = u64::from_le_bytes(data[17..25].try_into().unwrap());
    assert_eq!(&data[25..57], validator.as_ref());
    assert_eq!(
        opened.message.static_account_keys()[usize::from(instruction.accounts[0])],
        validator
    );
    assert_eq!(
        opened.message.static_account_keys()[usize::from(instruction.accounts[1])],
        session
    );
    Some(PortalCaptureSession {
        #[cfg(feature = "vrf-candidate-capture")]
        identity,
        #[cfg(feature = "vrf-candidate-capture")]
        identity_sha256: solana_runtime_digest(&raw),
        session,
        portal,
        grid_id,
        ttl_slots,
        fee_cap,
        manager,
    })
}

#[cfg(feature = "vrf-candidate-capture")]
fn solana_runtime_digest(bytes: &[u8]) -> String {
    hashv(&[bytes])
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn bytes(value: &Value, key: &str) -> Vec<u8> {
    value[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| u8::try_from(n.as_u64().unwrap()).unwrap())
        .collect()
}

#[test]
fn vrf_coordinator_runtime_parent_enables_required_curve_syscalls() {
    let feature = agave_feature_set::curve25519_syscall_enabled::id();
    assert!(!GenesisConfig::new(&[], &[]).accounts.contains_key(&feature));
    let bank = Bank::new_for_tests(&curve_enabled_genesis());
    assert!(bank.feature_set.is_active(&feature));
}

#[test]
fn vrf_coordinator_runtime_journal_preserves_records_and_wire_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.jsonl");
    let mut journal = Journal::new(&path);
    journal.record(json!({"kind":"synthetic-unit-record", "data":vec![7u8; 131_072]}));
    let signer = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[],
        Some(&signer.pubkey()),
        &[&signer],
        solana_hash::Hash::new_unique(),
    );
    journal.transaction("unit:wire", &tx);
    let records: Vec<Value> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["data"].as_array().unwrap().len(), 131_072);
    let captured = fs::read(path.with_extension("transactions").join("unit-wire.bin")).unwrap();
    let decoded: Transaction = bincode::deserialize(&captured).unwrap();
    assert_eq!(decoded, tx);
    assert_eq!(records[1]["bytes"], captured.len());
}

#[test]
fn vrf_coordinator_runtime_alpha_and_verify_abi_golden() {
    let request = Pubkey::new_from_array([7; 32]);
    let alpha = hashv(&[
        b"sonic-vrf/v1",
        COORDINATOR.as_ref(),
        request.as_ref(),
        &[1; 32],
        &100u64.to_le_bytes(),
        &99u64.to_le_bytes(),
        &[2; 32],
    ])
    .to_bytes();
    let hex: String = alpha.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "85d739f7f781f84ed02f7f3f15be1c87f985a893a464b5355c94501bb15cdefb"
    );
    let mut args = vec![1];
    args.extend_from_slice(&[7; 32]);
    args.extend_from_slice(&alpha);
    args.extend_from_slice(&80u32.to_le_bytes());
    args.extend_from_slice(&[3; 80]);
    let verify = instruction(VERIFY, "verify", vec![], &args);
    assert_eq!(&verify.data[..8], &[133, 161, 141, 48, 120, 198, 88, 150]);
    assert_eq!(verify.data.len(), 157);
    assert!(verify.accounts.is_empty());
}

#[test]
#[ignore = "requires freshly built pinned library, Bun reference dependencies, and explicit local \
            diagnostic environment"]
fn vrf_coordinator_runtime_request_verify_callback() {
    assert_eq!(std::env::var("NORTHSTAR_SANDBOX").unwrap(), "1");
    let artifact_dir = PathBuf::from(
        std::env::var_os("VRF_COORDINATOR_ARTIFACT_DIR").expect("VRF_COORDINATOR_ARTIFACT_DIR"),
    );
    assert_eq!(
        fs::read_to_string(artifact_dir.join("source-revision.txt"))
            .unwrap()
            .trim(),
        SOURCE_REVISION
    );
    let cycles: u32 = std::env::var("VRF_RUNTIME_CYCLES")
        .unwrap_or_else(|_| "1".into())
        .parse()
        .unwrap();
    assert!((1..=100).contains(&cycles));
    let scenario = std::env::var("VRF_RUNTIME_SCENARIO").unwrap_or_else(|_| "success".into());
    assert!(scenario == "success" || scenario == "callback_failure");
    let callback_failure = scenario == "callback_failure";
    assert!(!callback_failure || cycles == 1);
    let minimum_tries: Option<u64> = std::env::var("VRF_RUNTIME_MIN_TRIES")
        .ok()
        .map(|value| value.parse().unwrap());
    assert!(minimum_tries
        .is_none_or(|value| (1..=20).contains(&value) && cycles == 1 && !callback_failure));
    let slot_duration_ms: u64 = std::env::var("VRF_RUNTIME_SLOT_MS")
        .unwrap_or_else(|_| "50".into())
        .parse()
        .unwrap();
    assert!((1..=10_000).contains(&slot_duration_ms));
    let journal_path =
        PathBuf::from(std::env::var_os("VRF_RUNTIME_JOURNAL").expect("VRF_RUNTIME_JOURNAL"));
    let mut journal = Journal::new(&journal_path);
    journal.record(json!({"kind":"setup", "level":"local-er-diagnostic", "source_revision":SOURCE_REVISION,
        "cycles":cycles, "scenario":scenario, "synthetic_setup":["genesis upgradeable program accounts", "explicit curve syscall genesis feature", "credit_deposit without L1 event"],
        "delegated_handoff":false, "production_verifier":false, "execution_proof":false}));
    let portal_session = portal_capture_session();
    if portal_session.is_some() {
        assert_eq!(
            slot_duration_ms, 50,
            "matching-session candidate profile uses the actual 900-age runtime"
        );
    }
    let session_pda = portal_session
        .as_ref()
        .map_or_else(Pubkey::new_unique, |value| value.session);
    let grid_id = portal_session.as_ref().map_or(0, |value| value.grid_id);
    let ttl_slots = portal_session
        .as_ref()
        .map_or(100_000, |value| value.ttl_slots);
    let fee_cap = portal_session.as_ref().map_or(1000, |value| value.fee_cap);
    let portal_program_id = portal_session
        .as_ref()
        .map_or_else(Pubkey::new_unique, |value| value.portal);
    let manager = portal_session.as_ref().map_or_else(
        || Arc::new(Keypair::new()),
        |value| Arc::clone(&value.manager),
    );
    #[cfg(feature = "vrf-candidate-capture")]
    let manager_pubkey = manager.pubkey();
    let payer = Keypair::new();
    let fulfiller = Keypair::new();
    let genesis = curve_enabled_genesis();
    let genesis_sha256: String = genesis
        .hash()
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    journal.record(
        json!({"kind":"genesis_identity", "genesis_config_sha256":genesis_sha256,
        "synthetic_genesis":true, "program_accounts_installed_before_er":true}),
    );
    let bank = Bank::new_for_tests(&genesis);
    for (id, name) in [
        (COORDINATOR, "vrf_coordinator"),
        (VERIFY, "reference_verify"),
        (CONSUMER, "test_consumer"),
    ] {
        load_program(
            &bank,
            id,
            &artifact_dir.join(format!("deploy/{name}.so")),
            payer.pubkey(),
            &mut journal,
        );
    }
    bank.freeze();
    let node = Arc::new(Keypair::new());
    let cluster = Arc::new(ClusterInfo::new(
        ContactInfo::new_localhost(&node.pubkey(), solana_time_utils::timestamp()),
        node,
        SocketAddrSpace::Unspecified,
    ));
    let [rpc, ws, tpu] = ports();
    journal.record(json!({"kind":"runtime_starting","runtime_start_ns":journal.now_ns()}));
    let mut runtime = RunningEr(
        EphemeralRuntime::new_with_slot_duration(
            Arc::new(bank),
            cluster,
            EphemeralRollupSettings {
                session_pda,
                grid_id,
                ttl_slots,
                fee_cap,
                er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
                delegated_accounts: vec![],
            },
            rpc,
            ws,
            tpu,
            portal_program_id,
            manager,
            Duration::from_millis(slot_duration_ms),
            solana_rpc::er_history::DEFAULT_MAX_RETAINED_SLOTS,
        )
        .unwrap(),
    );
    if portal_session.is_some() {
        runtime.0.set_session_pda(session_pda);
        runtime.0.set_session_settings(grid_id, ttl_slots, fee_cap);
    }
    runtime.0.activate();
    journal.record(json!({"kind":"runtime_started","runtime_started_ns":journal.now_ns()}));
    #[cfg(feature = "vrf-candidate-capture")]
    let atomic_sink = if let Some(path) = std::env::var_os("VRF_ATOMIC_CAPTURE_DIR") {
        assert_capture_source_identity(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/vrf_coordinator_runtime.rs"
            )),
            &fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vrf_coordinator_runtime.rs"),
            )
            .unwrap(),
        );
        let binary = fs::read(std::env::current_exe().unwrap()).unwrap();
        let binary_sha256: String = hashv(&[&binary])
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let source_revision = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(source_revision.status.success());
        let compiler = Command::new("rustc")
            .args(["--version", "--verbose"])
            .output()
            .unwrap();
        assert!(compiler.status.success());
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tracked_diff = Command::new("git")
            .args(["diff", "--no-ext-diff", "--binary"])
            .current_dir(source_root)
            .output()
            .unwrap();
        assert!(tracked_diff.status.success());
        let untracked = Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard"])
            .current_dir(source_root)
            .output()
            .unwrap();
        assert!(untracked.status.success());
        let untracked_sources: Vec<_> = String::from_utf8(untracked.stdout).unwrap().lines().map(|path| json!({"path":path,
            "sha256":solana_runtime::bank::er_vrf_candidate::digest(&fs::read(source_root.join(path)).unwrap())})).collect();
        let mut feature_identity: Vec<_> = runtime
            .0
            .bank()
            .feature_set
            .active()
            .iter()
            .map(|(key, slot)| (key.to_string(), *slot))
            .collect();
        feature_identity.sort_unstable();
        let sink = northstar::vrf_candidate_capture::CandidateCaptureSink::new(Path::new(&path), Arc::clone(&journal.clock), 256,
            json!({"library_source_revision":SOURCE_REVISION, "runtime_source_revision":String::from_utf8(source_revision.stdout).unwrap().trim(),
                "runtime_binary_sha256":binary_sha256, "genesis_config_sha256":genesis_sha256,
                "l1_genesis_sha256":null,"runtime_source_dirty":!tracked_diff.stdout.is_empty() || !untracked_sources.is_empty(),
                "rustc_version":String::from_utf8(compiler.stdout).unwrap(),"rustflags":std::env::var("RUSTFLAGS").ok(),
                "runtime_tracked_diff_sha256":solana_runtime::bank::er_vrf_candidate::digest(&tracked_diff.stdout),"runtime_untracked_sources":untracked_sources,
                "feature_set_sha256":solana_runtime::bank::er_vrf_candidate::digest(&serde_json::to_vec(&feature_identity).unwrap()),
                "feature_set_digest_encoding":"JSON-sorted-base58-active-pairs-v1","active_features":feature_identity,
                "build_features":{"vrf-candidate-capture":true,"replay":cfg!(feature="replay"),"proof-coordinator":cfg!(feature="proof-coordinator")},
                "session_pda":runtime.0.session_pda().read().unwrap().map(|key|key.to_string()),"grid_id":grid_id,"ttl_slots":ttl_slots,"fee_cap":fee_cap,
                "portal_program_id":portal_program_id.to_string(),"manager":manager_pubkey.to_string(),
                "actual_portal_session":portal_session.as_ref().map(|value|&value.identity),"actual_portal_session_json_sha256":portal_session.as_ref().map(|value|&value.identity_sha256),
                "harness_source_sha256":solana_runtime::bank::er_vrf_candidate::digest(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vrf_coordinator_runtime.rs"))),
                "runtime_profile":"default-plus-curve25519-zero-fees", "measurement_target":"g5.xlarge/A10G", "local_capture_host":"sandbox",
                "synthetic_funding":true, "execution_verified":false})).unwrap();
        runtime.0.set_vrf_candidate_capture(Arc::clone(&sink));
        journal.record(json!({"kind":"atomic_capture_start", "start_ns":journal.clock.now_ns(), "runtime_binary_sha256":binary_sha256,
            "genesis_config_sha256":genesis_sha256, "collector_start_unix_ns":journal.clock.collector_start_unix_ns}));
        Some(sink)
    } else {
        None
    };
    runtime.0.credit_deposit(&payer.pubkey(), 10_000_000_000);
    runtime.0.credit_deposit(&fulfiller.pubkey(), 1_000_000_000);
    let mut active_features: Vec<_> = runtime
        .0
        .bank()
        .feature_set
        .active()
        .iter()
        .map(|(id, slot)| (id.to_string(), *slot))
        .collect();
    active_features.sort_unstable();
    assert!(
        runtime
            .0
            .bank()
            .feature_set
            .is_active(&agave_feature_set::curve25519_syscall_enabled::id()),
        "ER must inherit the curve syscall feature from its synthetic L1 parent"
    );
    journal.record(json!({"kind":"runtime", "active_features":active_features,
        "slot_duration_ms":slot_duration_ms, "lamports_per_signature":0, "lamports_per_write_lock":0,
        "compute_fee_bins":[], "replay_capture_enabled":false,
        "full_nested_vm_trace":false, "atomic_execution_snapshots":false}));
    let client =
        RpcClient::new_with_commitment(runtime.0.rpc_addr(), CommitmentConfig::processed());
    wait_for("RPC readiness", || client.get_latest_blockhash().is_ok());
    let config = address(&[b"config"], &COORDINATOR);
    let program_data = address(&[COORDINATOR.as_ref()], &bpf_loader_upgradeable::id());
    send(
        &client,
        &payer,
        "initialize",
        instruction(
            COORDINATOR,
            "initialize",
            vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(payer.pubkey(), true),
                AccountMeta::new_readonly(payer.pubkey(), true),
                AccountMeta::new_readonly(program_data, false),
                AccountMeta::new(config, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            payer.pubkey().as_ref(),
        ),
        &mut journal,
    );
    let proof = reference_proof(&[0; 32]);
    let mut args = bytes(&proof, "key");
    args.push(1);
    let authority_accounts = vec![
        AccountMeta::new_readonly(payer.pubkey(), true),
        AccountMeta::new(config, false),
    ];
    send(
        &client,
        &payer,
        "update_vrf_key",
        instruction(
            COORDINATOR,
            "update_vrf_key",
            authority_accounts.clone(),
            &args,
        ),
        &mut journal,
    );
    let mut args = 1u32.to_le_bytes().to_vec();
    args.extend_from_slice(fulfiller.pubkey().as_ref());
    send(
        &client,
        &payer,
        "update_fulfillers",
        instruction(
            COORDINATOR,
            "update_fulfillers",
            authority_accounts.clone(),
            &args,
        ),
        &mut journal,
    );
    let fee = 5_000u64;
    send(
        &client,
        &payer,
        "update_request_fee",
        instruction(
            COORDINATOR,
            "update_request_fee",
            authority_accounts,
            &fee.to_le_bytes(),
        ),
        &mut journal,
    );
    let identity = address(&[b"vrf_requester"], &CONSUMER);
    let callback_signer = address(&[b"vrf_callback", CONSUMER.as_ref()], &COORDINATOR);
    for cycle in 0..cycles {
        let mut key = [0u8; 32];
        key[..4].copy_from_slice(&cycle.to_le_bytes());
        key[31] = 0x71;
        let mut seed = [0x19u8; 32];
        let slot = address(&[b"slot", &key], &CONSUMER);
        let request = address(&[b"request", CONSUMER.as_ref(), &key], &COORDINATOR);
        let mut args = key.to_vec();
        args.push(u8::from(callback_failure)); // Behaviour::Ok or Behaviour::Fail
        send(
            &client,
            &payer,
            &format!("{cycle}:set_behaviour"),
            instruction(
                CONSUMER,
                "set_behaviour",
                vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new(slot, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                ],
                &args,
            ),
            &mut journal,
        );
        let predicted = minimum_tries.map(|minimum| {
            let selection_bank = runtime.0.bank();
            let clock = selection_bank.get_account(&sysvar::clock::id()).unwrap();
            let hashes = selection_bank
                .get_account(&sysvar::slot_hashes::id())
                .unwrap();
            let context = json!({"coordinator":COORDINATOR.to_bytes(),"request":request.to_bytes(),
                "request_slot":u64::from_le_bytes(clock.data()[..8].try_into().unwrap()),
                "hash_slot":u64::from_le_bytes(hashes.data()[8..16].try_into().unwrap()),
                "slot_hash":hashes.data()[16..48],"minimum_tries":minimum});
            let output = Command::new("bun")
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("scripts/vrf-coordinator-runtime-proof.ts"),
                )
                .arg("--search-seed")
                .arg(context.to_string())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let prediction: Value = serde_json::from_slice(&output.stdout).unwrap();
            seed.copy_from_slice(&bytes(&prediction, "seed"));
            journal.record(
                json!({"kind":"harder_seed_selection","context":context,"prediction":prediction,
                "not_execution_witness":true,"selection_source":"working-bank-not-rpc"}),
            );
            prediction
        });
        let cycle_started = Instant::now();
        let request_start_ns = journal.now_ns();
        let payer_before = client.get_balance(&payer.pubkey()).unwrap();
        let fulfiller_before = client.get_balance(&fulfiller.pubkey()).unwrap();
        let mut args = key.to_vec();
        args.extend_from_slice(&seed);
        args.extend_from_slice(&[0, 1]); // Mode::Callback, with_callback
        args.extend_from_slice(&20_000u32.to_le_bytes());
        args.push(1);
        args.extend_from_slice(&150u16.to_le_bytes());
        args.extend_from_slice(&0u32.to_le_bytes());
        send(
            &client,
            &payer,
            &format!("{cycle}:make_request"),
            instruction(
                CONSUMER,
                "make_request",
                vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new(slot, false),
                    AccountMeta::new_readonly(identity, false),
                    AccountMeta::new_readonly(CONSUMER, false),
                    AccountMeta::new_readonly(config, false),
                    AccountMeta::new(request, false),
                    AccountMeta::new_readonly(sysvar::slot_hashes::id(), false),
                    AccountMeta::new_readonly(COORDINATOR, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                ],
                &args,
            ),
            &mut journal,
        );
        let pending = client.get_account(&request).unwrap();
        assert_eq!(pending.owner, COORDINATOR);
        assert_eq!(pending.data.len(), 434);
        assert_eq!(&pending.data[8..11], &[1, 0, 0]);
        let request_slot = u64::from_le_bytes(pending.data[115..123].try_into().unwrap());
        let alpha = &pending.data[163..195];
        let expected_alpha = hashv(&[
            b"sonic-vrf/v1",
            COORDINATOR.as_ref(),
            request.as_ref(),
            &seed,
            &pending.data[115..123],
            &pending.data[123..131],
            &pending.data[131..163],
        ])
        .to_bytes();
        assert_eq!(alpha, expected_alpha);
        let proof = reference_proof(alpha);
        if let Some(prediction) = &predicted {
            assert_eq!(
                alpha,
                bytes(prediction, "predicted_alpha"),
                "slot/hash moved after seed selection; retain failed attempt and retry fresh"
            );
            assert!(proof["tries"].as_u64().unwrap() >= minimum_tries.unwrap());
        }
        assert_eq!(&pending.data[195..227], bytes(&proof, "key"));
        journal.record(json!({"kind":"request", "cycle":cycle, "address":request.to_string(), "account":pending,
            "proof":proof, "payer_before":payer_before, "fulfiller_before":fulfiller_before}));
        wait_for("strictly later fulfill slot", || {
            client.get_slot().unwrap() > request_slot
        });
        let proof_bytes = bytes(&proof, "proof");
        let mut args = (proof_bytes.len() as u32).to_le_bytes().to_vec();
        args.extend_from_slice(&proof_bytes);
        let fulfill_accounts = vec![
            AccountMeta::new(fulfiller.pubkey(), true),
            AccountMeta::new_readonly(config, false),
            AccountMeta::new(request, false),
            AccountMeta::new(payer.pubkey(), false),
            AccountMeta::new_readonly(callback_signer, false),
            AccountMeta::new_readonly(CONSUMER, false),
            AccountMeta::new_readonly(VERIFY, false),
            AccountMeta::new(slot, false),
        ];
        let negative_control =
            std::env::var("VRF_RUNTIME_NEGATIVE_CONTROL").is_ok_and(|value| value == "1");
        if cycle == 0
            && (negative_control || callback_failure || !cfg!(feature = "vrf-candidate-capture"))
        {
            let mut invalid_args = args.clone();
            invalid_args[4 + 32] ^= 1;
            send_expected(
                &client,
                &fulfiller,
                "invariant_invalid_proof_preserves_pending_request",
                instruction(
                    COORDINATOR,
                    "fulfill",
                    fulfill_accounts.clone(),
                    &invalid_args,
                ),
                &mut journal,
                Some(6003),
            );
            assert_eq!(client.get_account(&request).unwrap(), pending);
            assert_eq!(
                client.get_balance(&fulfiller.pubkey()).unwrap(),
                fulfiller_before
            );
            let undelivered = client.get_account(&slot).unwrap();
            assert_eq!(
                u32::from_le_bytes(undelivered.data[145..149].try_into().unwrap()),
                0
            );
            journal.record(json!({"kind":"invariant", "name":"invalid_proof_preserves_pending_request", "expected_error":6003,
                "request_unchanged":true, "deliveries":0, "fee_paid":0}));
        }
        send_expected(
            &client,
            &fulfiller,
            &format!("{cycle}:fulfill"),
            instruction(COORDINATOR, "fulfill", fulfill_accounts, &args),
            &mut journal,
            callback_failure.then_some(6001),
        );
        if callback_failure {
            assert_eq!(client.get_account(&request).unwrap(), pending);
            assert_eq!(
                client.get_balance(&fulfiller.pubkey()).unwrap(),
                fulfiller_before
            );
            assert_eq!(
                client.get_balance(&payer.pubkey()).unwrap(),
                payer_before - pending.lamports
            );
            let undelivered = client.get_account(&slot).unwrap();
            assert_eq!(
                u32::from_le_bytes(undelivered.data[145..149].try_into().unwrap()),
                0
            );
            #[cfg(feature = "vrf-candidate-capture")]
            if let Some(sink) = &atomic_sink {
                assert!(sink.errors().is_empty(), "{:?}", sink.errors());
                assert_eq!(sink.records(), 3);
                journal.record(json!({"kind":"atomic_capture_end", "end_ns":journal.clock.now_ns(), "records":sink.records()}));
            }
            drop(client);
            drop(runtime);
            journal.record(json!({"kind":"runtime_stopped","runtime_stopped_ns":journal.now_ns()}));
            journal.record(json!({"kind":"summary", "scenario":scenario, "completed":0,
                "callback_failure_rejected":true, "expected_error":6001, "pending_request_preserved":true,
                "fee_paid":0, "request_closed":false, "level":"local-er-diagnostic", "portal_resolution":false}));
            #[cfg(feature = "vrf-candidate-capture")]
            if let Some(sink) = &atomic_sink {
                sink.finish().unwrap();
            }
            return;
        }
        let delivered = client.get_account(&slot).unwrap();
        assert_eq!(delivered.owner, CONSUMER);
        assert_eq!(&delivered.data[41..73], request.as_ref());
        assert_eq!(&delivered.data[73..137], bytes(&proof, "output"));
        assert_eq!(
            u32::from_le_bytes(delivered.data[145..149].try_into().unwrap()),
            1,
            "exactly one callback"
        );
        assert!(
            client
                .get_account_with_commitment(&request, CommitmentConfig::processed())
                .unwrap()
                .value
                .is_none(),
            "request closed"
        );
        assert_eq!(
            client.get_balance(&payer.pubkey()).unwrap(),
            payer_before - fee,
            "rent refunded"
        );
        assert_eq!(
            client.get_balance(&fulfiller.pubkey()).unwrap(),
            fulfiller_before + fee,
            "fee paid once"
        );
        journal.record(
            json!({"kind":"callback", "cycle":cycle, "slot":slot.to_string(), "account":delivered,
            "request_closed":true, "deliveries":1, "fee_paid":fee, "rent_refunded":true,
            "request_to_callback_ms":cycle_started.elapsed().as_millis(), "request_start_ns":request_start_ns,
            "callback_observed_ns":journal.now_ns(), "includes_rejection_check":cycle == 0 && (negative_control || !cfg!(feature = "vrf-candidate-capture"))}),
        );
    }
    #[cfg(feature = "vrf-candidate-capture")]
    if let Some(sink) = &atomic_sink {
        assert!(sink.errors().is_empty(), "{:?}", sink.errors());
        assert_eq!(
            sink.records(),
            usize::try_from(
                cycles * 2
                    + u32::from(
                        std::env::var("VRF_RUNTIME_NEGATIVE_CONTROL")
                            .is_ok_and(|value| value == "1")
                    )
            )
            .unwrap()
        );
        journal.record(json!({"kind":"atomic_capture_end", "end_ns":journal.clock.now_ns(), "records":sink.records()}));
    }
    drop(client);
    drop(runtime);
    journal.record(json!({"kind":"runtime_stopped","runtime_stopped_ns":journal.now_ns()}));
    journal.record(
        json!({"kind":"summary", "completed":cycles, "lost_callbacks":0, "duplicate_callbacks":0,
        "level":"local-er-diagnostic", "hosted_handoff":false, "portal_resolution":false}),
    );
    #[cfg(feature = "vrf-candidate-capture")]
    if let Some(sink) = &atomic_sink {
        sink.finish().unwrap();
    }
}
