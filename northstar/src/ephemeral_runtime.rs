use {
    crate::{ephemeral_tx_client::EphemeralTransactionClient, EphemeralRollupSettings},
    log::info,
    solana_gossip::cluster_info::ClusterInfo,
    solana_ledger::{blockstore::Blockstore, leader_schedule_cache::LeaderScheduleCache},
    solana_pubkey::Pubkey,
    solana_rpc::{
        max_slots::MaxSlots, optimistically_confirmed_bank_tracker::OptimisticallyConfirmedBank,
        rpc::JsonRpcConfig, rpc_service::JsonRpcService,
    },
    solana_runtime::{
        bank::Bank,
        bank_forks::BankForks,
        commitment::{BlockCommitmentCache, CommitmentSlots},
    },
    solana_send_transaction_service::send_transaction_service,
    std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc, RwLock,
        },
    },
    tempfile::TempDir,
    tokio::runtime::Runtime as TokioRuntime,
};

pub struct EphemeralRuntime {
    bank: Arc<Bank>,
    bank_forks: Arc<RwLock<BankForks>>,
    tx_client: EphemeralTransactionClient,
    rpc_service: JsonRpcService,
    settings: EphemeralRollupSettings,
    rpc_port: u16,
    _ledger_dir: TempDir,
    exit: Arc<AtomicBool>,
    runtime: Arc<TokioRuntime>,
}

impl EphemeralRuntime {
    pub fn new(
        parent_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        settings: EphemeralRollupSettings,
        rpc_port: u16,
    ) -> Result<Self, String> {
        let ephemeral_slot = parent_bank.slot().saturating_add(1);
        let bank = Bank::new_from_parent(parent_bank, &Pubkey::default(), ephemeral_slot);
        let bank_forks = BankForks::new_rw_arc(bank);
        let bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        let tx_client = EphemeralTransactionClient::new(Arc::clone(&bank));

        let ledger_dir = TempDir::new().map_err(|e| e.to_string())?;
        let ledger_path = ledger_dir.path().to_path_buf();
        let blockstore = Arc::new(Blockstore::open(&ledger_path).map_err(|e| e.to_string())?);

        let slot = bank.slot();
        let block_commitment_cache = Arc::new(RwLock::new(BlockCommitmentCache::new(
            std::collections::HashMap::new(),
            0,
            CommitmentSlots {
                slot,
                root: slot,
                highest_confirmed_slot: slot,
                highest_super_majority_root: slot,
            },
        )));

        let optimistically_confirmed_bank = Arc::new(RwLock::new(OptimisticallyConfirmedBank {
            bank: Arc::clone(&bank),
        }));

        let leader_schedule_cache = Arc::new(LeaderScheduleCache::default());

        let max_slots = Arc::new(MaxSlots::default());

        let max_complete_transaction_status_slot = Arc::new(AtomicU64::default());

        let genesis_hash = bank.hash();

        let validator_exit = Arc::new(RwLock::new(solana_validator_exit::Exit::default()));
        let exit = Arc::new(AtomicBool::new(false));
        let override_health_check = Arc::new(AtomicBool::new(true));

        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .max_blocking_threads(1)
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?,
        );

        let rpc_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), rpc_port);

        let rpc_config = JsonRpcConfig {
            full_api: true,
            enable_rpc_transaction_history: false,
            disable_health_check: true,
            ..JsonRpcConfig::default()
        };

        let rpc_service = JsonRpcService::new_with_client(
            rpc_addr,
            rpc_config,
            None,
            bank_forks.clone(),
            block_commitment_cache,
            blockstore,
            cluster_info,
            genesis_hash,
            &ledger_path,
            validator_exit,
            exit.clone(),
            override_health_check,
            optimistically_confirmed_bank,
            send_transaction_service::Config::default(),
            max_slots,
            leader_schedule_cache,
            tx_client.clone(),
            max_complete_transaction_status_slot,
            None,
            runtime.clone(),
        )?;

        info!(
            "EphemeralRuntime started on port {} with slot {}",
            rpc_port,
            bank.slot()
        );

        Ok(Self {
            bank,
            bank_forks,
            tx_client,
            rpc_service,
            settings,
            rpc_port,
            _ledger_dir: ledger_dir,
            exit,
            runtime,
        })
    }

    pub fn rpc_port(&self) -> u16 {
        self.rpc_port
    }

    pub fn bank(&self) -> Arc<Bank> {
        self.bank.clone()
    }

    pub fn shutdown(&mut self) {
        info!("Shutting down EphemeralRuntime on port {}", self.rpc_port);
        self.exit.store(true, Ordering::Relaxed);
        self.rpc_service.exit();
        info!("EphemeralRuntime shutdown complete");
    }
}

