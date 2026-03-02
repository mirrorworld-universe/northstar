use {
    crate::{
        ephemeral_tx_client::EphemeralTransactionClient, slot_advancer::SlotAdvancer,
        EphemeralRollupSettings,
    },
    log::{info, warn},
    solana_account::{AccountSharedData, ReadableAccount},
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
        collections::{HashMap, HashSet},
        net::SocketAddr,
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc, RwLock,
        },
        time::Duration,
    },
    tempfile::TempDir,
    tokio::runtime::Runtime as TokioRuntime,
};

pub struct EphemeralRuntime {
    bank_forks: Arc<RwLock<BankForks>>,
    tx_client: EphemeralTransactionClient,
    rpc_service: JsonRpcService,
    settings: EphemeralRollupSettings,
    rpc_addr: SocketAddr,
    _ledger_dir: TempDir,
    exit: Arc<AtomicBool>,
    runtime: Arc<TokioRuntime>,
    slot_advancer: Option<SlotAdvancer>,
    /// Snapshot of delegated account state at ER creation time.
    /// Used for settlement diff computation (future task).
    initial_account_snapshots: HashMap<Pubkey, AccountSharedData>,
    /// Set of delegated account pubkeys for fast lookup.
    delegated_accounts: HashSet<Pubkey>,
}

impl EphemeralRuntime {
    pub fn new(
        parent_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        settings: EphemeralRollupSettings,
        rpc_addr: SocketAddr,
        portal_program_id: Pubkey,
    ) -> Result<Self, String> {
        let ephemeral_slot = parent_bank.slot().saturating_add(1);
        let bank = Bank::new_from_parent(parent_bank.clone(), &Pubkey::default(), ephemeral_slot);
        let bank_forks = BankForks::new_rw_arc(bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        // Validate and snapshot delegated accounts
        let mut initial_account_snapshots = HashMap::new();
        let mut delegated_accounts = HashSet::new();

        for pubkey in &settings.delegated_accounts {
            let Some(account) = parent_bank.get_account(pubkey) else {
                warn!("Account {pubkey} listed as delegated but does not exist on L1. Skipping.");
                continue;
            };
            if account.owner() != &portal_program_id {
                warn!(
                    "Account {} listed as delegated but owned by {}, not portal program {}. \
                     Skipping.",
                    pubkey,
                    account.owner(),
                    portal_program_id,
                );
                continue;
            }
            info!("Delegated account {} validated and snapshotted", pubkey);
            initial_account_snapshots.insert(*pubkey, account);
            delegated_accounts.insert(*pubkey);
        }

        info!(
            "EphemeralRuntime: {} of {} delegated accounts validated",
            delegated_accounts.len(),
            settings.delegated_accounts.len(),
        );

        let delegated_set = Arc::new(delegated_accounts.clone());
        let tx_client = EphemeralTransactionClient::new(bank_forks.clone(), delegated_set);

        let ledger_dir = TempDir::new().map_err(|e| e.to_string())?;
        let blockstore = Arc::new(Blockstore::open(ledger_dir.path()).map_err(|e| e.to_string())?);

        let slot = initial_bank.slot();
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
            bank: Arc::clone(&initial_bank),
        }));

        let leader_schedule_cache = Arc::new(LeaderScheduleCache::default());

        let max_slots = Arc::new(MaxSlots::default());

        let max_complete_transaction_status_slot = Arc::new(AtomicU64::default());

        let genesis_hash = initial_bank.hash();

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
            block_commitment_cache.clone(),
            blockstore,
            cluster_info,
            genesis_hash,
            ledger_dir.path(),
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
            "EphemeralRuntime listening at {rpc_addr} with slot {}",
            initial_bank.slot()
        );

        let slot_advancer = SlotAdvancer::new(
            bank_forks.clone(),
            block_commitment_cache.clone(),
            initial_bank,
            Duration::from_millis(400),
            Pubkey::default(),
            exit.clone(),
        );

        Ok(Self {
            bank_forks,
            tx_client,
            rpc_service,
            settings,
            rpc_addr,
            _ledger_dir: ledger_dir,
            exit,
            runtime,
            slot_advancer: Some(slot_advancer),
            initial_account_snapshots,
            delegated_accounts,
        })
    }

    pub fn rpc_addr(&self) -> String {
        format!("http://{}", self.rpc_addr)
    }

    pub fn bank(&self) -> Arc<Bank> {
        self.bank_forks.read().unwrap().working_bank()
    }

    pub fn shutdown(&mut self) {
        info!("Shutting down EphemeralRuntime at {}", self.rpc_addr);
        self.exit.store(true, Ordering::Relaxed);
        if let Some(advancer) = self.slot_advancer.take() {
            advancer.join();
        }
        self.rpc_service.exit();
        info!("EphemeralRuntime shutdown complete");
    }

    /// Returns the set of delegated account pubkeys.
    pub fn delegated_accounts(&self) -> &HashSet<Pubkey> {
        &self.delegated_accounts
    }

    /// Returns the initial snapshot of a delegated account.
    pub fn initial_account_snapshot(&self, pubkey: &Pubkey) -> Option<&AccountSharedData> {
        self.initial_account_snapshots.get(pubkey)
    }
}

