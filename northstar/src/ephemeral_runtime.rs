use {
    crate::{
        ephemeral_tpu::EphemeralTpu, ephemeral_tx_client::EphemeralTransactionClient,
        settlement::ReceiptBalanceSettlement, slot_advancer::SlotAdvancer, EphemeralRollupSettings,
    },
    crossbeam_channel::{unbounded, Sender},
    log::{debug, info, warn},
    solana_account::{state_traits::StateMut, AccountSharedData, ReadableAccount, WritableAccount},
    solana_accounts_db::accounts_db::AccountsDb,
    solana_clock::{BankId, Slot},
    solana_gossip::cluster_info::ClusterInfo,
    solana_keypair::Keypair,
    solana_lattice_hash::lt_hash::{Checksum, LtHash},
    solana_leader_schedule::SlotLeader,
    solana_ledger::{blockstore::Blockstore, leader_schedule_cache::LeaderScheduleCache},
    solana_loader_v3_interface::state::UpgradeableLoaderState,
    solana_pubkey::Pubkey,
    solana_rpc::{
        er_history::ErHistoryStore,
        max_slots::MaxSlots,
        northstar::NorthStarSyncStatus,
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
    solana_sdk_ids::bpf_loader_upgradeable,
    solana_send_transaction_service::send_transaction_service,
    solana_signer::Signer,
    solana_svm::account_loader::PROGRAM_OWNERS,
    std::{
        collections::{HashMap, HashSet},
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc, Mutex, RwLock,
        },
        thread::{Builder, JoinHandle},
        time::Duration,
    },
    tempfile::TempDir,
    tokio::runtime::Runtime as TokioRuntime,
};

// Sonic: ER banks inherit the L1 drop callback from their parent, which would flood
// the L1 pruned-bank queue on ER teardown. ER slots are purged explicitly instead.
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

/// One account whose ER state hash differs from the current L1 anchor state hash.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ErStateDiffAccount {
    pub pubkey: Pubkey,
    pub l1_account: Option<AccountSharedData>,
    pub er_account: AccountSharedData,
    pub l1_lt_hash: LtHash,
    pub er_lt_hash: LtHash,
}

/// Lattice-hash accumulator for all account changes that exist in ER but not L1.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ErStateDiff {
    pub accounts: Vec<ErStateDiffAccount>,
    pub lt_hash: LtHash,
}

impl ErStateDiff {
    pub fn checksum(&self) -> Checksum {
        self.lt_hash.checksum()
    }
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
    /// Sonic: Thin in-memory source of truth for ER-local account writes across
    /// L1 reanchors. Rehydrated into the current Bank so existing SVM/RPC code
    /// paths can keep using normal Bank account access.
    er_account_overlay: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
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
    /// Sonic: L1 sync cursor, shared with the RPC handler.
    sync_status: Arc<NorthStarSyncStatus>,
    /// Sonic: In-memory ER transaction history for Phase 1 history APIs.
    er_history_store: Arc<ErHistoryStore>,
    portal_program_id: Pubkey,

    _tx_client: EphemeralTransactionClient,
    settings: EphemeralRollupSettings,
    slot_duration: Duration,
    _ledger_dir: TempDir,
    _runtime: Arc<TokioRuntime>,
}

impl EphemeralRuntime {
    fn delegation_record_pda(
        portal_program_id: &Pubkey,
        delegated_account: &Pubkey,
    ) -> (Pubkey, u8) {
        let (pda, bump) = northstar_portal::find_delegation_record_pda(
            &portal_program_id.to_bytes(),
            &delegated_account.to_bytes(),
        );
        (Pubkey::new_from_array(pda), bump)
    }