impl Drop for EphemeralRuntime {
    fn drop(&mut self) {
        if !self.exit.load(Ordering::Relaxed) {
            log::warn!(
                "EphemeralRuntime on port {} dropped without explicit shutdown",
                self.rpc_port
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_account::AccountSharedData,
        solana_gossip::contact_info::ContactInfo,
        solana_keypair::{Keypair, Signer},
        solana_message::Message,
        solana_net_utils::SocketAddrSpace,
        solana_rpc_client::rpc_client::RpcClient,
        solana_rpc_client_types::config::RpcSendTransactionConfig,
        solana_sdk_ids::system_program,
        solana_svm::transaction_processor::ExecutionRecordingConfig,
        solana_system_interface::instruction::transfer,
        solana_transaction::Transaction,
        std::{net::TcpListener, time::Duration},
    };

    fn create_test_cluster_info() -> Arc<ClusterInfo> {
        let keypair = Arc::new(Keypair::new());
        let contact_info =
            ContactInfo::new_localhost(&keypair.pubkey(), solana_time_utils::timestamp());
        Arc::new(ClusterInfo::new(
            contact_info,
            keypair,
            SocketAddrSpace::Unspecified,
        ))
    }

    fn find_free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    fn create_test_bank() -> Bank {
        use solana_genesis_config::GenesisConfig;
        let genesis_config = GenesisConfig::new(&[], &[]);
        Bank::new_for_tests(&genesis_config)
    }

    fn fund_account(bank: &Bank, pubkey: &Pubkey, lamports: u64) {
        let account = AccountSharedData::new(lamports, 0, &system_program::id());
        bank.store_account(pubkey, &account);
    }

    fn create_runtime() -> (Arc<Bank>, EphemeralRuntime) {
        let parent_bank = Arc::new(create_test_bank());
        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            delegated_addresses: vec![],
        };
        let runtime = EphemeralRuntime::new(
            parent_bank.clone(),
            cluster_info,
            settings,
            find_free_port(),
        )
        .unwrap();
        (parent_bank, runtime)
    }

    fn rpc_client(runtime: &EphemeralRuntime) -> RpcClient {
        RpcClient::new(format!("http://127.0.0.1:{}", runtime.rpc_port()))
    }

    #[test]
    fn test_construction_and_shutdown() {
        agave_logger::setup();
        let (_, mut runtime) = create_runtime();

        let socket = std::net::TcpStream::connect_timeout(
            &SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), runtime.rpc_port()),
            Duration::from_secs(5),
        );
        assert!(socket.is_ok(), "RPC port should be listening");

        runtime.shutdown();
    }

    #[test]
    fn test_rpc_get_latest_blockhash() {
        agave_logger::setup();
        let (_, mut runtime) = create_runtime();
        let rpc_client = rpc_client(&runtime);

        std::thread::sleep(Duration::from_secs(2));

        let blockhash = rpc_client.get_latest_blockhash().unwrap();
        assert_ne!(blockhash, solana_hash::Hash::default());

        runtime.shutdown();
    }

    #[test]
    fn test_rpc_account_reads() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let funded_pubkey = Pubkey::new_unique();
        let initial_balance = 10_000_000_000u64;
        fund_account(&parent_bank, &funded_pubkey, initial_balance);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            delegated_addresses: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_port(),
        )
        .unwrap();
        let rpc_client = rpc_client(&runtime);

        std::thread::sleep(Duration::from_secs(2));

        let balance = rpc_client.get_balance(&funded_pubkey).unwrap();
        assert_eq!(balance, initial_balance);

        runtime.shutdown();
    }

    #[test]
    fn test_send_transaction() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let sender_keypair = Keypair::new();
        let sender_pubkey = sender_keypair.pubkey();
        let receiver_pubkey = Pubkey::new_unique();
        let sender_initial = 100_000_000_000u64;
        let transfer_amount = 10_000_000_000u64;
        fund_account(&parent_bank, &sender_pubkey, sender_initial);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            delegated_addresses: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_port(),
        )
        .unwrap();
        let rpc_client = rpc_client(&runtime);

        std::thread::sleep(Duration::from_secs(2));

        let blockhash = rpc_client.get_latest_blockhash().unwrap();
        let instruction = transfer(&sender_pubkey, &receiver_pubkey, transfer_amount);
        let tx = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&sender_pubkey),
            &[&sender_keypair],
            blockhash,
        );

        let config = RpcSendTransactionConfig {
            skip_preflight: true,
            ..Default::default()
        };
        rpc_client
            .send_transaction_with_config(&tx, config)
            .unwrap();

        std::thread::sleep(Duration::from_millis(500));

        let receiver_balance = rpc_client.get_balance(&receiver_pubkey).unwrap();
        assert_eq!(receiver_balance, transfer_amount);

        runtime.shutdown();
    }

    #[test]
    fn test_isolation_from_l1() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let sender_pubkey = Pubkey::new_unique();
        let receiver_pubkey = Pubkey::new_unique();
        let sender_initial = 100_000_000_000u64;
        let transfer_amount = 10_000_000_000u64;
        fund_account(&parent_bank, &sender_pubkey, sender_initial);
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let sender_before = parent_bank.get_balance(&sender_pubkey);
        let receiver_before = parent_bank.get_balance(&receiver_pubkey);

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            delegated_addresses: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            parent_bank.clone(),
            cluster_info,
            settings,
            find_free_port(),
        )
        .unwrap();

        let ephemeral_bank = runtime.bank();
        let blockhash = ephemeral_bank.last_blockhash();
        let instruction = transfer(&sender_pubkey, &receiver_pubkey, transfer_amount);
        let message = Message::new_with_blockhash(&[instruction], Some(&sender_pubkey), &blockhash);
        let tx = Transaction::new_unsigned(message);

        let batch = ephemeral_bank.prepare_batch_for_tests(vec![tx]);
        let mut timings = solana_svm_timings::ExecuteTimings::default();
        let _ = ephemeral_bank.load_execute_and_commit_transactions(
            &batch,
            solana_clock::MAX_PROCESSING_AGE,
            ExecutionRecordingConfig::default(),
            &mut timings,
            None,
        );

        assert!(ephemeral_bank.get_balance(&sender_pubkey) <= sender_initial - transfer_amount);
        assert_eq!(
            ephemeral_bank.get_balance(&receiver_pubkey),
            transfer_amount
        );

        assert_eq!(parent_bank.get_balance(&sender_pubkey), sender_before);
        assert_eq!(parent_bank.get_balance(&receiver_pubkey), receiver_before);

        runtime.shutdown();
    }
}
