use {
    crate::{
        ephemeral_tx_client::EphemeralTransactionClient, slot_advancer::SlotAdvancer,
        EphemeralRollupSettings,
    },
    log::{info, warn},
    solana_account::{AccountSharedData, ReadableAccount, WritableAccount},
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
    block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
    optimistically_confirmed_bank: Arc<RwLock<OptimisticallyConfirmedBank>>,
    rpc_service: JsonRpcService,
    rpc_addr: SocketAddr,
    /// Sonic: Controls the RPC service lifetime — only set on final shutdown.
    rpc_exit: Arc<AtomicBool>,
    /// Sonic: Controls the current SlotAdvancer — set when resetting to new parent.
    advancer_exit: Arc<AtomicBool>,
    slot_advancer: Option<SlotAdvancer>,
    /// Snapshot of delegated account state at ER creation time.
    /// Used for settlement diff computation (future task).
    initial_account_snapshots: HashMap<Pubkey, AccountSharedData>,
    /// Set of delegated account pubkeys for fast lookup.
    /// Shared with EphemeralTransactionClient — wrapped in RwLock so new
    /// delegations arriving from L1 can be added at runtime.
    delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    /// Shared with EphemeralTransactionClient - tracks accounts that have been written to on this ER.
    touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,

    /// Sonic: When false, the tx_client rejects all transactions.
    /// Set to true when an ephemeral session is active.
    active: Arc<AtomicBool>,
    _portal_program_id: Pubkey,

    _tx_client: EphemeralTransactionClient,
    _settings: EphemeralRollupSettings,
    _ledger_dir: TempDir,
    _runtime: Arc<TokioRuntime>,
}

impl EphemeralRuntime {
    /// Slot offset that separates ER slot numbers from L1 slot numbers.
    /// The ER and L1 share the same `AccountsDb`, whose root tracker requires
    /// `add_root` calls in monotonically increasing order.  By placing ER slots
    /// far above any reachable L1 slot we guarantee the two never interleave.
    ///
    /// We use 1 trillion which is unreachable by L1 in practice
    /// (at 2.5 slots/sec it would take ~14,000 years) but small enough to avoid
    /// arithmetic overflows in tick-height calculations.
    const ER_SLOT_OFFSET: u64 = 1000000000000;

    pub fn new(
        parent_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        settings: EphemeralRollupSettings,
        rpc_addr: SocketAddr,
        portal_program_id: Pubkey,
    ) -> Result<Self, String> {
        // Place ER slots far above L1 slots so the shared AccountsDb root
        // tracker never sees an out-of-order add_root from either side.
        let ephemeral_slot = Self::ER_SLOT_OFFSET + parent_bank.slot() + 1;
        let bank = Bank::new_from_parent(parent_bank.clone(), &Pubkey::default(), ephemeral_slot);

        // The bank inherits tick_height from the L1 parent, but max_tick_height
        // is (ephemeral_slot + 1) * ticks_per_slot — astronomically large due to
        // ER_SLOT_OFFSET.  Warp tick_height so only one slot's worth of ticks
        // remains, matching what a normal bank would need.
        let ticks_per_slot = bank.ticks_per_slot();
        bank.set_tick_height(bank.max_tick_height() - ticks_per_slot);

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

        let delegated_set = Arc::new(RwLock::new(delegated_accounts.clone()));
        let touched_accounts = Arc::new(RwLock::new(HashSet::new()));
        // Sonic: Starts inactive — transactions rejected until activate() is called
        let active = Arc::new(AtomicBool::new(false));
        let tx_client = EphemeralTransactionClient::new(
            bank_forks.clone(),
            delegated_set.clone(),
            touched_accounts.clone(),
            active.clone(),
        );

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
        // Sonic: Separate exit flags for RPC service and slot advancer.
        // rpc_exit controls the RPC service lifetime (set only on final shutdown).
        // advancer_exit controls the current SlotAdvancer (set when resetting to new parent).
        let rpc_exit = Arc::new(AtomicBool::new(false));
        let advancer_exit = Arc::new(AtomicBool::new(false));
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
            rpc_exit.clone(),
            override_health_check,
            optimistically_confirmed_bank.clone(),
            send_transaction_service::Config::default(),
            max_slots,
            leader_schedule_cache,
            tx_client.clone(),
            max_complete_transaction_status_slot,
            None,
            runtime.clone(),
            Some(delegated_set.clone()),
        )?;

