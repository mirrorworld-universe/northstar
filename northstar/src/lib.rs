use {log::*, solana_pubkey::Pubkey, solana_runtime::bank::Bank, std::sync::Arc, thiserror::Error};

#[derive(Error, Debug)]
pub enum NorthStarError {}

pub type Result<T> = std::result::Result<T, NorthStarError>;

/// Configuration for NorthStar Manager
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Portal program ID (to read L1 state from)
    // TODO: Configure this from genesis or parameter
    pub portal_program_id: Pubkey,
}

/// Metadata about an ephemeral fork
#[derive(Debug, Clone)]
pub struct EphemeralForkMetadata {}

/// Main manager for ephemeral rollup forks
pub struct Manager {
    config: ManagerConfig,
}

impl Manager {
    /// Create a new NorthStar Manager
    pub fn new(config: ManagerConfig) -> Self {
        info!("Initializing NorthStar Manager with config: {:?}", config);
        Self { config }
    }

    /// Create an ephemeral fork from the root bank
    ///
    /// This is the main entry point when a new root is detected.
    /// It creates a virtual fork bank for rollup simulation.
    pub fn create_ephemeral_fork_from_root(
        &self,
        _root_bank: Arc<Bank>,
    ) -> Result<EphemeralForkMetadata> {
        Ok(todo!())
    }
}
