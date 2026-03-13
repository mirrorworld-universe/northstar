use {
    log::{debug, info},
    solana_hash::Hash,
    solana_pubkey::Pubkey,
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

const ROOT_INTERVAL: u64 = 32;

pub struct SlotAdvancer {
    thread: JoinHandle<()>,
    _exit: Arc<AtomicBool>,
}

impl SlotAdvancer {
    pub fn new(
        bank_forks: Arc<RwLock<BankForks>>,
        block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
        initial_bank: Arc<Bank>,
        slot_duration: Duration,
        manager_account: Pubkey,
        exit: Arc<AtomicBool>,
    ) -> Self {
        let exit_clone = Arc::clone(&exit);
        let thread = thread::spawn(move || {
            Self::run(
                bank_forks,
                block_commitment_cache,
                initial_bank,
                slot_duration,
                manager_account,
                exit_clone,
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
        slot_duration: Duration,
        manager_account: Pubkey,
        exit: Arc<AtomicBool>,
    ) {
        let mut current_slot = current_bank.slot();
        let mut slots_since_root = 0u64;

        while !exit.load(Ordering::Relaxed) {
            thread::sleep(slot_duration);

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
            let next_bank = Bank::new_from_parent(current_bank, &manager_account, current_slot);

            let next_bank_arc = {
                let mut bank_forks_write = bank_forks.write().unwrap();
                let inserted = bank_forks_write.insert(next_bank);
                slots_since_root += 1;

                if slots_since_root >= ROOT_INTERVAL {
                    bank_forks_write.set_root(current_slot, None, None);
                    slots_since_root = 0;
                }

                inserted.clone_without_scheduler()
            };

            {
                let mut cache = block_commitment_cache.write().unwrap();
                *cache = BlockCommitmentCache::new(
                    std::collections::HashMap::new(),
                    0,
                    CommitmentSlots {
                        slot: current_slot,
                        root: current_slot,
                        highest_confirmed_slot: current_slot,
                        highest_super_majority_root: current_slot,
                    },
                );
            }

            debug!(
                "SlotAdvancer: advanced to slot {}, new blockhash {}",
                current_slot,
                next_bank_arc.last_blockhash()
            );

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

    #[test]
    fn test_slot_advances() {
        agave_logger::setup();

        let parent_bank = create_test_bank();
        let bank_forks = BankForks::new_rw_arc(parent_bank);
        let initial_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        let initial_slot = initial_bank.slot();

        let exit = Arc::new(AtomicBool::new(false));
        let slot_duration = Duration::from_millis(10);

        let block_commitment_cache = Arc::new(RwLock::new(BlockCommitmentCache::new(
            std::collections::HashMap::new(),
            0,
            CommitmentSlots {
                slot: initial_slot,
                root: initial_slot,
                highest_confirmed_slot: initial_slot,
                highest_super_majority_root: initial_slot,
            },
        )));

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            block_commitment_cache,
            initial_bank,
            slot_duration,
            Default::default(),
            exit.clone(),
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
        let slot_duration = Duration::from_millis(10);

        let block_commitment_cache = Arc::new(RwLock::new(BlockCommitmentCache::new(
            std::collections::HashMap::new(),
            0,
            CommitmentSlots {
                slot: initial_slot,
                root: initial_slot,
                highest_confirmed_slot: initial_slot,
                highest_super_majority_root: initial_slot,
            },
        )));

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            block_commitment_cache,
            initial_bank,
            slot_duration,
            Default::default(),
            exit.clone(),
        );

        thread::sleep(Duration::from_millis(50));

        exit.store(true, Ordering::Relaxed);
        advancer.join();

        let latest_bank = bank_forks.read().unwrap().working_bank();
        let new_blockhash = latest_bank.last_blockhash();
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
        let slot_duration = Duration::from_millis(10);

        let block_commitment_cache = Arc::new(RwLock::new(BlockCommitmentCache::new(
            std::collections::HashMap::new(),
            0,
            CommitmentSlots {
                slot: initial_slot,
                root: initial_slot,
                highest_confirmed_slot: initial_slot,
                highest_super_majority_root: initial_slot,
            },
        )));

        let advancer = SlotAdvancer::new(
            bank_forks.clone(),
            block_commitment_cache,
            initial_bank,
            slot_duration,
            Default::default(),
            exit.clone(),
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
}
