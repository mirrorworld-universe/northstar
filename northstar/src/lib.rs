use {
    log::*,
    solana_clock::Slot,
    solana_gossip::cluster_info::ClusterInfo,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    std::sync::{Arc, RwLock},
    thiserror::Error,
};

pub mod ephemeral_runtime;
pub mod ephemeral_tx_client;
pub mod slot_advancer;

pub use crate::ephemeral_runtime::EphemeralRuntime;

#[derive(Error, Debug)]
pub enum NorthStarError {
    #[error("Failed to create ephemeral runtime: {0}")]
    RuntimeCreationFailed(String),
}

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

    /// Manager account keypair (for signing transactions in ephemeral rollups)
    pub manager_account: Arc<Keypair>,
}

/// Metadata about an ephemeral fork
#[derive(Debug, Clone)]
pub struct EphemeralForkMetadata {}

/// Main manager for ephemeral rollup forks
pub struct Manager {
    config: ManagerConfig,
    bank_forks: Arc<RwLock<BankForks>>,
    /// Active ephemeral runtime, if one is running
    active_runtime: Option<EphemeralRuntime>,
}

impl Manager {
    /// Create a new NorthStar Manager
    pub fn new(config: ManagerConfig, bank_forks: Arc<RwLock<BankForks>>) -> Self {
        info!("Initializing NorthStar Manager with config: {config:?}");
        Self {
            config,
            bank_forks,
            active_runtime: None,
        }
    }

    /// Check if an ephemeral runtime is currently active
    pub fn has_active_runtime(&self) -> bool {
        self.active_runtime.is_some()
    }

    /// Get the RPC port of the active runtime, if any
    pub fn active_runtime_port(&self) -> Option<u16> {
        self.active_runtime.as_ref().map(|r| r.rpc_port())
    }

    /// Shutdown the active ephemeral runtime, if any
    pub fn shutdown_active_runtime(&mut self) {
        if let Some(mut runtime) = self.active_runtime.take() {
            info!("Shutting down ephemeral rollup");
            runtime.shutdown();
        }
    }

    pub fn get_l1_events(&self, slot: Slot) -> Vec<L1Event> {
        let Some(bank) = self.bank_forks.read().unwrap().get(slot) else {
            debug!("Slot {} not found in bank_forks, skipping", slot);
            return vec![];
        };
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

    /// Create and store an EphemeralRuntime from the root bank
    ///
    /// This creates a fully functional ephemeral rollup with its own RPC server.
    /// The runtime is stored in the Manager and can be accessed via active_runtime_port().
    pub fn create_ephemeral_runtime(
        &mut self,
        root_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        settings: EphemeralRollupSettings,
        rpc_port: u16,
    ) -> Result<()> {
        if self.active_runtime.is_some() {
            info!("Ephemeral runtime already active, skipping creation");
            return Ok(());
        }

        let runtime =
            EphemeralRuntime::new(root_bank, cluster_info, settings, rpc_port).map_err(|e| {
                error!("Failed to create ephemeral runtime: {}", e);
                NorthStarError::RuntimeCreationFailed(e)
            })?;

        info!("Ephemeral rollup started on port {}", rpc_port);
        self.active_runtime = Some(runtime);
        Ok(())
    }
}