    fn effective_delegated_account(
        parent_bank: &Bank,
        portal_program_id: &Pubkey,
        grid_id: u64,
        delegated_account: &Pubkey,
        account: &AccountSharedData,
    ) -> Option<AccountSharedData> {
        if account.owner() != portal_program_id {
            warn!(
                "Account {} listed as delegated but owned by {}, not portal program {}. Skipping.",
                delegated_account,
                account.owner(),
                portal_program_id,
            );
            return None;
        }

        let (record_pubkey, _) = Self::delegation_record_pda(portal_program_id, delegated_account);
        let Some(record_account) = parent_bank.get_account(&record_pubkey) else {
            warn!(
                "Delegated account {delegated_account} missing delegation record {record_pubkey}"
            );
            return None;
        };
        let Some(crate::portal_state::PortalAccount::DelegationRecord(record)) =
            crate::portal_state::try_parse_raw_portal_account(record_account.data())
        else {
            warn!("Delegation record {record_pubkey} has invalid account data");
            return None;
        };

        if record.grid_id != grid_id {
            warn!(
                "Delegation record {} grid {} does not match active grid {}",
                record_pubkey, record.grid_id, grid_id,
            );
            return None;
        }

        let owner_program: Pubkey = record.owner_program.into();
        let mut effective_account = account.clone();
        effective_account.set_owner(owner_program);
        Some(effective_account)
    }

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
        er_fee_structure: &solana_fee_structure::FeeStructure,
        recent_blockhash_max_age: usize,
    ) -> Arc<Bank> {
        let (frozen_slot, frozen_bank, next_bank_slot, next_bank_arc) = {
            let current_bank = bank_forks.read().unwrap().working_bank();
            current_bank.freeze();
            er_history_store.finalize_slot(&current_bank);

            let frozen_slot = current_bank.slot();
            let frozen_bank = current_bank.clone();
            let next_bank_slot = frozen_slot.saturating_add(1);
            let mut next_bank = Bank::new_from_parent_ephemeral(
                current_bank,
                SlotLeader::default(),
                next_bank_slot,
            );
            next_bank.configure_er(er_fee_structure, recent_blockhash_max_age);
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
                    parent.disconnect_from_parent();
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
            subs.notify_roots(vec![next_bank_slot]);
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
        Self::new_with_slot_duration(
            parent_bank,
            cluster_info,
            settings,
            rpc_addr,
            ws_addr,
            tpu_addr,
            portal_program_id,
            manager_keypair,
            crate::DEFAULT_ER_SLOT_DURATION,
        )
    }

    pub fn new_with_slot_duration(
        parent_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        settings: EphemeralRollupSettings,
        rpc_addr: SocketAddr,
        ws_addr: SocketAddr,
        tpu_addr: SocketAddr,
        portal_program_id: Pubkey,
        manager_keypair: Arc<Keypair>,
        slot_duration: Duration,
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
        let transaction_max_age = crate::er_transaction_max_age_for_slot_duration(slot_duration);
        let recent_blockhash_max_age =
            crate::er_recent_blockhash_max_age_for_slot_duration(slot_duration);
        let mut bank = Bank::new_from_parent_ephemeral_isolated(
            parent_bank.clone(),
            SlotLeader::default(),
            ephemeral_slot,
        );
        bank.configure_er(&settings.er_fee_structure, recent_blockhash_max_age);
        bank.set_callback(Some(Box::new(NoopDropCallback)));
        info!(
            "EphemeralRuntime::new: ER fees configured at {} lamports/signature",
            settings.er_fee_structure.lamports_per_signature
        );
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
            let Some(effective_account) = Self::effective_delegated_account(
                &parent_bank,
                &portal_program_id,
                settings.grid_id,
                pubkey,
                &account,
            ) else {
                continue;
            };
            info!("Delegated account {} validated and snapshotted", pubkey);
            initial_account_snapshots.insert(*pubkey, account);
            initial_bank.store_account(pubkey, &effective_account);
            delegated_accounts.insert(*pubkey);
        }

        info!(
            "EphemeralRuntime: {} of {} delegated accounts validated",
            delegated_accounts.len(),
            settings.delegated_accounts.len(),
        );

        let delegated_set = Arc::new(RwLock::new(delegated_accounts.clone()));
        let touched_accounts = Arc::new(RwLock::new(HashSet::new()));
        let er_account_overlay = Arc::new(RwLock::new(
            initial_account_snapshots
                .iter()
                .filter_map(|(pubkey, account)| {
                    Self::effective_delegated_account(
                        &parent_bank,
                        &portal_program_id,
                        settings.grid_id,
                        pubkey,
                        account,
                    )
                    .map(|effective| (*pubkey, effective))
                })
                .collect::<HashMap<_, _>>(),
        ));
        let bank_operation_lock = Arc::new(Mutex::new(()));
        // Sonic: Starts inactive — transactions rejected until activate() is called
        let active = Arc::new(AtomicBool::new(false));
        let session_pda: Arc<RwLock<Option<Pubkey>>> = Arc::new(RwLock::new(None));
        let sync_status = Arc::new(NorthStarSyncStatus::new(parent_bank.slot()));
        let er_history_store = Arc::new(ErHistoryStore::default());

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
        let tx_client =
            EphemeralTransactionClient::new_with_history_overlay_commitment_cache_and_transaction_max_age(
                bank_forks.clone(),
                bank_operation_lock.clone(),
                delegated_set.clone(),
                touched_accounts.clone(),
                active.clone(),
                er_account_overlay.clone(),
                er_history_store.clone(),
                block_commitment_cache.clone(),
                transaction_max_age,
            );

        let optimistically_confirmed_bank = Arc::new(RwLock::new(OptimisticallyConfirmedBank {
            bank: Arc::clone(&initial_bank),
        }));

        let initial_bank = Self::freeze_and_rotate_bank_for_rpc(
            &bank_forks,
            &block_commitment_cache,
            &optimistically_confirmed_bank,
            None,
            &er_history_store,
            &settings.er_fee_structure,
            recent_blockhash_max_age,
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
                slot_duration_ms: u64::try_from(slot_duration.as_millis()).unwrap_or(u64::MAX),
            }),
            Some(sync_status.clone()),
            Some(Arc::new(tx_client.clone()) as Arc<dyn solana_rpc::rpc::ErTxExecutor>),
        )?;

        // Sonic: Start PubSub WebSocket service. ER block subscriptions are
        // backed by ErHistoryStore, so enable blockSubscribe on this local path.
        let pubsub_config = PubSubConfig {
            enable_block_subscription: true,
            ..PubSubConfig::default_for_tests()
        };
        let rpc_subscriptions = Arc::new(RpcSubscriptions::new_with_config_and_er_history(
            rpc_exit.clone(),
            max_complete_transaction_status_slot,
            blockstore,
            bank_forks.clone(),
            block_commitment_cache.clone(),
            optimistically_confirmed_bank.clone(),
            &pubsub_config,
            None,
            Some(er_history_store.clone()),
        ));

        tx_client.set_rpc_subscriptions(rpc_subscriptions.clone());

        let (pubsub_service, pubsub_trigger) = {
            let (trigger, pubsub_svc) =
                PubSubService::new(pubsub_config, &rpc_subscriptions, ws_addr);
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
            er_account_overlay,
            l1_anchor_bank: parent_bank,
            retired_bank_forks_sender: Some(retired_bank_forks_sender),
            retired_bank_forks_reaper: Some(retired_bank_forks_reaper),
            retired_bank_forks_pending,
            #[cfg(test)]
            retired_bank_forks_reaper_pause,
            active,
            session_pda,
            sync_status,
            er_history_store,
            portal_program_id,

            settings,
            slot_duration,
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
                    slot_duration: self.slot_duration,
                    manager_account: Pubkey::default(),
                    er_fee_structure: self.settings.er_fee_structure.clone(),
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

    /// Sonic: Apply settings from the active L1 session before resetting ER state.
    pub fn set_session_settings(&mut self, grid_id: u64, ttl_slots: u64, fee_cap: u64) {
        self.settings.grid_id = grid_id;
        self.settings.ttl_slots = ttl_slots;
        self.settings.fee_cap = fee_cap;
    }

    /// Sonic: Get a clone of the session PDA Arc for sharing with RPC.
    pub fn session_pda(&self) -> Arc<RwLock<Option<Pubkey>>> {
        self.session_pda.clone()
    }

    /// Sonic: Update latest L1 slot observed by the NorthStar sync loop.
    pub fn update_latest_l1_slot(&self, slot: Slot) {
        self.sync_status.update_latest_l1_slot(slot);
    }

    /// Sonic: Mark L1 events synced through `slot`.
    pub fn mark_synced_through(&self, slot: Slot) {
        self.sync_status.mark_synced_through(slot);
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
        self.sync_status.update_latest_l1_slot(parent_bank.slot());
        self.sync_status.mark_synced_through(parent_bank.slot());

        // 1. Stop old slot advancer
        self.advancer_exit.store(true, Ordering::Relaxed);
        if let Some(advancer) = self.slot_advancer.take() {
            advancer.join();
        }

        let initial_bank = {
            let bank_operation_lock = self.bank_operation_lock.clone();
            let _bank_operation_guard = bank_operation_lock.lock().unwrap();

            // 2. Create new ephemeral bank from current L1 root
            let old_er_bank = self.bank_forks.read().unwrap().working_bank();
            let current_er_tip = old_er_bank.slot();
            let ephemeral_slot =
                Self::er_slot_for(&parent_bank).max(current_er_tip.saturating_add(1));
            info!(
                "reset_to_new_parent: parent_slot={}, ephemeral_slot={}, parent_epoch={}",
                parent_bank.slot(),
                ephemeral_slot,
                parent_bank.epoch(),
            );
            let recent_blockhash_max_age =
                crate::er_recent_blockhash_max_age_for_slot_duration(self.slot_duration);
            let mut bank = Bank::new_from_parent_ephemeral_isolated(
                parent_bank.clone(),
                SlotLeader::default(),
                ephemeral_slot,
            );
            bank.configure_er(&self.settings.er_fee_structure, recent_blockhash_max_age);
            let carried_blockhashes = bank.carry_forward_blockhashes_from(&old_er_bank);
            if carried_blockhashes > 0 {
                debug!("Carried {carried_blockhashes} ER recent blockhash(es) across reset");
            }
            bank.set_callback(Some(Box::new(NoopDropCallback)));
            info!(
                "reset_to_new_parent: ER fees configured at {} lamports/signature",
                self.settings.er_fee_structure.lamports_per_signature
            );
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

            // `new_rw_arc_ephemeral()` installed a Weak fork graph pointing at
            // the temporary Arc we just unwrapped. Rebind the isolated ER
            // ProgramCache to the long-lived Arc shared by RPC/TPU/tx client.
            self.bank_forks
                .read()
                .unwrap()
                .root_bank()
                .set_fork_graph_in_program_cache(Arc::downgrade(&self.bank_forks));

            // 4. Keep new L1 anchor alive even if ER root later severs its
            // parent link to keep ER chains shallow.
            self.l1_anchor_bank = parent_bank;

            // 5. Clear and rehydrate session state from existing L1
            // delegations before publishing RPC commitments. `AccountDelegated`
            // events are emitted only when the record is created, so close/open
            // reset must not depend on seeing those historical events again.
            self.initial_account_snapshots.clear();
            self.delegated_accounts.write().unwrap().clear();
            self.touched_accounts.write().unwrap().clear();
            self.er_account_overlay.write().unwrap().clear();
            let mut accounts = Vec::new();
            if self
                .l1_anchor_bank
                .scan_all_accounts(|entry| {
                    if let Some((pubkey, account, slot)) = entry {
                        accounts.push((*pubkey, account, slot));
                    }
                })
                .is_err()
            {
                warn!("Cannot hydrate existing delegations from L1: account scan failed");
            }

            let hydrated_delegations = accounts
                .into_iter()
                .filter(|(_, account, _)| account.owner() == &self.portal_program_id)
                .filter_map(|(pubkey, account, _slot)| {
                    let (record_pubkey, _) =
                        Self::delegation_record_pda(&self.portal_program_id, &pubkey);
                    let record_account = self.l1_anchor_bank.get_account(&record_pubkey)?;
                    let Some(crate::portal_state::PortalAccount::DelegationRecord(record)) =
                        crate::portal_state::try_parse_raw_portal_account(record_account.data())
                    else {
                        warn!("Delegation record {record_pubkey} has invalid account data");
                        return None;
                    };
                    if record.grid_id != self.settings.grid_id {
                        return None;
                    }

                    let owner_program = Pubkey::from(record.owner_program);
                    let mut effective_account = account.clone();
                    effective_account.set_owner(owner_program);

                    let owner_program_account = match self
                        .l1_anchor_bank
                        .get_account(&owner_program)
                    {
                        Some(program_account) => {
                            let programdata = match (
                                program_account.owner() == &bpf_loader_upgradeable::id(),
                                program_account.state(),
                            ) {
                                (
                                    true,
                                    Ok(UpgradeableLoaderState::Program {
                                        programdata_address,
                                    }),
                                ) => match self.l1_anchor_bank.get_account(&programdata_address) {
                                    Some(programdata_account) => {
                                        Some((programdata_address, programdata_account))
                                    }
                                    None => {
                                        warn!(
                                            "Cannot hydrate owner program {owner_program}: \
                                             programdata account {programdata_address} not found \
                                             on L1"
                                        );
                                        None
                                    }
                                },
                                (true, Ok(_)) => {
                                    warn!(
                                        "Cannot hydrate owner program {owner_program}: \
                                         upgradeable-loader account is not Program state"
                                    );
                                    None
                                }
                                (true, Err(err)) => {
                                    warn!(
                                        "Cannot hydrate owner program {owner_program}: failed to \
                                         parse upgradeable-loader state: {err:?}"
                                    );
                                    None
                                }
                                _ => None,
                            };
                            Some((owner_program, program_account, programdata))
                        }
                        None => {
                            warn!(
                                "Cannot hydrate owner program {owner_program}: program account \
                                 not found on L1"
                            );
                            None
                        }
                    };

                    Some((pubkey, account, effective_account, owner_program_account))
                })
                .collect::<Vec<_>>();

            let er_bank = self.bank();
            let mut seen_owner_programs = HashSet::new();

            let account_writes = hydrated_delegations
                .iter()
                .flat_map(
                    |(pubkey, _account, effective_account, owner_program_account)| {
                        let owner_program_accounts = owner_program_account
                            .as_ref()
                            .filter(|(owner_program, _, _)| {
                                seen_owner_programs.insert(*owner_program)
                            })
                            .into_iter()
                            .flat_map(|(owner_program, program_account, programdata)| {
                                std::iter::once((owner_program, program_account)).chain(
                                    programdata.as_ref().map(
                                        |(programdata_address, programdata_account)| {
                                            (programdata_address, programdata_account)
                                        },
                                    ),
                                )
                            });

                        std::iter::once((pubkey, effective_account)).chain(owner_program_accounts)
                    },
                )
                .collect::<Vec<_>>();
            if !account_writes.is_empty() {
                er_bank.store_accounts((er_bank.slot(), account_writes.as_slice()));
            }
            Self::remove_reloadable_programs_from_cache(
                &er_bank,
                "reset hydration",
                hydrated_delegations
                    .iter()
                    .filter_map(|(_, _, _, owner_program_account)| {
                        owner_program_account
                            .as_ref()
                            .map(|(owner_program, program_account, _)| {
                                (*owner_program, program_account)
                            })
                    }),
            );

            self.initial_account_snapshots.extend(
                hydrated_delegations
                    .iter()
                    .map(|(pubkey, account, _, _)| (*pubkey, account.clone())),
            );
            self.delegated_accounts
                .write()
                .unwrap()
                .extend(hydrated_delegations.iter().map(|(pubkey, _, _, _)| *pubkey));
            self.touched_accounts
                .write()
                .unwrap()
                .extend(hydrated_delegations.iter().map(|(pubkey, _, _, _)| *pubkey));
            self.er_account_overlay.write().unwrap().extend(
                hydrated_delegations
                    .iter()
                    .map(|(pubkey, _, effective_account, _)| (*pubkey, effective_account.clone())),
            );

            let hydrated = hydrated_delegations.len();
            if hydrated > 0 {
                info!("Hydrated {hydrated} existing delegated account(s) from L1 after reset");
            }

            // 6. Publish frozen ER bank for RPC/preflight, keep fresh child as working bank.
            Self::freeze_and_rotate_bank_for_rpc(
                &self.bank_forks,
                &self.block_commitment_cache,
                &self.optimistically_confirmed_bank,
                Some(&self.rpc_subscriptions),
                &self.er_history_store,
                &self.settings.er_fee_structure,
                recent_blockhash_max_age,
            )
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
                slot_duration: self.slot_duration,
                manager_account: Pubkey::default(),
                er_fee_structure: self.settings.er_fee_structure.clone(),
            },
            advancer_exit,
            Some(self.rpc_subscriptions.clone()),
            Some(self.er_history_store.clone()),
        ));

        info!("EphemeralRuntime reset to new L1 parent, ER slot {}", slot);
    }

    fn apply_er_account_overlay_to_bank(&self, bank: &Bank) -> usize {
        let overlay_snapshot = self
            .er_account_overlay
            .read()
            .unwrap()
            .iter()
            .map(|(pubkey, account)| (*pubkey, account.clone()))
            .collect::<Vec<_>>();
        if overlay_snapshot.is_empty() {
            return 0;
        }

        let account_writes = overlay_snapshot
            .iter()
            .map(|(pubkey, account)| (pubkey, account))
            .collect::<Vec<_>>();
        bank.store_accounts((bank.slot(), account_writes.as_slice()));
        overlay_snapshot.len()
    }

    /// Sonic: Re-anchor the active ER onto a fresh L1 bank while preserving the
    /// ER-local account overlay. This is the per-L1-block path: stop the slot
    /// advancer, create a fresh ER bank whose parent is the new L1 bank, replay
    /// the thin in-memory ER overlay, publish the bank for RPC, then resume the
    /// advancer if the session is still active.
    pub fn reanchor_to_l1_parent(&mut self, parent_bank: Arc<Bank>) {
        self.sync_status.update_latest_l1_slot(parent_bank.slot());
        self.sync_status.mark_synced_through(parent_bank.slot());

        if !self.is_active() {
            return;
        }

        self.advancer_exit.store(true, Ordering::Relaxed);
        if let Some(advancer) = self.slot_advancer.take() {
            advancer.join();
        }

        let bank_operation_lock = self.bank_operation_lock.clone();
        let _bank_operation_guard = bank_operation_lock.lock().unwrap();

        // ER slot numbers are an ER-local clock. Do not derive the next
        // reanchor slot from L1 slot duration; just keep the ER clock
        // moving forward and far above L1 slots.
        const ER_SLOT_OFFSET: Slot = 1u64 << 40;
        let old_er_bank = self.bank_forks.read().unwrap().working_bank();
        let current_er_tip = old_er_bank.slot();
        let ephemeral_slot = if current_er_tip >= ER_SLOT_OFFSET {
            current_er_tip.saturating_add(1)
        } else {
            Self::er_slot_for(&parent_bank)
        };
        info!(
            "reanchor_to_l1_parent: parent_slot={}, ephemeral_slot={}, parent_epoch={}",
            parent_bank.slot(),
            ephemeral_slot,
            parent_bank.epoch(),
        );

        let recent_blockhash_max_age =
            crate::er_recent_blockhash_max_age_for_slot_duration(self.slot_duration);
        let mut bank = Bank::new_from_parent_ephemeral_isolated(
            parent_bank.clone(),
            SlotLeader::default(),
            ephemeral_slot,
        );
        bank.configure_er(&self.settings.er_fee_structure, recent_blockhash_max_age);
        let carried_blockhashes = bank.carry_forward_blockhashes_from(&old_er_bank);
        if carried_blockhashes > 0 {
            debug!("Carried {carried_blockhashes} ER recent blockhash(es) across reanchor");
        }
        bank.set_callback(Some(Box::new(NoopDropCallback)));
        Self::prepare_initial_working_bank(&bank);

        let new_bf_arc = BankForks::new_rw_arc_ephemeral(bank);
        let new_bf = Arc::try_unwrap(new_bf_arc)
            .unwrap_or_else(|_| panic!("just created, refcount must be 1"))
            .into_inner()
            .expect("lock not poisoned");
        let old_bf = std::mem::replace(&mut *self.bank_forks.write().unwrap(), new_bf);
        self.retire_bank_forks(old_bf);

        self.bank_forks
            .read()
            .unwrap()
            .root_bank()
            .set_fork_graph_in_program_cache(Arc::downgrade(&self.bank_forks));

        self.l1_anchor_bank = parent_bank;

        let er_bank = self.bank();
        let overlaid = self.apply_er_account_overlay_to_bank(&er_bank);
        if overlaid > 0 {
            info!("Rehydrated {overlaid} ER overlay account(s) after L1 reanchor");
        }
        let refreshed_owner_programs = self.hydrate_delegated_owner_programs_from_l1(
            &self.l1_anchor_bank,
            &er_bank,
            "reanchor hydration",
        );
        if refreshed_owner_programs > 0 {
            info!(
                "Rehydrated {refreshed_owner_programs} delegated owner program(s) after L1 \
                 reanchor"
            );
        }

        let initial_bank = Self::freeze_and_rotate_bank_for_rpc(
            &self.bank_forks,
            &self.block_commitment_cache,
            &self.optimistically_confirmed_bank,
            Some(&self.rpc_subscriptions),
            &self.er_history_store,
            &self.settings.er_fee_structure,
            recent_blockhash_max_age,
        );
        drop(_bank_operation_guard);

        let advancer_exit = Arc::new(AtomicBool::new(false));
        self.advancer_exit = advancer_exit.clone();
        self.slot_advancer = Some(crate::slot_advancer::SlotAdvancer::new_with_history(
            self.bank_forks.clone(),
            self.bank_operation_lock.clone(),
            self.block_commitment_cache.clone(),
            self.optimistically_confirmed_bank.clone(),
            initial_bank,
            crate::slot_advancer::Config {
                slot_duration: self.slot_duration,
                manager_account: Pubkey::default(),
                er_fee_structure: self.settings.er_fee_structure.clone(),
            },
            advancer_exit,
            Some(self.rpc_subscriptions.clone()),
            Some(self.er_history_store.clone()),
        ));
    }

    fn l1_account_for_state_diff(
        &self,
        pubkey: &Pubkey,
        delegated_accounts: &HashSet<Pubkey>,
    ) -> Option<AccountSharedData> {
        let account = self.l1_anchor_bank.get_account(pubkey)?;
        (delegated_accounts.contains(pubkey) && account.owner() == &self.portal_program_id)
            .then(|| {
                Self::effective_delegated_account(
                    &self.l1_anchor_bank,
                    &self.portal_program_id,
                    self.settings.grid_id,
                    pubkey,
                    &account,
                )
            })
            .flatten()
            .or(Some(account))
    }

    /// Compute the ER-vs-L1 account delta as a lattice hash.
    ///
    /// For delegated accounts, the L1 side is normalized back to the original
    /// owner recorded in the delegation PDA. This keeps a freshly delegated but
    /// otherwise untouched account out of the diff; only ER-local state changes
    /// affect the accumulator.
    pub fn state_diff_from_l1(&self) -> ErStateDiff {
        let delegated_accounts = self.delegated_accounts.read().unwrap().clone();
        let mut overlay_accounts = self
            .er_account_overlay
            .read()
            .unwrap()
            .iter()
            .map(|(pubkey, account)| (*pubkey, account.clone()))
            .collect::<Vec<_>>();
        overlay_accounts.sort_by_key(|(pubkey, _)| pubkey.to_bytes());

        let diff_accounts = overlay_accounts
            .into_iter()
            .filter_map(|(pubkey, er_account)| {
                let l1_account = self.l1_account_for_state_diff(&pubkey, &delegated_accounts);
                let l1_lt_hash = l1_account
                    .as_ref()
                    .map(|account| AccountsDb::lt_hash_account(account, &pubkey).0)
                    .unwrap_or_else(LtHash::identity);
                let er_lt_hash = AccountsDb::lt_hash_account(&er_account, &pubkey).0;
                (l1_lt_hash != er_lt_hash).then_some(ErStateDiffAccount {
                    pubkey,
                    l1_account,
                    er_account,
                    l1_lt_hash,
                    er_lt_hash,
                })
            })
            .collect::<Vec<_>>();

        let lt_hash = diff_accounts
            .iter()
            .map(|account_diff| {
                let mut delta = LtHash::identity();
                delta.mix_out(&account_diff.l1_lt_hash);
                delta.mix_in(&account_diff.er_lt_hash);
                delta
            })
            .reduce(|mut accum, delta| {
                accum.mix_in(&delta);
                accum
            })
            .unwrap_or_else(LtHash::identity);

        ErStateDiff {
            accounts: diff_accounts,
            lt_hash,
        }
    }

    /// Returns a clone of the delegated account pubkeys set.
    pub fn delegated_accounts(&self) -> HashSet<Pubkey> {
        self.delegated_accounts.read().unwrap().clone()
    }

    /// Build validator-settled DepositReceipt balance updates from ER-local
    /// system account balances and explicit ER withdrawal transactions.
    ///
    /// Withdraw on ER by sending a normal system transfer from the recipient to
    /// `withdrawal_sink(session, recipient)`. The sink balance is cumulative;
    /// Portal settlement pays only the delta over `DepositReceipt.withdrawn`.
    pub fn settlement_receipt_balances(
        &self,
        session_pda: Pubkey,
    ) -> Vec<ReceiptBalanceSettlement> {
        let delegated_accounts = self.delegated_accounts.read().unwrap().clone();
        let overlay = self.er_account_overlay.read().unwrap();
        let mut receipt_balances = overlay
            .iter()
            .filter(|(recipient, account)| {
                !delegated_accounts.contains(recipient)
                    && account.owner() == &solana_sdk_ids::system_program::id()
                    && account.data().is_empty()
            })
            .filter_map(|(recipient, account)| {
                let (receipt_pda, _) = northstar_portal::find_deposit_receipt_pda(
                    &self.portal_program_id.to_bytes(),
                    &session_pda.to_bytes(),
                    &recipient.to_bytes(),
                );
                let receipt_pda = Pubkey::new_from_array(receipt_pda);
                let receipt_account = self.l1_anchor_bank.get_account(&receipt_pda)?;
                let Some(crate::portal_state::PortalAccount::DepositReceipt(receipt)) =
                    crate::portal_state::try_parse_raw_portal_account(receipt_account.data())
                else {
                    return None;
                };
                let (withdrawal_sink, _) = northstar_portal::find_withdrawal_sink_pda(
                    &self.portal_program_id.to_bytes(),
                    &session_pda.to_bytes(),
                    &recipient.to_bytes(),
                );
                let withdrawal_sink = Pubkey::new_from_array(withdrawal_sink);
                let withdrawn = overlay
                    .get(&withdrawal_sink)
                    .map(|sink| {
                        let l1_sink_lamports = self
                            .l1_anchor_bank
                            .get_account(&withdrawal_sink)
                            .map(|account| account.lamports())
                            .unwrap_or_else(|| solana_rent::Rent::default().minimum_balance(0));
                        sink.lamports().saturating_sub(l1_sink_lamports)
                    })
                    .unwrap_or(receipt.withdrawn);
                (receipt.balance != account.lamports() || receipt.withdrawn != withdrawn).then_some(
                    ReceiptBalanceSettlement {
                        recipient: *recipient,
                        balance: account.lamports(),
                        withdrawn,
                    },
                )
            })
            .collect::<Vec<_>>();
        receipt_balances.sort_by_key(|receipt| receipt.recipient.to_bytes());
        receipt_balances
    }

    /// Returns the initial snapshot of a delegated account.
    pub fn initial_account_snapshot(&self, pubkey: &Pubkey) -> Option<&AccountSharedData> {
        self.initial_account_snapshots.get(pubkey)
    }

    fn is_reloadable_program_account(account: &AccountSharedData) -> bool {
        PROGRAM_OWNERS.contains(account.owner())
    }

    fn remove_reloadable_programs_from_cache<'a>(
        bank: &Bank,
        context: &str,
        programs: impl IntoIterator<Item = (Pubkey, &'a AccountSharedData)>,
    ) {
        let mut reloadable = Vec::new();
        let mut skipped = Vec::new();
        for (program_id, account) in programs {
            if Self::is_reloadable_program_account(account) {
                reloadable.push(program_id);
            } else {
                skipped.push((program_id, *account.owner()));
            }
        }

        if !reloadable.is_empty() {
            debug!(
                "ER ProgramCache invalidation ({context}): removing reloadable programs \
                 {reloadable:?}"
            );
            bank.remove_programs_from_cache(reloadable);
        }
        if !skipped.is_empty() {
            debug!(
                "ER ProgramCache invalidation ({context}): keeping non-reloadable/native programs \
                 {skipped:?}"
            );
        }
    }

    fn publish_bank_for_rpc(&self) {
        Self::freeze_and_rotate_bank_for_rpc(
            &self.bank_forks,
            &self.block_commitment_cache,
            &self.optimistically_confirmed_bank,
            Some(&self.rpc_subscriptions),
            &self.er_history_store,
            &self.settings.er_fee_structure,
            crate::er_recent_blockhash_max_age_for_slot_duration(self.slot_duration),
        );
    }

    fn hydrate_delegated_owner_programs_from_l1(
        &self,
        l1_bank: &Bank,
        er_bank: &Bank,
        cache_context: &str,
    ) -> usize {
        let delegated_accounts = self.delegated_accounts.read().unwrap();

        let updates = delegated_accounts
            .iter()
            .filter_map(|delegated_account| {
                let (record_pubkey, _) =
                    Self::delegation_record_pda(&self.portal_program_id, delegated_account);
                let record_account = l1_bank.get_account(&record_pubkey)?;
                match crate::portal_state::try_parse_raw_portal_account(record_account.data()) {
                    Some(crate::portal_state::PortalAccount::DelegationRecord(record))
                        if record.grid_id == self.settings.grid_id =>
                    {
                        Some(Pubkey::from(record.owner_program))
                    }
                    Some(crate::portal_state::PortalAccount::DelegationRecord(_)) => None,
                    _ => {
                        warn!("Delegation record {record_pubkey} has invalid account data");
                        None
                    }
                }
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|owner_program| {
                let Some(program_account) = l1_bank.get_account(&owner_program) else {
                    warn!(
                        "Cannot refresh owner program {owner_program}: program account not found \
                         on L1"
                    );
                    return None;
                };

                let programdata = match (
                    program_account.owner() == &bpf_loader_upgradeable::id(),
                    program_account.state(),
                ) {
                    (
                        true,
                        Ok(UpgradeableLoaderState::Program {
                            programdata_address,
                        }),
                    ) => match l1_bank.get_account(&programdata_address) {
                        Some(programdata_account) => {
                            Some((programdata_address, programdata_account))
                        }
                        None => {
                            warn!(
                                "Cannot refresh owner program {owner_program}: programdata \
                                 account {programdata_address} not found on L1"
                            );
                            return None;
                        }
                    },
                    (true, Err(err)) => {
                        warn!(
                            "Cannot refresh owner program {owner_program}: failed to parse \
                             upgradeable-loader state: {err:?}"
                        );
                        None
                    }
                    _ => None,
                };

                let program_changed =
                    er_bank.get_account(&owner_program).as_ref() != Some(&program_account);
                let programdata_changed = programdata.as_ref().is_some_and(
                    |(programdata_address, programdata_account)| {
                        er_bank.get_account(programdata_address).as_ref()
                            != Some(programdata_account)
                    },
                );

                (program_changed || programdata_changed).then_some((
                    owner_program,
                    program_account,
                    programdata,
                ))
            })
            .collect::<Vec<_>>();

        let updated_accounts = updates
            .iter()
            .flat_map(|(owner_program, program_account, programdata)| {
                std::iter::once((owner_program, program_account)).chain(programdata.as_ref().map(
                    |(programdata_address, programdata_account)| {
                        (programdata_address, programdata_account)
                    },
                ))
            })
            .collect::<Vec<_>>();
        er_bank.store_accounts((er_bank.slot(), updated_accounts.as_slice()));
        Self::remove_reloadable_programs_from_cache(
            er_bank,
            cache_context,
            updates
                .iter()
                .map(|(owner_program, program_account, _)| (*owner_program, program_account)),
        );

        updates.len()
    }

    pub(crate) fn refresh_delegated_owner_programs_from_l1(&self, l1_bank: &Bank) {
        let _bank_operation_guard = self.bank_operation_lock.lock().unwrap();
        let er_bank = self.bank();
        let refreshed =
            self.hydrate_delegated_owner_programs_from_l1(l1_bank, &er_bank, "owner refresh");
        if refreshed > 0 {
            self.publish_bank_for_rpc();
            info!("Refreshed {refreshed} delegated owner program(s) from L1");
        }
    }

    /// Handle a new account delegation from L1.
    /// Copies the account data from L1 into the ER bank and adds it to the
    /// delegated accounts set so transactions can write to it.
    pub fn handle_delegation(&self, delegated_account: &Pubkey, account_data: AccountSharedData) {
        self.handle_delegation_inner(delegated_account, account_data, None, None);
    }

    pub fn handle_delegation_with_owner_program(
        &self,
        delegated_account: &Pubkey,
        account_data: AccountSharedData,
        owner_program: Option<Pubkey>,
    ) {
        self.handle_delegation_inner(delegated_account, account_data, owner_program, None);
    }

    // TODO: handle multiple delegations at once to speed things up?
    pub(crate) fn handle_delegation_inner(
        &self,
        delegated_account: &Pubkey,
        account_data: AccountSharedData,
        owner_program: Option<Pubkey>,
        l1_bank: Option<&Bank>,
    ) {
        let _bank_operation_guard = self.bank_operation_lock.lock().unwrap();
        let bank = self.bank();
        let mut er_account = account_data.clone();
        if let Some(owner_program) = owner_program {
            er_account.set_owner(owner_program);
            if let Some(l1_bank) = l1_bank {
                if let Some(program_account) = l1_bank.get_account(&owner_program) {
                    bank.store_account(&owner_program, &program_account);
                    if program_account.owner() == &bpf_loader_upgradeable::id() {
                        match program_account.state() {
                            Ok(UpgradeableLoaderState::Program {
                                programdata_address,
                            }) => {
                                if let Some(programdata_account) =
                                    l1_bank.get_account(&programdata_address)
                                {
                                    bank.store_account(&programdata_address, &programdata_account);
                                } else {
                                    warn!(
                                        "Cannot hydrate owner program {owner_program}: \
                                         programdata account {programdata_address} not found on L1"
                                    );
                                }
                            }
                            Ok(_) => warn!(
                                "Cannot hydrate owner program {owner_program}: upgradeable-loader \
                                 account is not Program state"
                            ),
                            Err(err) => warn!(
                                "Cannot hydrate owner program {owner_program}: failed to parse \
                                 upgradeable-loader state: {err:?}"
                            ),
                        }
                    }
                    Self::remove_reloadable_programs_from_cache(
                        &bank,
                        "delegation hydration",
                        Some((owner_program, &program_account)),
                    );
                } else {
                    warn!(
                        "Cannot hydrate owner program {owner_program}: program account not found \
                         on L1"
                    );
                }
            }
        }
        bank.store_account(delegated_account, &er_account);
        self.er_account_overlay
            .write()
            .unwrap()
            .insert(*delegated_account, er_account.clone());

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
            er_account.owner(),
            er_account.lamports()
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
        let withdrawal_sink = self.session_pda.read().unwrap().map(|session_pda| {
            crate::withdrawal_sink_pda(&self.portal_program_id, &session_pda, depositor)
        });
        {
            let mut overlay = self.er_account_overlay.write().unwrap();
            overlay.insert(*depositor, account.clone());

            if let Some(withdrawal_sink) = withdrawal_sink {
                if bank.get_account(&withdrawal_sink).is_none() {
                    let sink_account = AccountSharedData::new(
                        solana_rent::Rent::default().minimum_balance(0),
                        0,
                        &solana_sdk_ids::system_program::id(),
                    );
                    bank.store_account(&withdrawal_sink, &sink_account);
                    overlay.insert(withdrawal_sink, sink_account);
                }
            }
        }
        {
            // Mark as touched so the balance isn't zeroed later.
            let mut touched = self.touched_accounts.write().unwrap();
            if let Some(withdrawal_sink) = withdrawal_sink {
                touched.insert(withdrawal_sink);
            }
            touched.insert(*depositor);
        }

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
        northstar_portal::{DelegationRecord, DepositReceipt},
        solana_account::{
            state_traits::StateMut, AccountSharedData, ReadableAccount, WritableAccount,
        },
        solana_compute_budget_interface::ComputeBudgetInstruction,
        solana_fee_structure::FeeStructure,
        solana_gossip::contact_info::ContactInfo,
        solana_keypair::{Keypair, Signer},
        solana_lattice_hash::lt_hash::LtHash,
        solana_message::{Message, SanitizedMessage},
        solana_net_utils::SocketAddrSpace,
        solana_rpc_client::rpc_client::RpcClient,
        solana_rpc_client_types::config::{CommitmentConfig, RpcSendTransactionConfig},
        solana_sdk_ids::{bpf_loader_upgradeable, system_program},
        solana_send_transaction_service::{
            send_transaction_service_stats::SendTransactionServiceStats,
            transaction_client::TransactionClient,
        },
        solana_svm::transaction_processor::ExecutionRecordingConfig,
        solana_system_interface::instruction::transfer,
        solana_transaction::{versioned::VersionedTransaction, Transaction},
        solana_transaction_status::{TransactionConfirmationStatus, UiTransactionEncoding},
        std::{collections::HashSet, net::TcpListener, sync::atomic::AtomicU64, time::Duration},
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

    fn store_withdrawal_sink(
        bank: &Bank,
        portal_program_id: &Pubkey,
        session_pda: &Pubkey,
        recipient: &Pubkey,
    ) {
        let sink = crate::withdrawal_sink_pda(portal_program_id, session_pda, recipient);
        let account = AccountSharedData::new(
            solana_rent::Rent::default().minimum_balance(0),
            0,
            &system_program::id(),
        );
        bank.store_account(&sink, &account);
    }

    fn store_deposit_receipt(
        bank: &Bank,
        portal_program_id: &Pubkey,
        session_pda: &Pubkey,
        recipient: &Pubkey,
        balance: u64,
        withdrawn: u64,
    ) {
        let (receipt_pda, bump) = Pubkey::find_program_address(
            &[b"deposit_receipt", session_pda.as_ref(), recipient.as_ref()],
            portal_program_id,
        );
        let receipt = DepositReceipt {
            discriminator: DepositReceipt::DISCRIMINATOR,
            session: session_pda.to_bytes(),
            recipient: recipient.to_bytes(),
            balance,
            withdrawn,
            bump,
        };
        let mut account = AccountSharedData::new(
            solana_rent::Rent::default()
                .minimum_balance(DepositReceipt::LEN)
                .saturating_add(balance),
            DepositReceipt::LEN,
            portal_program_id,
        );
        account
            .data_as_mut_slice()
            .copy_from_slice(&borsh::to_vec(&receipt).unwrap());
        bank.store_account(&receipt_pda, &account);
    }

    fn store_delegation_record(
        bank: &Bank,
        portal_program_id: &Pubkey,
        delegated_account: &Pubkey,
        owner_program: &Pubkey,
        grid_id: u64,
    ) {
        let (record_pubkey, bump) =
            EphemeralRuntime::delegation_record_pda(portal_program_id, delegated_account);
        let record = DelegationRecord {
            discriminator: DelegationRecord::DISCRIMINATOR,
            owner_program: owner_program.to_bytes(),
            grid_id,
            bump,
        };
        let data = borsh::to_vec(&record).unwrap();
        let mut account = AccountSharedData::new(1_000_000, data.len(), portal_program_id);
        account.data_as_mut_slice().copy_from_slice(&data);
        bank.store_account(&record_pubkey, &account);
    }

    fn store_upgradeable_owner_program(
        bank: &Bank,
        owner_program: &Pubkey,
        programdata_address: &Pubkey,
        deployment_slot: Slot,
        program_bytes: &[u8],
    ) {
        let mut program_account = AccountSharedData::new(
            1_000_000,
            UpgradeableLoaderState::size_of_program(),
            &bpf_loader_upgradeable::id(),
        );
        program_account
            .set_state(&UpgradeableLoaderState::Program {
                programdata_address: *programdata_address,
            })
            .unwrap();
        program_account.set_executable(true);
        bank.store_account(owner_program, &program_account);

        let mut programdata_account = AccountSharedData::new(
            1_000_000,
            UpgradeableLoaderState::size_of_programdata(program_bytes.len()),
            &bpf_loader_upgradeable::id(),
        );
        programdata_account
            .set_state(&UpgradeableLoaderState::ProgramData {
                slot: deployment_slot,
                upgrade_authority_address: None,
            })
            .unwrap();
        programdata_account.data_as_mut_slice()
            [UpgradeableLoaderState::size_of_programdata_metadata()..]
            .copy_from_slice(program_bytes);
        bank.store_account(programdata_address, &programdata_account);
    }

    fn er_fee_structure(lamports_per_signature: u64) -> FeeStructure {
        FeeStructure {
            lamports_per_signature,
            lamports_per_write_lock: 0,
            compute_fee_bins: vec![],
        }
    }

    fn sanitized_transfer_message(bank: &Bank, fee_payer: &Pubkey) -> SanitizedMessage {
        let receiver = Pubkey::new_unique();
        let instruction = transfer(fee_payer, &receiver, 1);
        let message =
            Message::new_with_blockhash(&[instruction], Some(fee_payer), &bank.last_blockhash());
        SanitizedMessage::try_from_legacy_message(message, &HashSet::new()).unwrap()
    }

    fn sanitized_priority_fee_message(bank: &Bank, fee_payer: &Pubkey) -> SanitizedMessage {
        let message = Message::new_with_blockhash(
            &[ComputeBudgetInstruction::set_compute_unit_price(1_000_000)],
            Some(fee_payer),
            &bank.last_blockhash(),
        );
        SanitizedMessage::try_from_legacy_message(message, &HashSet::new()).unwrap()
    }

    fn create_runtime() -> (Arc<Bank>, EphemeralRuntime) {
        let parent_bank = Arc::new(create_test_bank());
        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: er_fee_structure(0),
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

    fn create_runtime_with_delegated_account(
        lamports: u64,
    ) -> (Pubkey, AccountSharedData, EphemeralRuntime) {
        let parent_bank = Arc::new(create_test_bank());
        let cluster_info = create_test_cluster_info();
        let portal_program_id = Pubkey::new_unique();
        let delegated_account = Pubkey::new_unique();
        let owner_program = system_program::id();

        let mut l1_account = AccountSharedData::new(lamports, 4, &portal_program_id);
        l1_account
            .data_as_mut_slice()
            .copy_from_slice(&[1, 2, 3, 4]);
        parent_bank.store_account(&delegated_account, &l1_account);
        store_delegation_record(
            &parent_bank,
            &portal_program_id,
            &delegated_account,
            &owner_program,
            0,
        );

        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: er_fee_structure(0),
            delegated_accounts: vec![delegated_account],
        };
        let runtime = EphemeralRuntime::new(
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

        (delegated_account, l1_account, runtime)
    }

    #[test]
    fn test_state_diff_from_l1_ignores_unchanged_delegated_account() {
        let (delegated_account, _l1_account, mut runtime) =
            create_runtime_with_delegated_account(10);

        let diff = runtime.state_diff_from_l1();

        assert!(runtime.delegated_accounts().contains(&delegated_account));
        assert!(diff.accounts.is_empty());
        assert_eq!(diff.lt_hash, LtHash::identity());

        runtime.shutdown();
    }

    #[test]
    fn test_state_diff_from_l1_hashes_delegated_account_delta() {
        let (delegated_account, _l1_account, mut runtime) =
            create_runtime_with_delegated_account(10);

        runtime.credit_deposit(&delegated_account, 5);
        let diff = runtime.state_diff_from_l1();

        assert_eq!(diff.accounts.len(), 1);
        let account_diff = &diff.accounts[0];
        assert_eq!(account_diff.pubkey, delegated_account);
        assert_eq!(account_diff.l1_account.as_ref().unwrap().lamports(), 10);
        assert_eq!(
            account_diff.l1_account.as_ref().unwrap().owner(),
            &system_program::id()
        );
        assert_eq!(account_diff.er_account.lamports(), 15);

        let mut expected_lt_hash = LtHash::identity();
        expected_lt_hash.mix_out(&account_diff.l1_lt_hash);
        expected_lt_hash.mix_in(&account_diff.er_lt_hash);
        assert_eq!(diff.lt_hash, expected_lt_hash);
        assert_ne!(diff.checksum(), LtHash::identity().checksum());

        runtime.shutdown();
    }

    #[test]
    fn test_state_diff_from_l1_hashes_new_er_account() {
        let (_, mut runtime) = create_runtime();
        let depositor = Pubkey::new_unique();

        runtime.credit_deposit(&depositor, 7);
        let diff = runtime.state_diff_from_l1();

        assert_eq!(diff.accounts.len(), 1);
        let account_diff = &diff.accounts[0];
        assert_eq!(account_diff.pubkey, depositor);
        assert!(account_diff.l1_account.is_none());
        assert_eq!(account_diff.l1_lt_hash, LtHash::identity());
        assert_eq!(account_diff.er_account.lamports(), 7);

        runtime.shutdown();
    }

    #[test]
    fn test_er_withdrawal_transaction_settles_via_withdrawal_sink() {
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let session_pda = Pubkey::new_unique();
        let recipient_keypair = Keypair::new();
        let recipient = recipient_keypair.pubkey();
        let deposit_amount = 1_000u64;
        let withdraw_amount = 250u64;
        store_deposit_receipt(
            &parent_bank,
            &portal_program_id,
            &session_pda,
            &recipient,
            deposit_amount,
            0,
        );
        store_withdrawal_sink(&parent_bank, &portal_program_id, &session_pda, &recipient);
        parent_bank.freeze();

        let settings = EphemeralRollupSettings {
            session_pda,
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            delegated_accounts: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            Arc::new(parent_bank),
            create_test_cluster_info(),
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            portal_program_id,
            Arc::new(Keypair::new()),
        )
        .unwrap();
        runtime.activate();
        runtime.credit_deposit(&recipient, deposit_amount);

        let withdrawal_ix = crate::er_withdrawal_instruction(
            &portal_program_id,
            &session_pda,
            &recipient,
            withdraw_amount,
        );
        let tx = Transaction::new_signed_with_payer(
            &[withdrawal_ix],
            Some(&recipient),
            &[&recipient_keypair],
            runtime.bank().last_blockhash(),
        );
        let wire_tx = bincode::serialize(&VersionedTransaction::from(tx)).unwrap();
        TransactionClient::send_transactions_in_batch(
            &runtime._tx_client,
            vec![wire_tx],
            &SendTransactionServiceStats::default(),
        );

        let withdrawal_sink =
            crate::withdrawal_sink_pda(&portal_program_id, &session_pda, &recipient);
        assert_eq!(
            runtime.bank().get_balance(&recipient),
            deposit_amount - withdraw_amount
        );
        assert_eq!(
            runtime.bank().get_balance(&withdrawal_sink)
                - solana_rent::Rent::default().minimum_balance(0),
            withdraw_amount
        );

        let receipt_balances = runtime.settlement_receipt_balances(session_pda);
        assert_eq!(receipt_balances.len(), 1);
        assert_eq!(receipt_balances[0].recipient, recipient);
        assert_eq!(
            receipt_balances[0].balance,
            deposit_amount - withdraw_amount
        );
        assert_eq!(receipt_balances[0].withdrawn, withdraw_amount);

        runtime.shutdown();
    }

    #[test]
    fn test_zero_er_fee_structure_returns_zero_fee() {
        let (_, mut runtime) = create_runtime();
        let bank = runtime.bank();
        let fee_payer = Pubkey::new_unique();
        let message = sanitized_transfer_message(&bank, &fee_payer);

        assert_eq!(bank.last_blockhash_and_lamports_per_signature().1, 0);
        assert_eq!(bank.get_fee_for_message(&message), Some(0));

        runtime.shutdown();
    }

    #[test]
    fn test_reapplying_same_er_fee_structure_does_not_age_blockhashes() {
        let mut bank = create_test_bank();
        bank.register_unique_recent_blockhash_for_test();
        let old_valid_hash = bank.last_blockhash();
        for _ in 0..40 {
            bank.register_unique_recent_blockhash_for_test();
        }
        assert!(bank.is_hash_valid_for_age(&old_valid_hash, solana_clock::MAX_PROCESSING_AGE));

        for _ in 0..120 {
            bank.configure_er(
                &er_fee_structure(0),
                crate::er_recent_blockhash_max_age_for_slot_duration(
                    crate::DEFAULT_ER_SLOT_DURATION,
                ),
            );
        }

        assert!(bank.is_hash_valid_for_age(&old_valid_hash, solana_clock::MAX_PROCESSING_AGE));
    }

    #[test]
    fn test_zero_er_fee_structure_ignores_priority_fee() {
        let (_, mut runtime) = create_runtime();
        let bank = runtime.bank();
        let fee_payer = Pubkey::new_unique();
        let message = sanitized_priority_fee_message(&bank, &fee_payer);

        assert_eq!(bank.last_blockhash_and_lamports_per_signature().1, 0);
        assert_eq!(bank.get_fee_for_message(&message), Some(0));

        runtime.shutdown();
    }

    fn create_runtime_with_fee(lamports_per_signature: u64) -> EphemeralRuntime {
        let parent_bank = Arc::new(create_test_bank());
        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: er_fee_structure(lamports_per_signature),
            delegated_accounts: vec![],
        };
        EphemeralRuntime::new(
            parent_bank,
            cluster_info,
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
        )
        .unwrap()
    }

    #[test]
    fn test_nonzero_er_fee_structure_is_preserved() {
        let lamports_per_signature = 5_000;
        let mut runtime = create_runtime_with_fee(lamports_per_signature);
        let bank = runtime.bank();
        let fee_payer = Pubkey::new_unique();
        let message = sanitized_transfer_message(&bank, &fee_payer);

        assert_eq!(
            bank.last_blockhash_and_lamports_per_signature().1,
            lamports_per_signature
        );
        assert_eq!(
            bank.get_fee_for_message(&message),
            Some(lamports_per_signature)
        );

        runtime.shutdown();
    }

    #[test]
    fn test_nonzero_er_fee_structure_survives_slot_advancement() {
        let lamports_per_signature = 5_000;
        let mut runtime = create_runtime_with_fee(lamports_per_signature);
        runtime.activate();
        std::thread::sleep(Duration::from_millis(900));
        let bank = runtime.bank();
        let fee_payer = Pubkey::new_unique();
        let message = sanitized_transfer_message(&bank, &fee_payer);

        assert_eq!(
            bank.last_blockhash_and_lamports_per_signature().1,
            lamports_per_signature
        );
        assert_eq!(
            bank.get_fee_for_message(&message),
            Some(lamports_per_signature)
        );

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
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
    fn test_send_transaction_result_is_immediately_visible_at_processed_commitment() {
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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        rpc_client
            .send_transaction_with_config(
                &tx,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let receiver_balance = rpc_client
            .get_balance_with_commitment(&receiver_pubkey, CommitmentConfig::processed())
            .unwrap()
            .value;
        assert_eq!(
            receiver_balance, transfer_amount,
            "processed RPC should observe transaction writes after sendTransaction returns"
        );

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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        let new_parent = Bank::new_from_parent(parent_bank, SlotLeader::default(), 1);
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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        // Send a transaction — inactive ER RPC must reject it.
        let instruction = transfer(&sender_pubkey, &receiver_pubkey, transfer_amount);
        let tx = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&sender_pubkey),
            &[&sender_keypair],
            blockhash,
        );

        // sendTransaction RPC must survive preflight even while runtime is inactive,
        // but it must reject the user instead of accepting and silently dropping.
        let err = rpc_client
            .send_transaction_with_config(&tx, RpcSendTransactionConfig::default())
            .unwrap_err();
        assert!(
            err.to_string().contains("Ephemeral rollup is not active"),
            "unexpected inactive sendTransaction error: {err}"
        );

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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![],
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
        let new_parent = Arc::new(Bank::new_from_parent(parent_bank, SlotLeader::default(), 1));
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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![],
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        let new_parent = Arc::new(Bank::new_from_parent(
            parent_bank.clone(),
            SlotLeader::default(),
            1,
        ));
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
    fn test_reanchor_picks_up_fresh_l1_readonly_state() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let readonly_account = Pubkey::new_unique();
        fund_account(&parent_bank, &readonly_account, 1_000_000);
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        let new_parent = Bank::new_from_parent(parent_bank, SlotLeader::default(), 1);
        fund_account(&new_parent, &readonly_account, 2_000_000);
        new_parent.freeze();
        runtime.reanchor_to_l1_parent(Arc::new(new_parent));

        assert_eq!(runtime.bank().get_balance(&readonly_account), 2_000_000);
        runtime.shutdown();
    }

    #[test]
    fn test_reanchor_preserves_er_recent_blockhashes() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            delegated_accounts: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            parent_bank.clone(),
            create_test_cluster_info(),
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
        )
        .unwrap();
        // Exercise the active reanchor path without starting SlotAdvancer; this keeps
        // the regression deterministic and avoids waiting for an advancer sleep.
        runtime.active.store(true, Ordering::Relaxed);

        let er_bank_before = runtime.bank();
        er_bank_before.register_unique_recent_blockhash_for_test();
        let er_blockhash = er_bank_before.last_blockhash();
        assert!(
            er_bank_before.is_hash_valid_for_age(&er_blockhash, solana_clock::MAX_PROCESSING_AGE)
        );

        let new_parent = Bank::new_from_parent(parent_bank, SlotLeader::default(), 1);
        new_parent.freeze();
        runtime.reanchor_to_l1_parent(Arc::new(new_parent));

        assert!(
            runtime
                .bank()
                .is_hash_valid_for_age(&er_blockhash, solana_clock::MAX_PROCESSING_AGE),
            "ER blockhash minted before L1 reanchor must remain usable after reanchor"
        );
        runtime.shutdown();
    }

    #[test]
    fn test_reset_to_new_parent_preserves_er_recent_blockhashes() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            delegated_accounts: vec![],
        };
        let mut runtime = EphemeralRuntime::new(
            parent_bank.clone(),
            create_test_cluster_info(),
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
        )
        .unwrap();

        let er_bank_before = runtime.bank();
        er_bank_before.register_unique_recent_blockhash_for_test();
        let er_blockhash = er_bank_before.last_blockhash();
        assert!(
            er_bank_before.is_hash_valid_for_age(&er_blockhash, solana_clock::MAX_PROCESSING_AGE)
        );

        let new_parent = Bank::new_from_parent(parent_bank, SlotLeader::default(), 1);
        new_parent.freeze();
        runtime.reset_to_new_parent(Arc::new(new_parent));

        assert!(
            runtime
                .bank()
                .is_hash_valid_for_age(&er_blockhash, solana_clock::MAX_PROCESSING_AGE),
            "ER blockhash minted before L1 reset must remain usable after reset"
        );
        runtime.shutdown();
    }

    #[test]
    fn test_reanchor_preserves_er_touched_overlay() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let depositor = Pubkey::new_unique();
        fund_account(&parent_bank, &depositor, 1_000_000);
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
        runtime.credit_deposit(&depositor, 50_000);
        assert_eq!(runtime.bank().get_balance(&depositor), 50_000);

        let new_parent = Bank::new_from_parent(parent_bank, SlotLeader::default(), 1);
        fund_account(&new_parent, &depositor, 9_000_000);
        new_parent.freeze();
        runtime.reanchor_to_l1_parent(Arc::new(new_parent));

        assert_eq!(
            runtime.bank().get_balance(&depositor),
            50_000,
            "ER overlay must win over later L1 base state for touched accounts"
        );
        runtime.shutdown();
    }

    #[test]
    fn test_reanchor_preserves_delegated_overlay_owner_remap() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let delegated_account = Pubkey::new_unique();
        let owner_program = Pubkey::new_unique();
        let grid_id = 42;
        let portal_owned_account = AccountSharedData::new(1_000_000, 8, &portal_program_id);
        parent_bank.store_account(&delegated_account, &portal_owned_account);
        store_delegation_record(
            &parent_bank,
            &portal_program_id,
            &delegated_account,
            &owner_program,
            grid_id,
        );
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            delegated_accounts: vec![delegated_account],
        };
        let mut runtime = EphemeralRuntime::new(
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
        runtime.activate();
        assert_eq!(
            runtime
                .bank()
                .get_account(&delegated_account)
                .unwrap()
                .owner(),
            &owner_program
        );

        let new_parent = Bank::new_from_parent(parent_bank, SlotLeader::default(), 1);
        new_parent.freeze();
        runtime.reanchor_to_l1_parent(Arc::new(new_parent));

        assert_eq!(
            runtime
                .bank()
                .get_account(&delegated_account)
                .unwrap()
                .owner(),
            &owner_program,
            "delegated account should keep ER owner remap; L1 base remains portal-owned"
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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
        let new_parent = Bank::new_from_parent(parent_bank, SlotLeader::default(), 1);
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
    fn test_reset_rebinds_program_cache_fork_graph() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        let new_parent = Bank::new_from_parent(parent_bank, SlotLeader::default(), 1);
        let sender_keypair = Keypair::new();
        let sender_pubkey = sender_keypair.pubkey();
        let receiver_pubkey = Pubkey::new_unique();
        fund_account(&new_parent, &sender_pubkey, 10_000_000_000);
        new_parent.freeze();

        // Mirrors real session activation path:
        // NorthStarService::activate_session -> EphemeralRuntime::reset_to_new_parent.
        runtime.reset_to_new_parent(Arc::new(new_parent));

        // Stop slot advancer so the assertion below is single-threaded.
        runtime
            .advancer_exit
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(advancer) = runtime.slot_advancer.take() {
            advancer.join();
        }
        runtime
            .active
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let blockhash = runtime.bank().last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[transfer(&sender_pubkey, &receiver_pubkey, 1_000_000)],
            Some(&sender_pubkey),
            &[&sender_keypair],
            blockhash,
        );
        let wire_tx = bincode::serialize(
            &solana_transaction::versioned::VersionedTransaction::from(tx),
        )
        .unwrap();

        solana_send_transaction_service::transaction_client::TransactionClient::send_transactions_in_batch(
            &runtime._tx_client,
            vec![wire_tx],
            &solana_send_transaction_service::send_transaction_service_stats::SendTransactionServiceStats::default(),
        );

        assert_eq!(runtime.bank().get_balance(&receiver_pubkey), 1_000_000);

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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            delegated_accounts: vec![],
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
        let new_parent = Arc::new(Bank::new_from_parent(parent_bank, SlotLeader::default(), 1));
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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
    fn test_handle_delegation_with_owner_program_remaps_er_owner() {
        agave_logger::setup();

        let parent_bank = Arc::new(create_test_bank());
        let portal_program_id = Pubkey::new_unique();
        let owner_program = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();

        let mut delegated_l1_snapshot = AccountSharedData::new(1_000_000, 4, &portal_program_id);
        delegated_l1_snapshot
            .data_as_mut_slice()
            .copy_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
        delegated_l1_snapshot.set_executable(true);
        parent_bank.store_account(&delegated_pubkey, &delegated_l1_snapshot);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        runtime.handle_delegation_with_owner_program(
            &delegated_pubkey,
            delegated_l1_snapshot.clone(),
            Some(owner_program),
        );

        let er_account = runtime
            .bank()
            .get_account(&delegated_pubkey)
            .expect("delegated account should be visible in ER");
        assert_eq!(er_account.owner(), &owner_program);
        assert_eq!(er_account.data(), delegated_l1_snapshot.data());
        assert_eq!(er_account.executable(), delegated_l1_snapshot.executable());
        assert!(runtime.delegated_accounts().contains(&delegated_pubkey));

        runtime.shutdown();
    }

    #[test]
    fn test_handle_delegation_from_l1_hydrates_upgradeable_owner_program() {
        agave_logger::setup();

        let (parent_bank, mut runtime) = create_runtime();
        runtime.activate();

        let l1_bank = Bank::new_from_parent(
            parent_bank.clone(),
            SlotLeader::default(),
            parent_bank.slot().saturating_add(1),
        );
        let portal_program_id = runtime.portal_program_id;
        let owner_program = Pubkey::new_unique();
        let programdata_address = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();
        let program_bytes = [0xAA, 0xBB, 0xCC, 0xDD];

        let mut delegated_l1_snapshot = AccountSharedData::new(1_000_000, 4, &portal_program_id);
        delegated_l1_snapshot
            .data_as_mut_slice()
            .copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        l1_bank.store_account(&delegated_pubkey, &delegated_l1_snapshot);
        store_upgradeable_owner_program(
            &l1_bank,
            &owner_program,
            &programdata_address,
            l1_bank.slot(),
            &program_bytes,
        );

        let program_account = l1_bank.get_account(&owner_program).unwrap();
        let programdata_account = l1_bank.get_account(&programdata_address).unwrap();

        assert!(runtime.bank().get_account(&owner_program).is_none());
        assert!(runtime.bank().get_account(&programdata_address).is_none());

        runtime.handle_delegation_inner(
            &delegated_pubkey,
            delegated_l1_snapshot.clone(),
            Some(owner_program),
            Some(&l1_bank),
        );

        let er_program_account = runtime
            .bank()
            .get_account(&owner_program)
            .expect("owner program should be hydrated into ER");
        assert_eq!(er_program_account.owner(), &bpf_loader_upgradeable::id());
        assert_eq!(er_program_account.data(), program_account.data());

        let er_programdata_account = runtime
            .bank()
            .get_account(&programdata_address)
            .expect("programdata should be hydrated into ER");
        assert_eq!(er_programdata_account.data(), programdata_account.data());

        let er_delegated_account = runtime
            .bank()
            .get_account(&delegated_pubkey)
            .expect("delegated account should be visible in ER");
        assert_eq!(er_delegated_account.owner(), &owner_program);

        runtime.shutdown();
    }

    #[test]
    fn test_reset_to_new_parent_rehydrates_existing_delegations_from_l1() {
        agave_logger::setup();

        let (parent_bank, mut runtime) = create_runtime();
        runtime.activate();

        let l1_bank = Bank::new_from_parent(
            parent_bank.clone(),
            SlotLeader::default(),
            parent_bank.slot().saturating_add(1),
        );
        let portal_program_id = runtime.portal_program_id;
        let owner_program = Pubkey::new_unique();
        let programdata_address = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();
        let mut delegated_l1_snapshot = AccountSharedData::new(1_000_000, 4, &portal_program_id);
        delegated_l1_snapshot
            .data_as_mut_slice()
            .copy_from_slice(&[0x10, 0x20, 0x30, 0x40]);
        l1_bank.store_account(&delegated_pubkey, &delegated_l1_snapshot);
        store_delegation_record(
            &l1_bank,
            &portal_program_id,
            &delegated_pubkey,
            &owner_program,
            0,
        );
        store_upgradeable_owner_program(
            &l1_bank,
            &owner_program,
            &programdata_address,
            l1_bank.slot(),
            &[0xCA, 0xFE, 0xBA, 0xBE],
        );

        runtime.reset_to_new_parent(Arc::new(l1_bank));

        assert!(
            runtime.delegated_accounts().contains(&delegated_pubkey),
            "reset should hydrate delegated set from existing L1 DelegationRecord accounts"
        );
        let er_delegated_account = runtime
            .bank()
            .get_account(&delegated_pubkey)
            .expect("delegated account should be restored into ER after reset");
        assert_eq!(er_delegated_account.owner(), &owner_program);
        assert_eq!(er_delegated_account.data(), delegated_l1_snapshot.data());
        assert!(runtime
            .initial_account_snapshot(&delegated_pubkey)
            .is_some());
        assert!(runtime.bank().get_account(&programdata_address).is_some());

        runtime.shutdown();
    }

    #[test]
    fn test_reanchor_to_l1_parent_keeps_delegated_owner_program_executable() {
        agave_logger::setup();

        let (parent_bank, mut runtime) = create_runtime();
        runtime.activate();

        let program_bytes = std::fs::read("../programs/bpf_loader/test_elfs/out/noop_aligned.so")
            .expect("noop ELF should exist");
        let l1_bank = Arc::new(Bank::new_from_parent(
            parent_bank.clone(),
            SlotLeader::default(),
            parent_bank.slot().saturating_add(1),
        ));
        let portal_program_id = runtime.portal_program_id;
        let owner_program = Pubkey::new_unique();
        let programdata_address = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();
        let delegated_l1_snapshot = AccountSharedData::new(1_000_000, 4, &portal_program_id);
        l1_bank.store_account(&delegated_pubkey, &delegated_l1_snapshot);
        store_delegation_record(
            &l1_bank,
            &portal_program_id,
            &delegated_pubkey,
            &owner_program,
            0,
        );
        store_upgradeable_owner_program(
            &l1_bank,
            &owner_program,
            &programdata_address,
            l1_bank.slot(),
            &program_bytes,
        );
        runtime.handle_delegation_inner(
            &delegated_pubkey,
            delegated_l1_snapshot,
            Some(owner_program),
            Some(&l1_bank),
        );

        let next_l1_bank = Arc::new(Bank::new_from_parent(
            l1_bank.clone(),
            SlotLeader::default(),
            l1_bank.slot().saturating_add(1),
        ));
        runtime.reanchor_to_l1_parent(next_l1_bank);

        let fee_payer = Keypair::new();
        fund_account(&runtime.bank(), &fee_payer.pubkey(), 1_000_000_000);
        let instruction =
            solana_instruction::Instruction::new_with_bytes(owner_program, &[], vec![]);
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&fee_payer.pubkey()),
            &[&fee_payer],
            runtime.bank().last_blockhash(),
        );
        runtime
            .bank()
            .process_transaction(&transaction)
            .expect("delegated owner program should stay executable after reanchor");

        runtime.shutdown();
    }

    #[test]
    fn test_system_owned_delegation_keeps_system_builtin_executable() {
        let (parent_bank, mut runtime) = create_runtime();
        runtime.activate();

        let delegated_signer = Keypair::new();
        let delegated_l1_account =
            AccountSharedData::new(1_000_000_000, 0, &runtime.portal_program_id);
        runtime.handle_delegation_inner(
            &delegated_signer.pubkey(),
            delegated_l1_account,
            Some(system_program::id()),
            Some(&parent_bank),
        );

        let recipient = Pubkey::new_unique();
        let transaction = Transaction::new_signed_with_payer(
            &[transfer(&delegated_signer.pubkey(), &recipient, 1_000_000)],
            Some(&delegated_signer.pubkey()),
            &[&delegated_signer],
            runtime.bank().last_blockhash(),
        );
        let versioned_transaction = VersionedTransaction::from(transaction);

        TransactionClient::send_transactions_in_batch(
            &runtime._tx_client,
            vec![bincode::serialize(&versioned_transaction).unwrap()],
            &SendTransactionServiceStats::default(),
        );

        assert_eq!(runtime.bank().get_balance(&recipient), 1_000_000);
        runtime.shutdown();
    }

    #[test]
    fn test_refresh_delegated_owner_programs_updates_programdata_and_invalidates_cache() {
        agave_logger::setup();

        let (parent_bank, mut runtime) = create_runtime();
        runtime.activate();

        let program_bytes = std::fs::read("../programs/bpf_loader/test_elfs/out/noop_aligned.so")
            .expect("noop ELF should exist");
        let l1_bank = Arc::new(Bank::new_from_parent(
            parent_bank.clone(),
            SlotLeader::default(),
            parent_bank.slot().saturating_add(1),
        ));
        let portal_program_id = runtime.portal_program_id;
        let owner_program = Pubkey::new_unique();
        let programdata_address = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();
        let mut delegated_l1_snapshot = AccountSharedData::new(1_000_000, 4, &portal_program_id);
        delegated_l1_snapshot
            .data_as_mut_slice()
            .copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        l1_bank.store_account(&delegated_pubkey, &delegated_l1_snapshot);
        store_delegation_record(
            &l1_bank,
            &portal_program_id,
            &delegated_pubkey,
            &owner_program,
            0,
        );
        store_upgradeable_owner_program(
            &l1_bank,
            &owner_program,
            &programdata_address,
            l1_bank.slot(),
            &program_bytes,
        );
        runtime.handle_delegation_inner(
            &delegated_pubkey,
            delegated_l1_snapshot,
            Some(owner_program),
            Some(&l1_bank),
        );

        let fee_payer = Keypair::new();
        fund_account(&runtime.bank(), &fee_payer.pubkey(), 1_000_000_000);
        let instruction =
            solana_instruction::Instruction::new_with_bytes(owner_program, &[], vec![]);
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&fee_payer.pubkey()),
            &[&fee_payer],
            runtime.bank().last_blockhash(),
        );
        runtime
            .bank()
            .process_transaction(&transaction)
            .expect("noop program should execute and populate ER ProgramCache");
        assert!(!runtime
            .bank()
            .get_transaction_processor()
            .global_program_cache
            .read()
            .unwrap()
            .get_slot_versions_for_tests(&owner_program)
            .is_empty());

        let l1_upgrade_bank = Bank::new_from_parent(
            l1_bank.clone(),
            SlotLeader::default(),
            l1_bank.slot().saturating_add(1),
        );
        store_upgradeable_owner_program(
            &l1_upgrade_bank,
            &owner_program,
            &programdata_address,
            l1_upgrade_bank.slot(),
            &program_bytes,
        );
        let upgraded_programdata = l1_upgrade_bank.get_account(&programdata_address).unwrap();

        runtime.refresh_delegated_owner_programs_from_l1(&l1_upgrade_bank);

        let er_programdata = runtime
            .bank()
            .get_account(&programdata_address)
            .expect("upgraded programdata should be hydrated into ER");
        assert_eq!(er_programdata.data(), upgraded_programdata.data());
        assert!(runtime
            .bank()
            .get_transaction_processor()
            .global_program_cache
            .read()
            .unwrap()
            .get_slot_versions_for_tests(&owner_program)
            .is_empty());

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_valid() {
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let owner_program = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();
        let grid_id = 7;

        let account = AccountSharedData::new(1_000_000, 0, &portal_program_id);
        parent_bank.store_account(&delegated_pubkey, &account);
        store_delegation_record(
            &parent_bank,
            &portal_program_id,
            &delegated_pubkey,
            &owner_program,
            grid_id,
        );
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        assert!(runtime.delegated_accounts().contains(&delegated_pubkey));
        let snapshot = runtime
            .initial_account_snapshot(&delegated_pubkey)
            .expect("L1 snapshot should be stored");
        assert_eq!(snapshot.owner(), &portal_program_id);

        let er_account = runtime
            .bank()
            .get_account(&delegated_pubkey)
            .expect("delegated account should be visible in ER");
        assert_eq!(er_account.owner(), &owner_program);

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_missing_record() {
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();

        let account = AccountSharedData::new(1_000_000, 0, &portal_program_id);
        parent_bank.store_account(&delegated_pubkey, &account);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        assert!(!runtime.delegated_accounts().contains(&delegated_pubkey));
        assert!(runtime
            .initial_account_snapshot(&delegated_pubkey)
            .is_none());

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_wrong_grid() {
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let owner_program = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();

        let account = AccountSharedData::new(1_000_000, 0, &portal_program_id);
        parent_bank.store_account(&delegated_pubkey, &account);
        store_delegation_record(
            &parent_bank,
            &portal_program_id,
            &delegated_pubkey,
            &owner_program,
            41,
        );
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 42,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        assert!(!runtime.delegated_accounts().contains(&delegated_pubkey));
        assert!(runtime
            .initial_account_snapshot(&delegated_pubkey)
            .is_none());

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_wrong_owner() {
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let wrong_owner_program = Pubkey::new_unique();
        let delegated_pubkey = Pubkey::new_unique();

        let account = AccountSharedData::new(1_000_000, 0, &wrong_owner_program);
        parent_bank.store_account(&delegated_pubkey, &account);
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        assert!(!runtime.delegated_accounts().contains(&delegated_pubkey));

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_nonexistent() {
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let nonexistent_pubkey = Pubkey::new_unique();

        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            delegated_accounts: vec![nonexistent_pubkey],
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

        assert!(!runtime.delegated_accounts().contains(&nonexistent_pubkey));

        runtime.shutdown();
    }

    #[test]
    fn test_delegation_validation_multiple() {
        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let owner_program1 = Pubkey::new_unique();
        let owner_program2 = Pubkey::new_unique();

        let delegated1 = Pubkey::new_unique();
        let delegated2 = Pubkey::new_unique();
        let wrong_owner = Pubkey::new_unique();

        let account1 = AccountSharedData::new(1_000_000, 0, &portal_program_id);
        let account2 = AccountSharedData::new(2_000_000, 0, &portal_program_id);
        let account3 = AccountSharedData::new(3_000_000, 0, &Pubkey::new_unique()); // wrong owner

        parent_bank.store_account(&delegated1, &account1);
        parent_bank.store_account(&delegated2, &account2);
        parent_bank.store_account(&wrong_owner, &account3);
        store_delegation_record(
            &parent_bank,
            &portal_program_id,
            &delegated1,
            &owner_program1,
            0,
        );
        store_delegation_record(
            &parent_bank,
            &portal_program_id,
            &delegated2,
            &owner_program2,
            0,
        );
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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

        assert_eq!(runtime.delegated_accounts().len(), 2);
        assert!(runtime.delegated_accounts().contains(&delegated1));
        assert!(runtime.delegated_accounts().contains(&delegated2));

        assert!(runtime.initial_account_snapshot(&delegated1).is_some());
        assert!(runtime.initial_account_snapshot(&delegated2).is_some());
        assert_eq!(
            runtime.bank().get_account(&delegated1).unwrap().owner(),
            &owner_program1
        );
        assert_eq!(
            runtime.bank().get_account(&delegated2).unwrap().owner(),
            &owner_program2
        );

        runtime.shutdown();
    }

    #[test]
    fn test_rpc_get_delegated_accounts() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();

        let delegated1 = Pubkey::new_unique();
        let delegated2 = Pubkey::new_unique();
        let owner_program1 = Pubkey::new_unique();
        let owner_program2 = Pubkey::new_unique();
        let account1 = AccountSharedData::new(1_000_000, 0, &portal_program_id);
        let account2 = AccountSharedData::new(2_000_000, 0, &portal_program_id);
        parent_bank.store_account(&delegated1, &account1);
        parent_bank.store_account(&delegated2, &account2);
        store_delegation_record(
            &parent_bank,
            &portal_program_id,
            &delegated1,
            &owner_program1,
            0,
        );
        store_delegation_record(
            &parent_bank,
            &portal_program_id,
            &delegated2,
            &owner_program2,
            0,
        );
        parent_bank.freeze();

        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
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
            let next = Bank::new_from_parent(parent, SlotLeader::default(), s);
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
