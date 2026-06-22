use {
    crate::EphemeralRollupSettings,
    log::{debug, info},
    solana_clock::Slot,
    solana_fee_structure::FeeStructure,
    solana_hash::Hash,
    solana_leader_schedule::SlotLeader,
    solana_pubkey::Pubkey,
    solana_rpc::{
        er_history::ErHistoryStore,
        optimistically_confirmed_bank_tracker::OptimisticallyConfirmedBank,
        rpc_subscriptions::RpcSubscriptions,
    },
    solana_runtime::{
        bank::Bank,
        bank_forks::BankForks,
        commitment::{BlockCommitmentCache, CommitmentSlots},
        installed_scheduler_pool::SchedulerStatus,
    },
    std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex, RwLock,
        },
        thread::{self, JoinHandle},
        time::Duration,
    },
};

#[derive(Clone, Debug)]
pub struct Config {
    /// Duration to sleep between slot processing iterations.
    pub slot_duration: Duration,
    /// Pubkey that will be the parent of all banks created by the advancer.
    pub manager_account: Pubkey,
    /// Explicit ER transaction fee model.
    /// Use `lamports_per_signature = 0` for demo/devnet gasless ER transactions.
    pub er_fee_structure: FeeStructure,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            slot_duration: Duration::from_millis(10),
            manager_account: Pubkey::default(),
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
        }
    }
}

pub struct SlotAdvancer {
    thread: JoinHandle<()>,
    _exit: Arc<AtomicBool>,
}

const ER_SLOT_OFFSET: Slot = 1u64 << 40;

impl SlotAdvancer {
    pub fn new(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
        optimistically_confirmed_bank: Arc<RwLock<OptimisticallyConfirmedBank>>,
        initial_bank: Arc<Bank>,
        config: Config,
        exit: Arc<AtomicBool>,
        rpc_subscriptions: Option<Arc<RpcSubscriptions>>,
    ) -> Self {
        Self::new_with_history(
            bank_forks,
            bank_operation_lock,
            block_commitment_cache,
            optimistically_confirmed_bank,
            initial_bank,
            config,
            exit,
            rpc_subscriptions,
            None,
        )
    }

    pub fn new_with_history(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
        optimistically_confirmed_bank: Arc<RwLock<OptimisticallyConfirmedBank>>,
        initial_bank: Arc<Bank>,
        config: Config,
        exit: Arc<AtomicBool>,
        rpc_subscriptions: Option<Arc<RpcSubscriptions>>,
        er_history_store: Option<Arc<ErHistoryStore>>,
    ) -> Self {
        let exit_clone = Arc::clone(&exit);
        let thread = thread::spawn(move || {
            Self::run(
                bank_forks,
                bank_operation_lock,
                block_commitment_cache,
                optimistically_confirmed_bank,
                initial_bank,
                config,
                exit_clone,
                rpc_subscriptions,
                er_history_store,
            );
        });
        Self {
            thread,
            _exit: exit,
        }
    }

