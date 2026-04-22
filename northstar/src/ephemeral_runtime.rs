use {
    crate::{
        EphemeralRollupSettings, ephemeral_tpu::EphemeralTpu,
        ephemeral_tx_client::EphemeralTransactionClient, slot_advancer::SlotAdvancer,
    },
    crossbeam_channel::{Sender, unbounded},
    log::{info, warn},
    solana_account::{AccountSharedData, ReadableAccount, WritableAccount},
    solana_gossip::cluster_info::ClusterInfo,
    solana_clock::{BankId, Slot},
    solana_keypair::Keypair,
    solana_ledger::{blockstore::Blockstore, leader_schedule_cache::LeaderScheduleCache},
    solana_pubkey::Pubkey,
    solana_rpc::{
        er_history::ErHistoryStore,
        max_slots::MaxSlots,
        optimistically_confirmed_bank_tracker::OptimisticallyConfirmedBank,
        rpc::{ErNodeInfo, JsonRpcConfig},
        rpc_pubsub_service::{PubSubConfig, PubSubService},
        rpc_service::JsonRpcService,
        rpc_subscriptions::RpcSubscriptions,
    },
    solana_runtime::{
        bank::{Bank, DropCallback},
        bank_forks::BankForks,
        commitment::{BlockCommitmentCache, CommitmentSlots},
    },
    solana_send_transaction_service::send_transaction_service,
    solana_signer::Signer,
    std::{
        collections::{HashMap, HashSet},
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{
            Arc, Mutex, RwLock,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread::{Builder, JoinHandle},
        time::Duration,
    },
    tempfile::TempDir,
    tokio::runtime::Runtime as TokioRuntime,
};

#[derive(Debug, Clone)]
struct NoopDropCallback;

impl DropCallback for NoopDropCallback {
    fn callback(&self, _bank: &Bank) {}

    fn clone_box(&self) -> Box<dyn DropCallback + Send + Sync> {
        Box::new(self.clone())
    }
}

struct RetiredBankForks {
    bank_forks: BankForks,
    purge_slots: Vec<(Slot, BankId)>,
}

pub struct EphemeralRuntime {
    bank_forks: Arc<RwLock<BankForks>>,
    /// Serializes ER bank mutations with slot advancement and tx execution.
    bank_operation_lock: Arc<Mutex<()>>,
    block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
    optimistically_confirmed_bank: Arc<RwLock<OptimisticallyConfirmedBank>>,
    rpc_service: JsonRpcService,
    rpc_addr: SocketAddr,
    /// Sonic: PubSub WebSocket service for subscriptions.
    pubsub_service: Option<PubSubService>,
    /// Sonic: Trigger to cancel PubSub service on shutdown.
    pubsub_trigger: Option<stream_cancel::Trigger>,
    /// Sonic: RPC subscriptions shared between PubSub and slot advancer.
    rpc_subscriptions: Arc<RpcSubscriptions>,
    ws_addr: SocketAddr,
    /// Sonic: TPU QUIC endpoint for direct transaction submission.
    tpu: Option<EphemeralTpu>,
    tpu_addr: SocketAddr,
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
    /// Sonic: Hold current L1 anchor bank alive even if ER banks sever parent
    /// links. Without this, dropping the ER root would drop the unrooted L1
    /// parent bank and purge its slot from the shared AccountsDb.
    l1_anchor_bank: Arc<Bank>,
    /// Sonic: Old ER fork trees are dropped on a dedicated reaper thread so a
    /// session reset does not synchronously destroy thousands of banks on the
    /// `solNorthStar` service thread.
    retired_bank_forks_sender: Option<Sender<RetiredBankForks>>,
    retired_bank_forks_reaper: Option<JoinHandle<()>>,
    retired_bank_forks_pending: Arc<AtomicU64>,
    #[cfg(test)]
    retired_bank_forks_reaper_pause: Arc<AtomicBool>,

    /// Sonic: When false, the tx_client rejects all transactions.
    /// Set to true when an ephemeral session is active.
    active: Arc<AtomicBool>,
    /// Sonic: Current session PDA, shared with the RPC handler.
    session_pda: Arc<RwLock<Option<Pubkey>>>,
    /// Sonic: In-memory ER transaction history for Phase 1 history APIs.
    er_history_store: Arc<ErHistoryStore>,
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
    /// Compute the ER slot used when spinning up an ephemeral fork.
    ///
    /// The ER and L1 share the same `AccountsDb`. If an ER slot ever coincides
    /// with a slot the L1 later reaches, the L1's `AccountsBackgroundService`
    /// will panic in `purge_slot_cache_pubkeys` (the shared `AccountsStorage`
    /// already has an entry for that slot, produced by the ER).
    ///
    /// We therefore place the ER at `parent.slot + ER_SLOT_OFFSET`, where the
    /// offset is large enough that L1 can never catch up in any realistic
    /// timeframe (2^40 slots ≈ 1.4e13 years at 2.5 slots/s) while still being
    /// far below `Slot::MAX` so tick-height / block-height arithmetic never
    /// overflows.
    ///
    /// Epoch safety is handled separately: ER banks are constructed with
    /// `Bank::new_from_parent_ephemeral`, which suppresses all epoch-boundary
    /// side effects (stake-history rebuild, `begin_partitioned_rewards`,
    /// `update_epoch_stakes`, `distribute_partitioned_epoch_rewards`). That
    /// makes it safe for the ER slot to land in an arbitrarily distant epoch.
    fn er_slot_for(parent: &Bank) -> u64 {
        const ER_SLOT_OFFSET: u64 = 1u64 << 40;
        parent.slot().saturating_add(ER_SLOT_OFFSET)
    }

    fn advertise_addr(addr: SocketAddr) -> SocketAddr {
        if addr.ip().is_unspecified() {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port())
        } else {
            addr
        }
    }

    fn spawn_bank_forks_reaper(
        pending: Arc<AtomicU64>,
        #[cfg(test)] pause: Arc<AtomicBool>,
    ) -> (Sender<RetiredBankForks>, JoinHandle<()>) {
        let (sender, receiver) = unbounded::<RetiredBankForks>();
        let thread_hdl = Builder::new()
            .name("solNorthStarReaper".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                while let Ok(retired_bank_forks) = receiver.recv() {
                    #[cfg(test)]
                    while pause.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(10));
                    }

                    if !retired_bank_forks.purge_slots.is_empty() {
                        retired_bank_forks
                            .bank_forks
                            .root_bank()
                            .rc
                            .accounts
                            .accounts_db
                            .remove_unrooted_slots(&retired_bank_forks.purge_slots);
                    }
                    drop(retired_bank_forks.bank_forks);
                    pending.fetch_sub(1, Ordering::Relaxed);
                }
            })
            .unwrap();
        (sender, thread_hdl)
    }

    fn collect_bank_forks_purge_slots(bank_forks: &BankForks) -> Vec<(Slot, BankId)> {
        bank_forks
            .banks()
            .iter()
            .map(|(&slot, bank)| (slot, bank.bank_id()))
            .collect()
    }

    fn purge_bank_forks_slots(bank_forks: &BankForks) {
        let purge_slots = Self::collect_bank_forks_purge_slots(bank_forks);
        if !purge_slots.is_empty() {
            bank_forks
                .root_bank()
                .rc
                .accounts
                .accounts_db
                .remove_unrooted_slots(&purge_slots);
        }
    }

    fn retire_bank_forks(&self, old_bank_forks: BankForks) {
        if let Some(sender) = &self.retired_bank_forks_sender {
            self.retired_bank_forks_pending
                .fetch_add(1, Ordering::Relaxed);
            let retired_bank_forks = RetiredBankForks {
                purge_slots: Self::collect_bank_forks_purge_slots(&old_bank_forks),
                bank_forks: old_bank_forks,
            };
            if let Err(err) = sender.send(retired_bank_forks) {
                let retired_bank_forks = err.0;
                if !retired_bank_forks.purge_slots.is_empty() {
                    retired_bank_forks
                        .bank_forks
                        .root_bank()
                        .rc
                        .accounts
                        .accounts_db
                        .remove_unrooted_slots(&retired_bank_forks.purge_slots);
                }
                drop(retired_bank_forks.bank_forks);
                self.retired_bank_forks_pending
                    .fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    #[cfg(test)]
    fn set_bank_forks_reaper_paused(&self, paused: bool) {
        self.retired_bank_forks_reaper_pause
            .store(paused, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn retired_bank_forks_pending(&self) -> u64 {
        self.retired_bank_forks_pending.load(Ordering::Relaxed)
    }

    fn prepare_initial_working_bank(bank: &Bank) {
        let ticks_per_slot = bank.ticks_per_slot();
        bank.set_tick_height(bank.max_tick_height() - ticks_per_slot);
    }

    fn freeze_and_rotate_bank_for_rpc(
        bank_forks: &Arc<RwLock<BankForks>>,
        block_commitment_cache: &Arc<RwLock<BlockCommitmentCache>>,
        optimistically_confirmed_bank: &Arc<RwLock<OptimisticallyConfirmedBank>>,
        rpc_subscriptions: Option<&Arc<RpcSubscriptions>>,
        er_history_store: &Arc<ErHistoryStore>,
    ) -> Arc<Bank> {
        let (frozen_slot, frozen_bank, next_bank_slot, next_bank_arc) = {
            let current_bank = bank_forks.read().unwrap().working_bank();
            current_bank.freeze();
            er_history_store.finalize_slot(&current_bank);

            let frozen_slot = current_bank.slot();
            let frozen_bank = current_bank.clone();
            let next_bank_slot = frozen_slot.saturating_add(1);
            let next_bank =
                Bank::new_from_parent_ephemeral(current_bank, &Pubkey::default(), next_bank_slot);
            let next_bank_arc = {
                let mut bank_forks_write = bank_forks.write().unwrap();
                let inserted = bank_forks_write.insert(next_bank);
                inserted.clone_without_scheduler()
            };

            // Sonic: ER account lookup uses `ancestors`, not recursive parent
            // traversal. Once child exists, older ER banks no longer need to
            // keep their own parent links alive. Clear only ER->ER links; do
            // not mutate the L1 anchor bank.
            if let Some(parent) = frozen_bank.parent() {
                if parent.slot() >= (1u64 << 40) {
                    parent.clear_parent();
                }
            }

            (frozen_slot, frozen_bank, next_bank_slot, next_bank_arc)
        };

        *block_commitment_cache.write().unwrap() = BlockCommitmentCache::new(
            std::collections::HashMap::new(),
            0,
            CommitmentSlots {
                slot: frozen_slot,
                root: frozen_slot,
                highest_confirmed_slot: frozen_slot,
                highest_super_majority_root: frozen_slot,
            },
        );
        *optimistically_confirmed_bank.write().unwrap() =
            OptimisticallyConfirmedBank { bank: frozen_bank };

        if let Some(subs) = rpc_subscriptions {
            subs.notify_slot(next_bank_slot, frozen_slot, next_bank_slot);
        }

        next_bank_arc
    }

    pub fn new(
        parent_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        settings: EphemeralRollupSettings,
        rpc_addr: SocketAddr,
        ws_addr: SocketAddr,
        tpu_addr: SocketAddr,
        portal_program_id: Pubkey,
        manager_keypair: Arc<Keypair>,
    ) -> Result<Self, String> {
        // Place ER slots far above L1 slots so the shared AccountsDb root
        // tracker never sees an out-of-order add_root from either side.
        let ephemeral_slot = Self::er_slot_for(&parent_bank);
        info!(
            "EphemeralRuntime::new: parent_slot={}, ephemeral_slot={}, parent_epoch={}, \
             slots_per_epoch={}",
            parent_bank.slot(),
            ephemeral_slot,
            parent_bank.epoch(),
            parent_bank.get_slots_in_epoch(parent_bank.epoch()),
        );
        let bank = Bank::new_from_parent_ephemeral_isolated(
            parent_bank.clone(),
            &Pubkey::default(),
            ephemeral_slot,
        );
        bank.set_callback(Some(Box::new(NoopDropCallback)));
        info!(
            "EphemeralRuntime::new: ER bank created, slot={}, epoch={}",
            bank.slot(),
            bank.epoch(),
        );

        // The bank inherits tick_height from the L1 parent, but max_tick_height
        // is (ephemeral_slot + 1) * ticks_per_slot.  When the ER slot is in the
        // same epoch (not offset by 1 trillion), the gap is smaller, but we
        // still warp tick_height so only one slot's worth of ticks remains.
        Self::prepare_initial_working_bank(&bank);

        let bank_forks = BankForks::new_rw_arc_ephemeral(bank);
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
        let bank_operation_lock = Arc::new(Mutex::new(()));
        // Sonic: Starts inactive — transactions rejected until activate() is called
        let active = Arc::new(AtomicBool::new(false));
        let session_pda: Arc<RwLock<Option<Pubkey>>> = Arc::new(RwLock::new(None));
        let er_history_store = Arc::new(ErHistoryStore::default());
        let tx_client = EphemeralTransactionClient::new_with_history(
            bank_forks.clone(),
            bank_operation_lock.clone(),
            delegated_set.clone(),
            touched_accounts.clone(),
            active.clone(),
            er_history_store.clone(),
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

        let initial_bank = Self::freeze_and_rotate_bank_for_rpc(
            &bank_forks,
            &block_commitment_cache,
            &optimistically_confirmed_bank,
            None,
            &er_history_store,
        );

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
                .worker_threads(16)
                .max_blocking_threads(16)
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?,
        );

        let rpc_config = JsonRpcConfig {
            full_api: true,
            enable_rpc_transaction_history: false,
            disable_health_check: true,
            rpc_threads: 16,
            rpc_blocking_threads: 16,
            ..JsonRpcConfig::default()
        };

        let advertised_rpc_addr = Self::advertise_addr(rpc_addr);
        let advertised_ws_addr = Self::advertise_addr(ws_addr);
        let advertised_tpu_addr = Self::advertise_addr(tpu_addr);

        let rpc_service = JsonRpcService::new_with_client(
            rpc_addr,
            rpc_config,
            None,
            bank_forks.clone(),
            block_commitment_cache.clone(),
            blockstore.clone(),
            cluster_info.clone(),
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
            max_complete_transaction_status_slot.clone(),
            None,
            runtime.clone(),
            Some(delegated_set.clone()),
            Some(session_pda.clone()),
            Some(er_history_store.clone()),
            Some(ErNodeInfo {
                identity: manager_keypair.pubkey(),
                rpc: advertised_rpc_addr,
                pubsub: advertised_ws_addr,
                tpu_quic: advertised_tpu_addr,
                er_root_slot: ephemeral_slot,
                slot_duration_ms: 400,
            }),
            Some(Arc::new(tx_client.clone()) as Arc<dyn solana_rpc::rpc::ErTxExecutor>),
        )?;

        // Sonic: Start PubSub WebSocket service
        let rpc_subscriptions = Arc::new(RpcSubscriptions::new_with_config(
            rpc_exit.clone(),
            max_complete_transaction_status_slot,
            blockstore,
            bank_forks.clone(),
            block_commitment_cache.clone(),
            optimistically_confirmed_bank.clone(),
            &PubSubConfig::default(),
            None,
        ));

        let (pubsub_service, pubsub_trigger) = {
            let (trigger, pubsub_svc) =
                PubSubService::new(PubSubConfig::default(), &rpc_subscriptions, ws_addr);
            (Some(pubsub_svc), Some(trigger))
        };

        info!("EphemeralRuntime PubSub listening at {ws_addr}");

        // Sonic: Start TPU QUIC endpoint
        let tpu_socket = solana_net_utils::sockets::bind_to(tpu_addr.ip(), tpu_addr.port())
            .map_err(|e| format!("Failed to bind ER TPU socket on {tpu_addr}: {e}"))?;
        let actual_tpu_addr = tpu_socket
            .local_addr()
            .map_err(|e| format!("Failed to get ER TPU local addr: {e}"))?;
        let tpu = EphemeralTpu::new(
            tpu_socket,
            &manager_keypair,
            tx_client.clone(),
            rpc_exit.clone(),
        )
        .map_err(|e| format!("Failed to start ER TPU: {e}"))?;

        info!("EphemeralRuntime TPU listening at {actual_tpu_addr}");

        info!(
            "EphemeralRuntime listening at {rpc_addr} with working slot {}",
            initial_bank.slot()
        );

        // Slot advancer is NOT started here — it will be started
        // when activate() is called (session opened).
        let retired_bank_forks_pending = Arc::new(AtomicU64::new(0));
        #[cfg(test)]
        let retired_bank_forks_reaper_pause = Arc::new(AtomicBool::new(false));
        let (retired_bank_forks_sender, retired_bank_forks_reaper) = Self::spawn_bank_forks_reaper(
            retired_bank_forks_pending.clone(),
            #[cfg(test)]
            retired_bank_forks_reaper_pause.clone(),
        );

        Ok(Self {
            bank_forks,
            bank_operation_lock,
            block_commitment_cache,
            optimistically_confirmed_bank,
            rpc_service,
            rpc_addr,
            pubsub_service,
            pubsub_trigger,
            rpc_subscriptions,
            ws_addr,
            tpu: Some(tpu),
            tpu_addr: actual_tpu_addr,
            rpc_exit,
            advancer_exit,
            slot_advancer: None,
            initial_account_snapshots,
            delegated_accounts: delegated_set,
            touched_accounts,
            l1_anchor_bank: parent_bank,
            retired_bank_forks_sender: Some(retired_bank_forks_sender),
            retired_bank_forks_reaper: Some(retired_bank_forks_reaper),
            retired_bank_forks_pending,
            #[cfg(test)]
            retired_bank_forks_reaper_pause,
            active,
            session_pda,
            er_history_store,
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

    /// Sonic: Get the WebSocket address.
    pub fn ws_addr(&self) -> String {
        format!("ws://{}", self.ws_addr)
    }

    /// Sonic: Get the TPU QUIC address.
    pub fn tpu_addr(&self) -> SocketAddr {
        self.tpu_addr
    }

    /// Sonic: Activate the ephemeral rollup — starts slot advancer
    /// and begins accepting transactions.
    pub fn activate(&mut self) {
        info!("Activating ephemeral rollup at {}", self.rpc_addr);
        self.active.store(true, Ordering::Relaxed);

        // Start slot advancer if not already running
        if self.slot_advancer.is_none() {
            let advancer_exit = Arc::new(AtomicBool::new(false));
            self.advancer_exit = advancer_exit.clone();
            let initial_bank = self.bank_forks.read().unwrap().working_bank();
            self.slot_advancer = Some(crate::slot_advancer::SlotAdvancer::new_with_history(
                self.bank_forks.clone(),
                self.bank_operation_lock.clone(),
                self.block_commitment_cache.clone(),
                self.optimistically_confirmed_bank.clone(),
                initial_bank,
                crate::slot_advancer::Config {
                    slot_duration: Duration::from_millis(400),
                    manager_account: Pubkey::default(),
                },
                advancer_exit,
                Some(self.rpc_subscriptions.clone()),
                Some(self.er_history_store.clone()),
            ));
        }
    }

    /// Sonic: Deactivate the ephemeral rollup — stops slot advancer
    /// and rejects transactions.
    pub fn deactivate(&mut self) {
        info!("Deactivating ephemeral rollup at {}", self.rpc_addr);
        self.active.store(false, Ordering::Relaxed);

        // Stop slot advancer
        self.advancer_exit.store(true, Ordering::Relaxed);
        if let Some(advancer) = self.slot_advancer.take() {
            advancer.join();
        }

        // Clear session PDA
        *self.session_pda.write().unwrap() = None;
    }

    /// Sonic: Check if the ephemeral rollup is accepting transactions.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Sonic: Set the current session PDA.
    pub fn set_session_pda(&self, pda: Pubkey) {
        *self.session_pda.write().unwrap() = Some(pda);
    }

    /// Sonic: Get a clone of the session PDA Arc for sharing with RPC.
    pub fn session_pda(&self) -> Arc<RwLock<Option<Pubkey>>> {
        self.session_pda.clone()
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
        // Stop TPU
        if let Some(mut tpu) = self.tpu.take() {
            tpu.shutdown();
        }
        // Stop PubSub (Trigger drop cancels the Tripwire)
        drop(self.pubsub_trigger.take());
        if let Some(pubsub) = self.pubsub_service.take() {
            let _ = pubsub.close();
        }
        // Then stop RPC service
        self.rpc_exit.store(true, Ordering::Relaxed);
        self.rpc_service.exit();
        Self::purge_bank_forks_slots(&self.bank_forks.read().unwrap());
        drop(self.retired_bank_forks_sender.take());
        if let Some(reaper) = self.retired_bank_forks_reaper.take() {
            let _ = reaper.join();
        }
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

        let initial_bank = {
            let _bank_operation_guard = self.bank_operation_lock.lock().unwrap();

            // 2. Create new ephemeral bank from current L1 root
            let current_er_tip = self.bank_forks.read().unwrap().working_bank().slot();
            let ephemeral_slot = Self::er_slot_for(&parent_bank).max(current_er_tip.saturating_add(1));
            info!(
                "reset_to_new_parent: parent_slot={}, ephemeral_slot={}, parent_epoch={}",
                parent_bank.slot(),
                ephemeral_slot,
                parent_bank.epoch(),
            );
            let bank = Bank::new_from_parent_ephemeral_isolated(
                parent_bank.clone(),
                &Pubkey::default(),
                ephemeral_slot,
            );
            bank.set_callback(Some(Box::new(NoopDropCallback)));
            info!(
                "reset_to_new_parent: ER bank created, slot={}, epoch={}",
                bank.slot(),
                bank.epoch(),
            );
            Self::prepare_initial_working_bank(&bank);

            // 3. Swap BankForks in-place — same Arc, new contents.
            //    All holders (RPC service, tx_client) see the new bank.
            let new_bf_arc = BankForks::new_rw_arc_ephemeral(bank);
            let new_bf = Arc::try_unwrap(new_bf_arc)
                .unwrap_or_else(|_| panic!("just created, refcount must be 1"))
                .into_inner()
                .expect("lock not poisoned");
            let old_bf = std::mem::replace(&mut *self.bank_forks.write().unwrap(), new_bf);
            self.retire_bank_forks(old_bf);

            // 4. Publish frozen ER bank for RPC/preflight, keep fresh child as working bank.
            let initial_bank = Self::freeze_and_rotate_bank_for_rpc(
                &self.bank_forks,
                &self.block_commitment_cache,
                &self.optimistically_confirmed_bank,
                Some(&self.rpc_subscriptions),
                &self.er_history_store,
            );

            // 5. Keep new L1 anchor alive even if ER root later severs its
            // parent link to keep ER chains shallow.
            self.l1_anchor_bank = parent_bank;

            // 6. Clear session state
            self.initial_account_snapshots.clear();
            self.delegated_accounts.write().unwrap().clear();
            self.touched_accounts.write().unwrap().clear();

            initial_bank
        };

        // 7. Start new slot advancer
        let advancer_exit = Arc::new(AtomicBool::new(false));
        self.advancer_exit = advancer_exit.clone();
        let slot = initial_bank.slot();
        self.slot_advancer = Some(crate::slot_advancer::SlotAdvancer::new_with_history(
            self.bank_forks.clone(),
            self.bank_operation_lock.clone(),
            self.block_commitment_cache.clone(),
            self.optimistically_confirmed_bank.clone(),
            initial_bank,
            crate::slot_advancer::Config {
                slot_duration: Duration::from_millis(400),
                manager_account: Pubkey::default(),
            },
            advancer_exit,
            Some(self.rpc_subscriptions.clone()),
            Some(self.er_history_store.clone()),
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

    fn publish_bank_for_rpc(&self) {
        Self::freeze_and_rotate_bank_for_rpc(
            &self.bank_forks,
            &self.block_commitment_cache,
            &self.optimistically_confirmed_bank,
            Some(&self.rpc_subscriptions),
            &self.er_history_store,
        );
    }

    /// Handle a new account delegation from L1.
    /// Copies the account data from L1 into the ER bank and adds it to the
    /// delegated accounts set so transactions can write to it.
    pub fn handle_delegation(&self, delegated_account: &Pubkey, account_data: AccountSharedData) {
        let _bank_operation_guard = self.bank_operation_lock.lock().unwrap();
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

        self.publish_bank_for_rpc();

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
        let _bank_operation_guard = self.bank_operation_lock.lock().unwrap();
        let bank = self.bank();
        let mut account = bank.get_account(depositor).unwrap_or_default();
        let was_delegated = self.delegated_accounts.read().unwrap().contains(depositor);
        let was_touched = self.touched_accounts.read().unwrap().contains(depositor);

        // Untouched, non-delegated accounts inherit L1 lamports into the ER bank.
        // Deposits must materialize only the deposited amount, not L1 balance + deposit.
        let base_balance = if was_delegated || was_touched {
            account.lamports()
        } else {
            0
        };
        let new_balance = base_balance.saturating_add(lamports);
        account.set_lamports(new_balance);
        // Ensure the account is owned by system program when materializing a new balance.
        if account.owner() == &Pubkey::default() {
            account.set_owner(solana_sdk_ids::system_program::id());
        }
        bank.store_account(depositor, &account);

        // Mark as touched so the balance isn't zeroed later
        self.touched_accounts.write().unwrap().insert(*depositor);

        self.publish_bank_for_rpc();

        info!(
            "Credited {} lamports to {} on ER (base: {}, new balance: {})",
            lamports, depositor, base_balance, new_balance
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
        solana_account::{AccountSharedData, ReadableAccount, WritableAccount},
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
        solana_transaction_status::{TransactionConfirmationStatus, UiTransactionEncoding},
        std::{net::TcpListener, sync::atomic::AtomicU64, time::Duration},
    };

    #[derive(Debug, Clone)]
    struct CountingDropCallback(Arc<AtomicU64>);

    impl DropCallback for CountingDropCallback {
        fn callback(&self, _bank: &Bank) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }

        fn clone_box(&self) -> Box<dyn DropCallback + Send + Sync> {
            Box::new(self.clone())
        }
    }

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
            find_free_addr(),
            find_free_addr(),
            portal_program_id,
            Arc::new(Keypair::new()),
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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
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
    fn test_er_history_get_transaction_and_signature_status_after_reset() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let sender_keypair = Keypair::new();
        let sender_pubkey = sender_keypair.pubkey();
        let receiver_pubkey = Pubkey::new_unique();
        let sender_initial = 100_000_000_000u64;
        let transfer_amount = 10_000_000_000u64;
        fund_account(&parent_bank, &sender_pubkey, sender_initial);
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
        )
        .unwrap();
        runtime.activate();
        let rpc_client = rpc_client(&runtime);

        std::thread::sleep(Duration::from_secs(2));

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
        let signature = rpc_client
            .send_transaction_with_config(&tx, config)
            .unwrap();

        std::thread::sleep(Duration::from_secs(2));

        let confirmed_tx = rpc_client
            .get_transaction(&signature, UiTransactionEncoding::Json)
            .unwrap();
        assert_eq!(confirmed_tx.transaction_index, Some(0));
        let confirmed_meta = confirmed_tx
            .transaction
            .meta
            .as_ref()
            .expect("transaction meta should be present");
        assert!(confirmed_meta.err.is_none());
        assert!(
            matches!(
                confirmed_meta.log_messages.as_ref(),
                solana_transaction_status::option_serializer::OptionSerializer::Some(logs)
                    if !logs.is_empty()
            ),
            "ER transaction history should preserve Solana log messages"
        );

        let new_parent = Bank::new_from_parent(parent_bank, &Pubkey::default(), 1);
        new_parent.freeze();
        runtime.reset_to_new_parent(Arc::new(new_parent));

        let confirmed_tx_after_reset = rpc_client
            .get_transaction(&signature, UiTransactionEncoding::Json)
            .unwrap();
        assert_eq!(confirmed_tx_after_reset.slot, confirmed_tx.slot);

        let statuses = rpc_client
            .get_signature_statuses_with_history(&[signature])
            .unwrap()
            .value;
        let status = statuses
            .into_iter()
            .next()
            .flatten()
            .expect("history status should be available after reset");
        assert_eq!(
            status.confirmation_status,
            Some(TransactionConfirmationStatus::Finalized)
        );
        assert!(status.err.is_none());

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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
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

        // sendTransaction RPC must survive preflight even while runtime is inactive.
        // The tx is still dropped internally because the runtime rejects execution.
        rpc_client
            .send_transaction_with_config(&tx, RpcSendTransactionConfig::default())
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
    fn test_reset_to_new_parent_retires_old_bank_forks_async() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
        )
        .unwrap();
        runtime.activate();

        std::thread::sleep(Duration::from_millis(200));
        runtime.deactivate();
        runtime.set_bank_forks_reaper_paused(true);

        let old_er_tip = runtime.bank().slot();
        let new_parent = Arc::new(Bank::new_from_parent(parent_bank, &Pubkey::default(), 1));
        let expected_er_root_slot =
            EphemeralRuntime::er_slot_for(new_parent.as_ref()).max(old_er_tip.saturating_add(1));
        let expected_er_slot = expected_er_root_slot.saturating_add(1);
        runtime.reset_to_new_parent(new_parent);

        assert_eq!(
            runtime.bank().slot(),
            expected_er_slot,
            "reset should publish fresh ER bank immediately"
        );
        assert_eq!(
            runtime.retired_bank_forks_pending(),
            1,
            "old ER forks should be queued for async reaping"
        );

        runtime.set_bank_forks_reaper_paused(false);
        for _ in 0..50 {
            if runtime.retired_bank_forks_pending() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(runtime.retired_bank_forks_pending(), 0);

        runtime.shutdown();
    }

    #[test]
    fn test_reset_to_new_parent_purges_old_er_slots() {
        agave_logger::setup();

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
        let mut runtime = EphemeralRuntime::new(
            parent_bank.clone(),
            cluster_info,
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
        )
        .unwrap();
        runtime.activate();

        let depositor = Pubkey::new_unique();
        for _ in 0..4 {
            runtime.credit_deposit(&depositor, 1);
        }
        runtime.deactivate();

        let old_er_slots: Vec<_> = runtime
            .bank()
            .parents_inclusive()
            .into_iter()
            .map(|bank| bank.slot())
            .filter(|slot| *slot >= (1u64 << 40))
            .collect();
        assert!(!old_er_slots.is_empty());
        assert!(old_er_slots.iter().any(|slot| {
            !parent_bank
                .rc
                .accounts
                .accounts_db
                .get_pubkeys_for_slot(*slot)
                .is_empty()
        }));

        let new_parent = Arc::new(Bank::new_from_parent(parent_bank.clone(), &Pubkey::default(), 1));
        runtime.reset_to_new_parent(new_parent);
        for _ in 0..50 {
            if runtime.retired_bank_forks_pending() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(runtime.retired_bank_forks_pending(), 0);

        for slot in old_er_slots {
            assert!(
                parent_bank
                    .rc
                    .accounts
                    .accounts_db
                    .get_pubkeys_for_slot(slot)
                    .is_empty(),
                "old ER slot {slot} should be purged during async retirement"
            );
        }

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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
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
    fn test_er_banks_do_not_inherit_l1_drop_callback() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let dropped = Arc::new(AtomicU64::new(0));
        parent_bank.set_callback(Some(Box::new(CountingDropCallback(dropped.clone()))));
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
        )
        .unwrap();
        runtime.activate();

        std::thread::sleep(Duration::from_millis(150));
        let new_parent = Arc::new(Bank::new_from_parent(parent_bank, &Pubkey::default(), 1));
        runtime.reset_to_new_parent(new_parent);
        runtime.shutdown();

        assert_eq!(
            dropped.load(Ordering::Relaxed),
            0,
            "ER banks should not forward drops into the L1 callback queue"
        );
    }

    #[test]
    fn test_publish_bank_for_rpc_keeps_parent_chain_shallow() {
        agave_logger::setup();

        let (_, mut runtime) = create_runtime();
        runtime.activate();

        let depositor = Pubkey::new_unique();
        for _ in 0..64 {
            runtime.credit_deposit(&depositor, 1);
        }

        let working_bank = runtime.bank();
        assert!(
            working_bank.parents().len() <= 2,
            "ER working bank parent chain grew too deep: {}",
            working_bank.parents().len()
        );

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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
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
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
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
    fn test_handle_delegation_preserves_owner_data_executable_over_rpc_commitments() {
        agave_logger::setup();

        let parent_bank = Arc::new(create_test_bank());
        let portal_program_id = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();

        let l1_pre_delegate = AccountSharedData::new(1_000_000, 0, &system_program::id());
        parent_bank.store_account(&delegated_pubkey, &l1_pre_delegate);
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
            parent_bank,
            cluster_info,
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            portal_program_id,
            Arc::new(Keypair::new()),
        )
        .unwrap();

        runtime.activate();

        let initial_er_slot = runtime.bank().slot();
        let start = std::time::Instant::now();
        while runtime.bank().slot() == initial_er_slot {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "ER slot advancer did not move past initial slot"
            );
            std::thread::sleep(Duration::from_millis(25));
        }

        let mut delegated_l1_snapshot = AccountSharedData::new(1_000_000, 4, &portal_program_id);
        delegated_l1_snapshot
            .data_as_mut_slice()
            .copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        delegated_l1_snapshot.set_executable(true);

        runtime.handle_delegation(&delegated_pubkey, delegated_l1_snapshot.clone());

        let rpc_client = rpc_client(&runtime);
        let rpc_account = rpc_client
            .get_account_with_commitment(&delegated_pubkey, CommitmentConfig::finalized())
            .unwrap()
            .value
            .expect("delegated account should be visible over finalized RPC");

        assert_eq!(
            rpc_account.owner(),
            delegated_l1_snapshot.owner(),
            "ER RPC should expose delegated owner from L1 snapshot"
        );
        assert_eq!(
            rpc_account.data(),
            delegated_l1_snapshot.data(),
            "ER RPC should expose delegated data from L1 snapshot"
        );
        assert_eq!(
            rpc_account.executable(),
            delegated_l1_snapshot.executable(),
            "ER RPC should expose delegated executable flag from L1 snapshot"
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
            find_free_addr(),
            find_free_addr(),
            portal_program_id,
            Arc::new(Keypair::new()),
        )
        .unwrap();

        // Verify the delegated account is tracked
        assert!(runtime.delegated_accounts().contains(&delegated_pubkey));

        // Verify snapshot is stored
        assert!(
            runtime
                .initial_account_snapshot(&delegated_pubkey)
                .is_some()
        );

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
            find_free_addr(),
            find_free_addr(),
            portal_program_id,
            Arc::new(Keypair::new()),
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
            find_free_addr(),
            find_free_addr(),
            portal_program_id,
            Arc::new(Keypair::new()),
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
            find_free_addr(),
            find_free_addr(),
            portal_program_id,
            Arc::new(Keypair::new()),
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
            find_free_addr(),
            find_free_addr(),
            portal_program_id,
            Arc::new(Keypair::new()),
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

    /// Regression: https://linear / devnet panic observed at
    /// `accounts-db/src/accounts_db.rs:4229` — `purge_slot_cache_pubkeys`
    /// asserts that the purged slot has no backing storage entry. On a
    /// long-running devnet validator the L1 eventually reaches a slot that
    /// the ER has already populated in the shared `AccountsDb`, and the
    /// assertion fires.
    ///
    /// `er_slot_for` must place the ER far enough ahead of the parent that
    /// L1 can never realistically catch up during a session. The old
    /// implementation only kept the ER within the parent's epoch and fell
    /// through to `parent.slot() + 1` once the parent was past the
    /// "last quarter of the epoch" watermark — i.e. zero effective gap.
    #[test]
    fn test_er_slot_stays_far_ahead_of_parent() {
        use solana_genesis_config::GenesisConfig;
        // Minimum gap ER must keep from L1. At 2.5 slots/s, 2^30 slots is
        // ~13 years of continuous L1 advancement. Anything smaller is
        // reachable on a real long-running network.
        const MIN_GAP: u64 = 1u64 << 30;

        // Case 1: fresh parent bank at slot 0.
        let fresh_parent = Arc::new(create_test_bank());
        let er_slot = EphemeralRuntime::er_slot_for(&fresh_parent);
        assert!(
            er_slot.saturating_sub(fresh_parent.slot()) >= MIN_GAP,
            "fresh parent: ER slot {} is only {} slots ahead of parent slot {}; need at least {}",
            er_slot,
            er_slot - fresh_parent.slot(),
            fresh_parent.slot(),
            MIN_GAP,
        );

        // Case 2: parent bank deep into an epoch — simulates a long-running
        // mainnet/devnet validator. We construct a chain of child banks
        // whose final slot lands past the old `er_base` watermark
        // (`epoch_start + slots_per_epoch * 3 / 4`) which is where the old
        // implementation collapsed the gap to 1.
        let genesis_config = GenesisConfig::new(&[], &[]);
        let root_bank = Arc::new(Bank::new_for_tests(&genesis_config));
        let slots_per_epoch = root_bank.get_slots_in_epoch(root_bank.epoch());
        // Pick a target slot in the final quarter of epoch 0 so that the
        // old `er_base = slots_per_epoch * 3 / 4` is exceeded.
        let target_slot = slots_per_epoch - 4;
        let mut parent = root_bank;
        for s in 1..=target_slot {
            let next = Bank::new_from_parent(parent, &Pubkey::default(), s);
            parent = Arc::new(next);
        }
        assert!(
            parent.slot() > slots_per_epoch * 3 / 4,
            "test setup: parent.slot {} should be past 3/4 of epoch {}",
            parent.slot(),
            slots_per_epoch
        );

        let er_slot = EphemeralRuntime::er_slot_for(&parent);
        assert!(
            er_slot.saturating_sub(parent.slot()) >= MIN_GAP,
            "late-epoch parent: ER slot {} is only {} slots ahead of parent slot {}; need at \
             least {} to avoid L1/ER AccountsDb collisions",
            er_slot,
            er_slot - parent.slot(),
            parent.slot(),
            MIN_GAP,
        );
    }
}
