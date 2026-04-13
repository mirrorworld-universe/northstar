use {
    log::{debug, info},
    solana_hash::Hash,
    solana_pubkey::Pubkey,
    solana_rpc::rpc_subscriptions::RpcSubscriptions,
    solana_runtime::{
        bank::Bank,
        bank_forks::BankForks,
        commitment::{BlockCommitmentCache, CommitmentSlots},
        installed_scheduler_pool::SchedulerStatus,
    },
    std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, RwLock,
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            slot_duration: Duration::from_millis(10),
            manager_account: Pubkey::default(),
        }
    }
}

pub struct SlotAdvancer {
    thread: JoinHandle<()>,
    _exit: Arc<AtomicBool>,
}

impl SlotAdvancer {
    pub fn new(
        bank_forks: Arc<RwLock<BankForks>>,
        block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
        initial_bank: Arc<Bank>,
        config: Config,
        exit: Arc<AtomicBool>,
        rpc_subscriptions: Option<Arc<RpcSubscriptions>>,
    ) -> Self {
        let exit_clone = Arc::clone(&exit);
        let thread = thread::spawn(move || {
            Self::run(
                bank_forks,
                block_commitment_cache,
                initial_bank,
                config,
                exit_clone,
                rpc_subscriptions,
            );
        });
        Self {
            thread,
            _exit: exit,
        }
    }

    fn run(
        bank_forks: Arc<RwLock<BankForks>>,
        block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
        mut current_bank: Arc<Bank>,
        config: Config,
        exit: Arc<AtomicBool>,
        rpc_subscriptions: Option<Arc<RpcSubscriptions>>,
    ) {
        let mut current_slot = current_bank.slot();

        info!(
            "SlotAdvancer: starting at slot {}, bank_forks_root {}",
            current_slot,
            bank_forks.read().unwrap().root()
        );

        while !exit.load(Ordering::Relaxed) {
            thread::sleep(config.slot_duration);

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

            debug!(
                "SlotAdvancer: after freeze, slot {}, blockhash {}",
                current_bank.slot(),
                current_bank.last_blockhash()
            );

            current_slot += 1;
            let current_bank_slot = current_bank.slot();
            let next_bank =
                Bank::new_from_parent(current_bank, &config.manager_account, current_slot);

            let next_bank_arc = {
                let mut bank_forks_write = bank_forks.write().unwrap();
                let inserted = bank_forks_write.insert(next_bank);

                // NOTE: We intentionally do NOT call set_root() here.
                // The ER shares an AccountsDb with the L1 validator.
                // Bank::squash() (called by set_root) walks the entire parent
                // chain and calls add_root() for each slot — including the L1
                // parent.  Because the L1 concurrently advances its own roots
                // on the same AccountsDb, this causes a "Roots must be added
                // in order" panic.  The ER is short-lived so rooting is not
                // required for correctness; the commitment cache update below
                // is sufficient for RPC queries.

                inserted.clone_without_scheduler()
            };

            {
                let mut cache = block_commitment_cache.write().unwrap();
                // We don't call set_root (see above), so report the current
                // frozen slot as both the tip and the "root" for RPC purposes.
                // This makes all commitment levels (processed, confirmed,
                // finalized) resolve to the latest frozen bank.
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

            debug!(
                "SlotAdvancer: advanced to slot {}, new blockhash {}",
                current_slot,
                next_bank_arc.last_blockhash()
            );

            // Sonic: Notify RPC subscriptions about the new slot
            if let Some(ref subs) = rpc_subscriptions {
                subs.notify_slot(current_slot, current_bank_slot, current_slot);
            }

            current_bank = next_bank_arc;
        }

        info!("SlotAdvancer: thread exiting");
    }

    pub fn join(self) {
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            block_commitment_cache,
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

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            block_commitment_cache,
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

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            block_commitment_cache,
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
        let ephemeral_bank = Bank::new_from_parent(parent_bank, &Pubkey::default(), ephemeral_slot);

        let bank_forks = BankForks::new_rw_arc(ephemeral_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        let exit = Arc::new(AtomicBool::new(false));
        let config = Config {
            slot_duration: Duration::from_millis(5),
            manager_account: Pubkey::default(),
        };
        let block_commitment_cache = create_block_commitment_cache(initial_bank.slot());

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            block_commitment_cache,
            initial_bank,
            config,
            exit.clone(),
            None,
        );

        thread::sleep(Duration::from_millis(150));
        exit.store(true, Ordering::Relaxed);
        advancer.join();

        let latest_slot = bank_forks.read().unwrap().working_bank().slot();
        assert!(
            latest_slot > ephemeral_slot + 10,
            "Should have advanced well past initial slot {}, but only got {}",
            ephemeral_slot,
            latest_slot
        );
    }
}