impl Drop for EphemeralRuntime {
    fn drop(&mut self) {
        if !self.exit.load(Ordering::Relaxed) {
            log::warn!(
                "EphemeralRuntime on {} dropped without explicit shutdown",
                self.rpc_addr
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

    fn find_free_addr() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
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
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![],
        };
        let portal_program_id = Pubkey::new_unique();
        let runtime = EphemeralRuntime::new(
            parent_bank.clone(),
            cluster_info,
            settings,
            find_free_addr(),
            portal_program_id,
        )
        .unwrap();
        (parent_bank, runtime)
    }

    fn rpc_client(runtime: &EphemeralRuntime) -> RpcClient {
        RpcClient::new(runtime.rpc_addr())
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
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_addr(),
            Pubkey::new_unique(),
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
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_addr(),
            Pubkey::new_unique(),
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
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            parent_bank.clone(),
            cluster_info,
            settings,
            find_free_addr(),
            Pubkey::new_unique(),
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

    #[test]
    fn test_blockhash_changes_over_time() {
        agave_logger::setup();
        let (_, mut runtime) = create_runtime();
        let rpc_client = rpc_client(&runtime);

        let hash1 = rpc_client.get_latest_blockhash().unwrap();

        std::thread::sleep(Duration::from_secs(1));

        let hash2 = rpc_client.get_latest_blockhash().unwrap();
        assert_ne!(hash1, hash2, "Blockhash should change over time");

        runtime.shutdown();
    }

    #[test]
    fn test_transactions_work_after_blockhash_refresh() {
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
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_addr(),
            Pubkey::new_unique(),
        )
        .unwrap();
        let rpc_client = rpc_client(&runtime);

        std::thread::sleep(Duration::from_secs(1));

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
    fn test_old_blockhash_eventually_rejected() {
        agave_logger::setup();
        let (_, mut runtime) = create_runtime();
        let rpc_client = rpc_client(&runtime);

        let old_blockhash = rpc_client.get_latest_blockhash().unwrap();

        std::thread::sleep(Duration::from_secs(3));

        let result = rpc_client.send_transaction(&Transaction::new_unsigned(
            Message::new_with_blockhash(&[], None, &old_blockhash),
        ));

        assert!(result.is_err(), "Old blockhash should be rejected");

        runtime.shutdown();
    }

    #[test]
    fn test_transactions_during_slot_transition() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let sender_keypair = Keypair::new();
        let sender_pubkey = sender_keypair.pubkey();
        let receiver_pubkey = Pubkey::new_unique();
        let sender_initial = 1_000_000_000_000u64;
        fund_account(&parent_bank, &sender_pubkey, sender_initial);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_addr(),
            Pubkey::new_unique(),
        )
        .unwrap();
        let rpc_client = rpc_client(&runtime);

        std::thread::sleep(Duration::from_millis(500));

        let mut results = Vec::new();
        for _ in 0..100 {
            let blockhash = rpc_client.get_latest_blockhash().unwrap();
            let instruction = transfer(&sender_pubkey, &receiver_pubkey, 1_000_000u64);
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
            results.push(rpc_client.send_transaction_with_config(&tx, config));
        }

        std::thread::sleep(Duration::from_millis(500));

        let successes = results.iter().filter(|r| r.is_ok()).count();
        assert!(
            successes > 50,
            "Most transactions should succeed during slot transitions, got {}",
            successes
        );

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_valid() {
        // Test that a properly delegated account (owned by portal) is validated
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();

        // Create an account owned by the portal program
        let account = AccountSharedData::new(1_000_000, 0, &portal_program_id);
        parent_bank.store_account(&delegated_pubkey, &account);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![delegated_pubkey],
        };

        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_addr(),
            portal_program_id,
        )
        .unwrap();