    fn run(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
        optimistically_confirmed_bank: Arc<RwLock<OptimisticallyConfirmedBank>>,
        initial_bank: Arc<Bank>,
        config: Config,
        exit: Arc<AtomicBool>,
        rpc_subscriptions: Option<Arc<RpcSubscriptions>>,
        er_history_store: Option<Arc<ErHistoryStore>>,
    ) {
        info!(
            "SlotAdvancer: starting at slot {}, bank_forks_root {}",
            initial_bank.slot(),
            bank_forks.read().unwrap().root()
        );

        while !exit.load(Ordering::Relaxed) {
            thread::sleep(config.slot_duration);

            let (current_bank_slot, next_bank_slot, frozen_bank, next_bank_arc) = {
                let _bank_operation_guard = bank_operation_lock.lock().unwrap();
                let current_bank = bank_forks.read().unwrap().working_bank();

                let max_tick_height = current_bank.max_tick_height();
                let tick_height = current_bank.tick_height();
                let ticks_remaining = max_tick_height - tick_height;

                debug!(
                    "SlotAdvancer: current slot {}, tick_height {}, max_tick_height {}, \
                     ticks_remaining {}",
                    current_bank.slot(),
                    tick_height,
                    max_tick_height,
                    ticks_remaining
                );

                for _ in 0..ticks_remaining {
                    // Ephemeral rollup: PoH is not verified externally, so we
                    // use random tick hashes to drive slot advancement.
                    let hash = Hash::new_unique();
                    let scheduler = RwLock::new(SchedulerStatus::Unavailable);
                    current_bank.register_tick(&hash, &scheduler);
                }

                current_bank.freeze();
                if let Some(er_history_store) = &er_history_store {
                    er_history_store.finalize_slot(&current_bank);
                }

                debug!(
                    "SlotAdvancer: after freeze, slot {}, blockhash {}",
                    current_bank.slot(),
                    current_bank.last_blockhash()
                );

                let current_bank_slot = current_bank.slot();
                let frozen_bank = current_bank.clone();
                let next_bank_slot = current_bank_slot.saturating_add(1);
                let mut next_bank = Bank::new_from_parent_ephemeral(
                    current_bank,
                    SlotLeader {
                        id: config.manager_account,
                        vote_address: Pubkey::default(),
                    },
                    next_bank_slot,
                );
                next_bank.configure_er(
                    &config.er_fee_structure,
                    crate::er_recent_blockhash_max_age_for_slot_duration(config.slot_duration),
                );

                let next_bank_arc = {
                    let mut bank_forks_write = bank_forks.write().unwrap();
                    let inserted = bank_forks_write.insert_ephemeral(next_bank);
                    let next_bank_arc = inserted.clone_without_scheduler();

                    // NOTE: We intentionally do NOT call BankForks::set_root() here.
                    // The ER shares an AccountsDb with the L1 validator.
                    // Bank::squash() (called by set_root) walks the entire parent
                    // chain and calls add_root() for each slot — including the L1
                    // parent.  Because the L1 concurrently advances its own roots
                    // on the same AccountsDb, this causes a "Roots must be added
                    // in order" panic.  Instead, set_root_ephemeral() advances only
                    // BankForks root metadata and prunes old ER banks.

                    // Sonic: ER account lookup uses `ancestors`, not recursive parent
                    // traversal. Once child exists, the frozen ER bank no longer
                    // needs to keep an Arc to its ER parent alive. Do not mutate the
                    // initial ER bank's L1 anchor parent.
                    if frozen_bank
                        .parent()
                        .is_some_and(|parent| parent.slot() >= ER_SLOT_OFFSET)
                    {
                        frozen_bank.disconnect_from_parent();
                    }

                    drop(bank_forks_write.set_root_ephemeral(current_bank_slot));

                    next_bank_arc
                };

                (
                    current_bank_slot,
                    next_bank_slot,
                    frozen_bank,
                    next_bank_arc,
                )
            };

            {
                let mut cache = block_commitment_cache.write().unwrap();
                // We don't call set_root (see above), so report the current
                // frozen slot as both the tip and the "root" for RPC purposes.
                // This makes processed/finalized resolve to latest frozen bank.
                *cache = BlockCommitmentCache::new(
                    std::collections::HashMap::new(),
                    0,
                    CommitmentSlots {
                        slot: current_bank_slot,
                        root: current_bank_slot,
                        highest_confirmed_slot: current_bank_slot,
                        highest_super_majority_root: current_bank_slot,
                    },
                );
            }

            // Confirmed commitment should track latest frozen ER bank.
            // RPC preflight simulation requires a frozen bank, so do not point
            // this at the new working bank.
            *optimistically_confirmed_bank.write().unwrap() =
                OptimisticallyConfirmedBank { bank: frozen_bank };

            debug!(
                "SlotAdvancer: advanced to slot {}, new blockhash {}",
                next_bank_slot,
                next_bank_arc.last_blockhash()
            );

            // Sonic: Notify RPC subscriptions about finalized ER slot and new slot/root.
            if let Some(ref subs) = rpc_subscriptions {
                subs.notify_subscribers(CommitmentSlots {
                    slot: current_bank_slot,
                    root: current_bank_slot,
                    highest_confirmed_slot: current_bank_slot,
                    highest_super_majority_root: current_bank_slot,
                });
                subs.notify_slot(next_bank_slot, current_bank_slot, next_bank_slot);
                subs.notify_roots(vec![next_bank_slot]);
            }
        }

        info!("SlotAdvancer: thread exiting");
    }

    pub fn join(self) {
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        serde_json::Value,
        solana_rpc::rpc_subscription_tracker::SubscriptionParams,
        std::{sync::atomic::AtomicU64, time::Instant},
        tokio::sync::broadcast::error::TryRecvError,
    };

