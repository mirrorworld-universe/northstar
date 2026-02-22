use {
    log::*,
    solana_clock::Slot,
    solana_pubkey::Pubkey,
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    std::sync::{Arc, RwLock},
    thiserror::Error,
};

#[derive(Error, Debug)]
pub enum NorthStarError {}

pub type Result<T> = std::result::Result<T, NorthStarError>;

#[derive(Debug, Clone)]
pub struct EphemeralRollupSettings {
    pub delegated_addresses: Vec<Pubkey>,
}

#[derive(Debug, Clone)]
pub enum L1Event {
    CreateEphemeralRollup(EphemeralRollupSettings),
}

/// Configuration for NorthStar Manager
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Portal program ID (to read L1 state from)
    // TODO: Configure this from genesis or parameter
    pub portal_program_id: Pubkey,

    /// Manager account pubkey
    pub manager_account: Pubkey,
}

/// Metadata about an ephemeral fork
#[derive(Debug, Clone)]
pub struct EphemeralForkMetadata {}

/// Main manager for ephemeral rollup forks
pub struct Manager {
    config: ManagerConfig,
    bank_forks: Arc<RwLock<BankForks>>,
}

impl Manager {
    /// Create a new NorthStar Manager
    pub fn new(config: ManagerConfig, bank_forks: Arc<RwLock<BankForks>>) -> Self {
        info!("Initializing NorthStar Manager with config: {config:?}");
        Self { config, bank_forks }
    }

    pub fn get_l1_events(&self, slot: Slot) -> Vec<L1Event> {
        let bank = self.bank_forks.read().unwrap().get(slot).unwrap();
        let logs = bank
            .transaction_log_collector
            .read()
            .unwrap()
            .get_logs_for_address(Some(&self.config.portal_program_id))
            .unwrap_or_default();

        // TODO: parse logs properly
        if logs.is_empty() {
            vec![]
        } else {
            vec![L1Event::CreateEphemeralRollup(EphemeralRollupSettings {
                delegated_addresses: vec![],
            })]
        }
    }

    /// Create an ephemeral fork from the root bank
    ///
    /// This is the main entry point when a new root is detected.
    /// It creates a virtual fork bank for rollup simulation.
    pub fn create_ephemeral_fork_from_root(
        &self,
        from: Slot,
        EphemeralRollupSettings {
            delegated_addresses,
        }: EphemeralRollupSettings,
    ) -> Result<EphemeralForkMetadata> {
        let parent = self.bank_forks.read().unwrap().get(from).unwrap();
        let bank = Bank::new_from_parent(parent, &self.config.manager_account, from + 1);
        // TODO: execute some transactions
        // bank.load_execute_and_commit_transactions(
        //     batch,
        //     max_age,
        //     recording_config,
        //     timings,
        //     log_messages_bytes_limit,
        // );
        Ok(todo!())
    }
}