        info!(
            "EphemeralRuntime listening at {rpc_addr} with slot {}",
            initial_bank.slot()
        );

        let slot_advancer = crate::slot_advancer::SlotAdvancer::new(
            bank_forks.clone(),
            block_commitment_cache.clone(),
            initial_bank,
            crate::slot_advancer::Config {
                slot_duration: Duration::from_millis(400),
                manager_account: Pubkey::default(),
            },
            advancer_exit.clone(),
        );

        Ok(Self {
            bank_forks,
            block_commitment_cache,
            optimistically_confirmed_bank,
            rpc_service,
            rpc_addr,
            rpc_exit,
            advancer_exit,
            slot_advancer: Some(slot_advancer),
            initial_account_snapshots,
            delegated_accounts: delegated_set,
            touched_accounts,
            active,
            _portal_program_id: portal_program_id,

            _settings: settings,
            _tx_client: tx_client,
            _ledger_dir: ledger_dir,
            _runtime: runtime,
        })
    }

    pub fn rpc_addr(&self) -> String {
        format!("http://{}", self.rpc_addr)
    }

    /// Sonic: Activate the ephemeral rollup — transactions will be accepted.
    pub fn activate(&self) {
        info!("Activating ephemeral rollup at {}", self.rpc_addr);
        self.active.store(true, Ordering::Relaxed);
    }

    /// Sonic: Deactivate the ephemeral rollup — transactions will be rejected.
    pub fn deactivate(&self) {
        info!("Deactivating ephemeral rollup at {}", self.rpc_addr);
        self.active.store(false, Ordering::Relaxed);
    }

    /// Sonic: Check if the ephemeral rollup is accepting transactions.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn bank(&self) -> Arc<Bank> {
        self.bank_forks.read().unwrap().working_bank()
    }

    pub fn shutdown(&mut self) {
        info!("Shutting down EphemeralRuntime at {}", self.rpc_addr);
        // Stop slot advancer first
        self.advancer_exit.store(true, Ordering::Relaxed);
        if let Some(advancer) = self.slot_advancer.take() {
            advancer.join();
        }
        // Then stop RPC service
        self.rpc_exit.store(true, Ordering::Relaxed);
        self.rpc_service.exit();
        info!("EphemeralRuntime shutdown complete");
    }

    /// Sonic: Reset the ephemeral bank to a fresh fork from a new L1 root bank.
    /// Stops the old SlotAdvancer, swaps BankForks in-place (same Arc, new contents),
    /// clears session state, and starts a new SlotAdvancer.
    /// Called when a new session opens to get a fresh L1 snapshot.
    pub fn reset_to_new_parent(&mut self, parent_bank: Arc<Bank>) {
        // 1. Stop old slot advancer
        self.advancer_exit.store(true, Ordering::Relaxed);
        if let Some(advancer) = self.slot_advancer.take() {
            advancer.join();
        }

        // 2. Create new ephemeral bank from current L1 root
        let ephemeral_slot = Self::ER_SLOT_OFFSET + parent_bank.slot() + 1;
        let bank = Bank::new_from_parent(parent_bank, &Pubkey::default(), ephemeral_slot);
        let ticks_per_slot = bank.ticks_per_slot();
        bank.set_tick_height(bank.max_tick_height() - ticks_per_slot);

        // 3. Swap BankForks in-place — same Arc, new contents.
        //    All holders (RPC service, tx_client) see the new bank.
        let new_bf_arc = BankForks::new_rw_arc(bank);
        let new_bf = Arc::try_unwrap(new_bf_arc)
            .unwrap_or_else(|_| panic!("just created, refcount must be 1"))
            .into_inner()
            .expect("lock not poisoned");
        *self.bank_forks.write().unwrap() = new_bf;

        let initial_bank = self.bank_forks.read().unwrap().root_bank();
        let slot = initial_bank.slot();

        // 4. Update commitment cache
        *self.block_commitment_cache.write().unwrap() = BlockCommitmentCache::new(
            std::collections::HashMap::new(),
            0,
            CommitmentSlots {
                slot,
                root: slot,
                highest_confirmed_slot: slot,
                highest_super_majority_root: slot,
            },
        );

        // 5. Update optimistically confirmed bank
        *self.optimistically_confirmed_bank.write().unwrap() = OptimisticallyConfirmedBank {
            bank: initial_bank.clone(),
        };

        // 6. Clear session state
        self.initial_account_snapshots.clear();
        self.delegated_accounts.write().unwrap().clear();
        self.touched_accounts.write().unwrap().clear();

        // 7. Start new slot advancer
        let advancer_exit = Arc::new(AtomicBool::new(false));
        self.advancer_exit = advancer_exit.clone();
        self.slot_advancer = Some(crate::slot_advancer::SlotAdvancer::new(
            self.bank_forks.clone(),
            self.block_commitment_cache.clone(),
            initial_bank,
            crate::slot_advancer::Config {
                slot_duration: Duration::from_millis(400),
                manager_account: Pubkey::default(),
            },
            advancer_exit,
        ));

        info!("EphemeralRuntime reset to new L1 parent, ER slot {}", slot);
    }

    /// Returns a clone of the delegated account pubkeys set.
    pub fn delegated_accounts(&self) -> HashSet<Pubkey> {
        self.delegated_accounts.read().unwrap().clone()
    }

    /// Returns the initial snapshot of a delegated account.
    pub fn initial_account_snapshot(&self, pubkey: &Pubkey) -> Option<&AccountSharedData> {
        self.initial_account_snapshots.get(pubkey)
    }

    /// Handle a new account delegation from L1.
    /// Copies the account data from L1 into the ER bank and adds it to the
    /// delegated accounts set so transactions can write to it.
    pub fn handle_delegation(&self, delegated_account: &Pubkey, account_data: AccountSharedData) {
        let bank = self.bank();
        bank.store_account(delegated_account, &account_data);

        // Add to the delegated accounts set so the tx client allows writes
        self.delegated_accounts
            .write()
            .unwrap()
            .insert(*delegated_account);

        // Mark as touched so the balance isn't zeroed later
        self.touched_accounts
            .write()
            .unwrap()
            .insert(*delegated_account);

        info!(
            "Delegated account {} added to ER (owner: {}, lamports: {})",
            delegated_account,
            account_data.owner(),
            account_data.lamports()
        );
    }

    /// Credit a deposit on the ephemeral bank. Called by NorthStarService
    /// when a FeeDeposited event is detected on L1.
    pub fn credit_deposit(&self, depositor: &Pubkey, lamports: u64) {
        // TODO: make sure we do it between blocks or postpone a bit block creation
        let bank = self.bank();
        let mut account = bank.get_account(depositor).unwrap_or_default();
        let new_balance = account.lamports().saturating_add(lamports);
        account.set_lamports(new_balance);
        // Ensure the account is owned by system program
        if account.owner() == &Pubkey::default() {
            account.set_owner(solana_sdk_ids::system_program::id());
        }
        bank.store_account(depositor, &account);

        // Mark as touched so the balance isn't zeroed later
        self.touched_accounts.write().unwrap().insert(*depositor);

        info!(
            "Credited {} lamports to {} on ER (new balance: {})",
            lamports, depositor, new_balance
        );
    }
}