        // Verify the delegated account is tracked
        assert!(runtime.delegated_accounts().contains(&delegated_pubkey));

        // Verify snapshot is stored
        assert!(runtime
            .initial_account_snapshot(&delegated_pubkey)
            .is_some());

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_wrong_owner() {
        // Test that accounts not owned by portal are rejected
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let wrong_owner_program = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();

        // Create an account owned by a different program
        let account = AccountSharedData::new(1_000_000, 0, &wrong_owner_program);
        parent_bank.store_account(&delegated_pubkey, &account);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![delegated_pubkey],
        };

        // Should succeed but the account should NOT be in delegated set
        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_addr(),
            portal_program_id,
        )
        .unwrap();

        // Verify the account is NOT in delegated set (rejected due to wrong owner)
        assert!(!runtime.delegated_accounts().contains(&delegated_pubkey));

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_nonexistent() {
        // Test that nonexistent accounts are rejected
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let nonexistent_pubkey = Pubkey::new_unique();

        // Don't create the account - it doesn't exist
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![nonexistent_pubkey],
        };

        // Should succeed but the account should NOT be in delegated set
        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_addr(),
            portal_program_id,
        )
        .unwrap();

        // Verify the account is NOT in delegated set (doesn't exist)
        assert!(!runtime.delegated_accounts().contains(&nonexistent_pubkey));

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_multiple() {
        // Test validation of multiple delegated accounts
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();

        let delegated1 = Pubkey::new_unique();
        let delegated2 = Pubkey::new_unique();
        let wrong_owner = Pubkey::new_unique();

        // Create valid delegated accounts
        let account1 = AccountSharedData::new(1_000_000, 0, &portal_program_id);
        let account2 = AccountSharedData::new(2_000_000, 0, &portal_program_id);
        let account3 = AccountSharedData::new(3_000_000, 0, &Pubkey::new_unique()); // wrong owner

        parent_bank.store_account(&delegated1, &account1);
        parent_bank.store_account(&delegated2, &account2);
        parent_bank.store_account(&wrong_owner, &account3);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![delegated1, delegated2, wrong_owner, Pubkey::new_unique()],
        };

        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_addr(),
            portal_program_id,
        )
        .unwrap();

        // Only 2 should be in the delegated set (valid ones)
        assert_eq!(runtime.delegated_accounts().len(), 2);
        assert!(runtime.delegated_accounts().contains(&delegated1));
        assert!(runtime.delegated_accounts().contains(&delegated2));

        // Snapshots should exist for valid accounts
        assert!(runtime.initial_account_snapshot(&delegated1).is_some());
        assert!(runtime.initial_account_snapshot(&delegated2).is_some());

        runtime.shutdown();
    }
}