    fn create_test_bank() -> Bank {
        use solana_genesis_config::GenesisConfig;
        let genesis_config = GenesisConfig::new(&[], &[]);
        Bank::new_for_tests(&genesis_config)
    }

    fn create_block_commitment_cache(slot: u64) -> Arc<RwLock<BlockCommitmentCache>> {
        Arc::new(RwLock::new(BlockCommitmentCache::new(
            std::collections::HashMap::new(),
            0,
            CommitmentSlots {
                slot,
                root: slot,
                highest_confirmed_slot: slot,
                highest_super_majority_root: slot,
            },
        )))
    }

    fn create_optimistically_confirmed_bank(
        bank: Arc<Bank>,
    ) -> Arc<RwLock<OptimisticallyConfirmedBank>> {
        Arc::new(RwLock::new(OptimisticallyConfirmedBank { bank }))
    }

    fn receive_notification_json(
        receiver: &mut tokio::sync::broadcast::Receiver<
            solana_rpc::rpc_subscriptions::RpcNotification,
        >,
        deadline: Instant,
    ) -> Option<Value> {
        while Instant::now() < deadline {
            match receiver.try_recv() {
                Ok(notification) => {
                    if let Some(json) = notification.json.upgrade() {
                        return Some(serde_json::from_str(&json).unwrap());
                    }
                }
                Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
                Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Closed) => panic!("subscription broadcast channel closed"),
            }
        }
        None
    }

    #[test]
    fn test_slot_advancer_notifies_roots() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let bank_forks = BankForks::new_rw_arc(parent_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());
        let initial_slot = initial_bank.slot();

        let exit = Arc::new(AtomicBool::new(false));
        let block_commitment_cache = create_block_commitment_cache(initial_slot);
        let optimistically_confirmed_bank =
            create_optimistically_confirmed_bank(initial_bank.clone());
        let rpc_subscriptions = Arc::new(RpcSubscriptions::new_for_tests(
            exit.clone(),
            Arc::new(AtomicU64::default()),
            bank_forks.clone(),
            block_commitment_cache.clone(),
            optimistically_confirmed_bank.clone(),
        ));
        let _root_token = rpc_subscriptions
            .control()
            .subscribe(SubscriptionParams::Root)
            .unwrap();
        let _slots_updates_token = rpc_subscriptions
            .control()
            .subscribe(SubscriptionParams::SlotsUpdates)
            .unwrap();
        let mut receiver = rpc_subscriptions.control().broadcast_receiver();

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            Arc::new(Mutex::new(())),
            block_commitment_cache,
            optimistically_confirmed_bank,
            initial_bank,
            Config {
                slot_duration: Duration::from_millis(5),
                manager_account: Pubkey::default(),
                er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            },
            exit.clone(),
            Some(rpc_subscriptions),
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_root_notification = false;
        let mut saw_slots_update_root = false;

        while Instant::now() < deadline && !(saw_root_notification && saw_slots_update_root) {
            let Some(notification) = receive_notification_json(&mut receiver, deadline) else {
                break;
            };
            match notification["method"].as_str() {
                Some("rootNotification") => {
                    if notification["params"]["result"].as_u64() > Some(initial_slot) {
                        saw_root_notification = true;
                    }
                }
                Some("slotsUpdatesNotification") => {
                    let result = &notification["params"]["result"];
                    if result["type"] == "root" && result["slot"].as_u64() > Some(initial_slot) {
                        saw_slots_update_root = true;
                    }
                }
                _ => {}
            }
        }

        exit.store(true, Ordering::Relaxed);
        advancer.join();

        assert!(
            saw_root_notification,
            "slot advancer must notify rootSubscribe subscribers"
        );
        assert!(
            saw_slots_update_root,
            "slot advancer must emit root updates for slotsUpdatesSubscribe subscribers"
        );
    }

    #[test]
    fn test_slot_advances() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let bank_forks = BankForks::new_rw_arc(parent_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());
        let initial_slot = initial_bank.slot();

        let exit = Arc::new(AtomicBool::new(false));
        let config = Config::default();
        let block_commitment_cache = create_block_commitment_cache(initial_slot);
        let optimistically_confirmed_bank =
            create_optimistically_confirmed_bank(initial_bank.clone());

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            Arc::new(Mutex::new(())),
            block_commitment_cache,
            optimistically_confirmed_bank,
            initial_bank,
            config,
            exit.clone(),
            None,
        );

        thread::sleep(Duration::from_millis(300));
        exit.store(true, Ordering::Relaxed);
        advancer.join();

        let latest_slot = bank_forks.read().unwrap().working_bank().slot();
        assert!(
            latest_slot > initial_slot,
            "Slot should have advanced from {}, but got {}",
            initial_slot,
            latest_slot
        );
    }

    #[test]
    fn test_blockhash_refreshes() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let bank_forks = BankForks::new_rw_arc(parent_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());
        let initial_blockhash = initial_bank.last_blockhash();
        let initial_slot = initial_bank.slot();

        let exit = Arc::new(AtomicBool::new(false));
        let config = Config::default();
        let block_commitment_cache = create_block_commitment_cache(initial_slot);
        let optimistically_confirmed_bank =
            create_optimistically_confirmed_bank(initial_bank.clone());

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            Arc::new(Mutex::new(())),
            block_commitment_cache,
            optimistically_confirmed_bank,
            initial_bank,
            config,
            exit.clone(),
            None,
        );

        thread::sleep(Duration::from_millis(50));
        exit.store(true, Ordering::Relaxed);
        advancer.join();

        let new_blockhash = bank_forks.read().unwrap().working_bank().last_blockhash();
        assert_ne!(
            initial_blockhash, new_blockhash,
            "Blockhash should have changed"
        );
    }

    #[test]
    fn test_exit_stops_advancer() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let bank_forks = BankForks::new_rw_arc(parent_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());
        let initial_slot = initial_bank.slot();

        let exit = Arc::new(AtomicBool::new(false));
        let config = Config::default();
        let block_commitment_cache = create_block_commitment_cache(initial_slot);
        let optimistically_confirmed_bank =
            create_optimistically_confirmed_bank(initial_bank.clone());

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            Arc::new(Mutex::new(())),
            block_commitment_cache,
            optimistically_confirmed_bank,
            initial_bank,
            config,
            exit.clone(),
            None,
        );

        thread::sleep(Duration::from_millis(100));
        let slot_before_exit = bank_forks.read().unwrap().working_bank().slot();
        exit.store(true, Ordering::Relaxed);
        advancer.join();

        thread::sleep(Duration::from_millis(50));
        let slot_after_exit = bank_forks.read().unwrap().working_bank().slot();
        assert!(
            slot_after_exit <= slot_before_exit + 1,
            "Slot should not advance much after exit (before: {}, after: {})",
            slot_before_exit,
            slot_after_exit
        );
    }

    /// Regression test: slot advancer works starting from a non-zero slot.
    #[test]
    fn test_slot_advances_from_nonzero_initial_slot() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let ephemeral_slot = 40u64;
        let ephemeral_bank =
            Bank::new_from_parent(parent_bank, SlotLeader::default(), ephemeral_slot);

        let bank_forks = BankForks::new_rw_arc_ephemeral(ephemeral_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        let exit = Arc::new(AtomicBool::new(false));
        let config = Config {
            slot_duration: Duration::from_millis(5),
            manager_account: Pubkey::default(),
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
        };
        let block_commitment_cache = create_block_commitment_cache(initial_bank.slot());
        let optimistically_confirmed_bank =
            create_optimistically_confirmed_bank(initial_bank.clone());

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            Arc::new(Mutex::new(())),
            block_commitment_cache,
            optimistically_confirmed_bank,
            initial_bank,
            config,
            exit.clone(),
            None,
        );

        thread::sleep(Duration::from_millis(300));
        exit.store(true, Ordering::Relaxed);
        advancer.join();

        let latest_slot = bank_forks.read().unwrap().working_bank().slot();
        assert!(
            latest_slot > ephemeral_slot + 5,
            "Should have advanced from non-zero initial slot {} by multiple slots, but only got {}",
            ephemeral_slot,
            latest_slot
        );
    }

    #[test]
    fn test_ephemeral_slot_advancer_keeps_parent_chain_shallow() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let initial_slot = 1u64 << 40;
        let initial_bank = Bank::new_from_parent_ephemeral_isolated(
            parent_bank,
            SlotLeader::default(),
            initial_slot,
        );
        let ticks_per_slot = initial_bank.ticks_per_slot();
        initial_bank.set_tick_height(initial_bank.max_tick_height() - ticks_per_slot);

        let bank_forks = BankForks::new_rw_arc_ephemeral(initial_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        let exit = Arc::new(AtomicBool::new(false));
        let block_commitment_cache = create_block_commitment_cache(initial_bank.slot());
        let optimistically_confirmed_bank =
            create_optimistically_confirmed_bank(initial_bank.clone());

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            Arc::new(Mutex::new(())),
            block_commitment_cache,
            optimistically_confirmed_bank,
            initial_bank,
            Config {
                slot_duration: Duration::from_millis(5),
                manager_account: Pubkey::default(),
                er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            },
            exit.clone(),
            None,
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let latest_slot = bank_forks.read().unwrap().working_bank().slot();
            if latest_slot > initial_slot + 5 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        exit.store(true, Ordering::Relaxed);
        advancer.join();

        let working_bank = bank_forks.read().unwrap().working_bank();
        assert!(
            working_bank.slot() > initial_slot + 5,
            "slot advancer only reached slot {} from initial slot {}",
            working_bank.slot(),
            initial_slot
        );
        assert!(
            working_bank.parents().len() <= 2,
            "ER working bank parent chain grew too deep: {}",
            working_bank.parents().len()
        );
    }

    #[test]
    fn test_ephemeral_slot_advancer_prunes_old_bank_forks() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        parent_bank.freeze();
        let parent_bank = Arc::new(parent_bank);

        let initial_slot = 1u64 << 40;
        let initial_bank = Bank::new_from_parent_ephemeral_isolated(
            parent_bank,
            SlotLeader::default(),
            initial_slot,
        );
        let ticks_per_slot = initial_bank.ticks_per_slot();
        initial_bank.set_tick_height(initial_bank.max_tick_height() - ticks_per_slot);

        let bank_forks = BankForks::new_rw_arc_ephemeral(initial_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());
        let exit = Arc::new(AtomicBool::new(false));
        let block_commitment_cache = create_block_commitment_cache(initial_bank.slot());
        let optimistically_confirmed_bank =
            create_optimistically_confirmed_bank(initial_bank.clone());

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            Arc::new(Mutex::new(())),
            block_commitment_cache,
            optimistically_confirmed_bank,
            initial_bank,
            Config {
                slot_duration: Duration::from_millis(5),
                manager_account: Pubkey::default(),
                er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            },
            exit.clone(),
            None,
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let latest_slot = bank_forks.read().unwrap().working_bank().slot();
            if latest_slot > initial_slot + 8 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        exit.store(true, Ordering::Relaxed);
        advancer.join();

        let bank_forks = bank_forks.read().unwrap();
        assert!(
            bank_forks.working_bank().slot() > initial_slot + 8,
            "slot advancer did not run enough slots"
        );
        assert!(
            bank_forks.len() <= 3,
            "old ER banks should not accumulate in BankForks; retained {} banks",
            bank_forks.len()
        );
        let descendant_entries = bank_forks.descendants().len();
        assert!(
            descendant_entries <= 3,
            "old ER ancestry should not accumulate in BankForks descendants; retained \
             {descendant_entries} entries",
        );
    }

    #[test]
    fn test_slot_advancer_waits_for_bank_operation_lock() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let bank_forks = BankForks::new_rw_arc(parent_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());
        let initial_slot = initial_bank.slot();

        let exit = Arc::new(AtomicBool::new(false));
        let block_commitment_cache = create_block_commitment_cache(initial_slot);
        let optimistically_confirmed_bank =
            create_optimistically_confirmed_bank(initial_bank.clone());
        let bank_operation_lock = Arc::new(Mutex::new(()));
        let bank_operation_guard = bank_operation_lock.lock().unwrap();

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            bank_operation_lock.clone(),
            block_commitment_cache,
            optimistically_confirmed_bank,
            initial_bank,
            Config {
                slot_duration: Duration::from_millis(5),
                manager_account: Pubkey::default(),
                er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            },
            exit.clone(),
            None,
        );

        thread::sleep(Duration::from_millis(50));
        let slot_while_locked = bank_forks.read().unwrap().working_bank().slot();
        assert_eq!(
            slot_while_locked, initial_slot,
            "slot advancer must not freeze/advance while bank op lock held"
        );

        drop(bank_operation_guard);
        thread::sleep(Duration::from_millis(50));
        exit.store(true, Ordering::Relaxed);
        advancer.join();

        let slot_after_release = bank_forks.read().unwrap().working_bank().slot();
        assert!(
            slot_after_release > initial_slot,
            "slot advancer should advance once bank op lock released"
        );
    }
}