impl Drop for EphemeralRuntime {
    fn drop(&mut self) {
        if !self.rpc_exit.load(Ordering::Relaxed) {
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
        solana_rpc_client_types::config::{CommitmentConfig, RpcSendTransactionConfig},
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
        runtime.activate();
        let rpc_client = rpc_client(&runtime);

        // Wait for the slot advancer to advance past the initial slots
        std::thread::sleep(Duration::from_secs(2));

        // Refresh blockhash using processed commitment (heaviest slot) before sending transaction
        let blockhash = rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .unwrap()
            .0;
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

        // Wait for transaction to be processed (longer sleep for slower slot advancement)
        std::thread::sleep(Duration::from_secs(2));

        // Use processed commitment to read from the working bank, not the root
        let receiver_balance = rpc_client
            .get_balance_with_commitment(&receiver_pubkey, CommitmentConfig::processed())
            .unwrap()
            .value;
        assert_eq!(receiver_balance, transfer_amount);

        runtime.shutdown();
    }

    #[test]
    fn test_transactions_rejected_when_inactive() {
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
        // Do NOT call runtime.activate() — runtime stays inactive
        assert!(!runtime.is_active());
        let rpc_client = rpc_client(&runtime);

        std::thread::sleep(Duration::from_secs(2));

        // RPC reads should still work when inactive
        let blockhash = rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .unwrap()
            .0;
        assert_ne!(blockhash, solana_hash::Hash::default());

        // Send a transaction — it should be silently dropped by the inactive tx client
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
        // sendTransaction RPC succeeds (returns sig) but tx is dropped internally
        rpc_client
            .send_transaction_with_config(&tx, config)
            .unwrap();

        std::thread::sleep(Duration::from_secs(2));

        // Receiver should have 0 balance — transaction was rejected
        let receiver_balance = rpc_client
            .get_balance_with_commitment(&receiver_pubkey, CommitmentConfig::processed())
            .unwrap()
            .value;
        assert_eq!(
            receiver_balance, 0,
            "Transaction should be rejected when runtime is inactive"
        );

        runtime.shutdown();
    }

    #[test]
    fn test_reset_to_new_parent_picks_up_fresh_l1_state() {
        agave_logger::setup();

        // Create initial L1 bank with account A
        let parent_bank = create_test_bank();
        let account_a = Pubkey::new_unique();
        fund_account(&parent_bank, &account_a, 10_000_000_000);
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        // Create runtime from initial bank (inactive)
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

        // Verify account A visible on ER
        assert_eq!(runtime.bank().get_balance(&account_a), 10_000_000_000);

        // Create a new L1 bank with account B (simulates L1 advancing)
        let new_parent = Bank::new_from_parent(parent_bank, &Pubkey::default(), 1);
        let account_b = Pubkey::new_unique();
        fund_account(&new_parent, &account_b, 20_000_000_000);
        new_parent.freeze();
        let new_parent = Arc::new(new_parent);

        // Reset to new parent
        runtime.reset_to_new_parent(new_parent);
        runtime.activate();

        std::thread::sleep(Duration::from_millis(500));

        // Account B (created after startup) should now be visible
        assert_eq!(runtime.bank().get_balance(&account_b), 20_000_000_000);

        // Account A should still be visible (inherited from L1 chain)
        assert_eq!(runtime.bank().get_balance(&account_a), 10_000_000_000);

        // Session state should be cleared
        assert!(runtime.delegated_accounts().is_empty());

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

        // With "finalized" commitment (default), blockhash comes from the root bank.
        // Instead, we verify the blockhash is valid (non-default).
        let hash = rpc_client.get_latest_blockhash().unwrap();
        assert_ne!(
            hash,
            solana_hash::Hash::default(),
            "Blockhash should be valid"
        );

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
        runtime.activate();
        let rpc_client = rpc_client(&runtime);

        // Wait for the slot advancer to advance past the initial slots
        std::thread::sleep(Duration::from_secs(2));

        // Refresh blockhash using processed commitment (heaviest slot) before sending transaction
        let blockhash = rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .unwrap()
            .0;
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

        // Wait for transaction to be processed (longer sleep for slower slot advancement)
        std::thread::sleep(Duration::from_secs(2));

        // Use processed commitment to read from the working bank, not the root
        let receiver_balance = rpc_client
            .get_balance_with_commitment(&receiver_pubkey, CommitmentConfig::processed())
            .unwrap()
            .value;
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
        runtime.activate();
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

    #[test]
    fn test_rpc_get_delegated_accounts() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();

        // Create two delegated accounts owned by portal program
        let delegated1 = Pubkey::new_unique();
        let delegated2 = Pubkey::new_unique();
        let account1 = AccountSharedData::new(1_000_000, 0, &portal_program_id);
        let account2 = AccountSharedData::new(2_000_000, 0, &portal_program_id);
        parent_bank.store_account(&delegated1, &account1);
        parent_bank.store_account(&delegated2, &account2);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![delegated1, delegated2],
        };

        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            cluster_info,
            settings,
            find_free_addr(),
            portal_program_id,
        )
        .unwrap();

        std::thread::sleep(Duration::from_secs(2));

        // Call getDelegatedAccounts via RPC
        let rpc_client = rpc_client(&runtime);
        let accounts: Vec<String> = rpc_client
            .send(
                solana_rpc_client_types::request::RpcRequest::Custom {
                    method: "getDelegatedAccounts",
                },
                serde_json::Value::Null,
            )
            .unwrap();

        assert_eq!(accounts.len(), 2);

        let account_set: HashSet<String> = accounts.into_iter().collect();
        assert!(account_set.contains(&delegated1.to_string()));
        assert!(account_set.contains(&delegated2.to_string()));

        runtime.shutdown();
    }
}
