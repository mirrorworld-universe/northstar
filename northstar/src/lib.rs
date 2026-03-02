use {
    log::*,
    solana_account::ReadableAccount,
    solana_gossip::cluster_info::ClusterInfo,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    std::sync::{Arc, RwLock},
    thiserror::Error,
};

pub mod ephemeral_runtime;
pub mod ephemeral_tx_client;
pub mod portal_state;
pub mod slot_advancer;

pub use crate::{
    ephemeral_runtime::EphemeralRuntime,
    portal_state::{try_parse_portal_account, PortalAccount},
};

#[derive(Error, Debug)]
pub enum NorthStarError {
    #[error("Failed to create ephemeral runtime: {0}")]
    RuntimeCreationFailed(String),
}

pub type Result<T> = std::result::Result<T, NorthStarError>;

#[derive(Debug, Clone)]
pub struct EphemeralRollupSettings {
    pub session_pda: Pubkey,
    pub owner: Pubkey,
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
    pub delegated_accounts: Vec<Pubkey>,
}

/// Events detected on L1 that are relevant to ephemeral rollups.
///
/// These events are emitted when the NorthStar service scans portal
/// program accounts and detects state changes.
#[derive(Debug, Clone)]
pub enum L1Event {
    /// A new Session PDA was created on L1
    SessionOpened {
        session_pda: Pubkey,
        owner: Pubkey,
        grid_id: u64,
        ttl_slots: u64,
        fee_cap: u64,
    },
    /// A Session PDA was closed on L1
    SessionClosed {
        session_pda: Pubkey,
        owner: Pubkey,
        grid_id: u64,
    },
    /// An account was delegated to the portal program
    AccountDelegated {
        delegation_record_pda: Pubkey,
        delegated_account: Pubkey,
        owner_program: Pubkey,
        grid_id: u64,
    },
    /// An account was undelegated (returned to original owner)
    AccountUndelegated {
        delegation_record_pda: Pubkey,
        delegated_account: Pubkey,
    },
    /// A fee deposit was made
    FeeDeposited { fee_vault_pda: Pubkey, amount: u64 },
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

    pub fn get_l1_events(&self, bank: &Bank) -> Vec<L1Event> {
        let modified =
            bank.get_program_accounts_modified_since_parent(&self.config.portal_program_id);

        let mut events = Vec::new();

        for (pubkey, account) in &modified {
            let data = account.data();
            if data.is_empty() || data.iter().all(|b| *b == 0) {
                // Account was zeroed — determine type from previous state
                self.handle_zeroed_account(bank, pubkey, &mut events);
                continue;
            }

            match try_parse_portal_account(data) {
                Some(PortalAccount::Session(session)) => {
                    // Check if this is a new session (didn't exist in parent)
                    if self.is_new_in_slot(bank, pubkey) {
                        events.push(L1Event::SessionOpened {
                            session_pda: *pubkey,
                            owner: session.owner,
                            grid_id: session.grid_id,
                            ttl_slots: session.ttl_slots,
                            fee_cap: session.fee_cap,
                        });
                    }
                }
                Some(PortalAccount::DelegationRecord(record)) => {
                    if self.is_new_in_slot(bank, pubkey) {
                        // Find the delegated account by scanning
                        if let Some(delegated) = self.find_delegated_account(bank, pubkey, &record)
                        {
                            events.push(L1Event::AccountDelegated {
                                delegation_record_pda: *pubkey,
                                delegated_account: delegated,
                                owner_program: record.owner_program,
                                grid_id: record.grid_id,
                            });
                        }
                    }
                }
                Some(PortalAccount::FeeVault(vault)) => {
                    events.push(L1Event::FeeDeposited {
                        fee_vault_pda: *pubkey,
                        amount: vault.balance,
                    });
                }
                None => {
                    // Unrecognized — log and skip
                    debug!("Unrecognized portal account at {pubkey}");
                }
            }
        }

        events
    }

    /// Check if an account existed in the parent bank
    fn is_new_in_slot(&self, bank: &Bank, pubkey: &Pubkey) -> bool {
        match bank.parent() {
            Some(parent) => parent.get_account(pubkey).is_none(),
            None => true, // No parent means genesis — everything is new
        }
    }

    /// For a newly created DelegationRecord PDA, find which account was delegated
    /// by scanning all modifications in the slot
    fn find_delegated_account(
        &self,
        bank: &Bank,
        delegation_record_pda: &Pubkey,
        _record: &portal_state::DelegationRecord,
    ) -> Option<Pubkey> {
        // Get all accounts modified in this slot
        let all_modified = bank.get_all_accounts_modified_since_parent();

        for (pubkey, account) in &all_modified {
            // Skip the delegation record itself
            if pubkey == delegation_record_pda {
                continue;
            }
            // Check if this account is now owned by the portal program
            if account.owner() != &self.config.portal_program_id {
                continue;
            }
            // Verify PDA derivation matches
            let (expected_pda, _) = Pubkey::find_program_address(
                &[b"delegation", pubkey.as_ref()],
                &self.config.portal_program_id,
            );
            if &expected_pda == delegation_record_pda {
                return Some(*pubkey);
            }
        }

        warn!(
            "Could not find delegated account for delegation record {}",
            delegation_record_pda
        );
        None
    }

    /// Find accounts whose owner changed FROM the portal program (undelegation)
    fn find_undelegated_account(
        &self,
        bank: &Bank,
        delegation_record_pda: &Pubkey,
    ) -> Option<Pubkey> {
        let parent = bank.parent()?;
        let all_modified = bank.get_all_accounts_modified_since_parent();

        for (pubkey, account) in &all_modified {
            if pubkey == delegation_record_pda {
                continue;
            }
            // Account is now NOT owned by portal, but was before
            if account.owner() == &self.config.portal_program_id {
                continue;
            }
            // Check parent — was it owned by portal?
            if let Some(prev) = parent.get_account(pubkey) {
                if prev.owner() != &self.config.portal_program_id {
                    continue;
                }
                // Verify PDA derivation
                let (expected_pda, _) = Pubkey::find_program_address(
                    &[b"delegation", pubkey.as_ref()],
                    &self.config.portal_program_id,
                );
                if &expected_pda == delegation_record_pda {
                    return Some(*pubkey);
                }
            }
        }

        warn!(
            "Could not find undelegated account for delegation record {}",
            delegation_record_pda
        );
        None
    }

    /// When an account's data is zeroed, determine what type it was from the parent bank
    fn handle_zeroed_account(&self, bank: &Bank, pubkey: &Pubkey, events: &mut Vec<L1Event>) {
        let parent = match bank.parent() {
            Some(p) => p,
            None => return,
        };

        let prev_account = match parent.get_account(pubkey) {
            Some(a) => a,
            None => return,
        };

        let prev_data = prev_account.data();
        match try_parse_portal_account(prev_data) {
            Some(PortalAccount::Session(session)) => {
                events.push(L1Event::SessionClosed {
                    session_pda: *pubkey,
                    owner: session.owner,
                    grid_id: session.grid_id,
                });
            }
            Some(PortalAccount::DelegationRecord(_record)) => {
                // Find the delegated account that was undelegated
                // by scanning for accounts whose owner changed FROM portal
                if let Some(delegated) = self.find_undelegated_account(bank, pubkey) {
                    events.push(L1Event::AccountUndelegated {
                        delegation_record_pda: *pubkey,
                        delegated_account: delegated,
                    });
                }
            }
            _ => {}
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

        let runtime = EphemeralRuntime::new(
            root_bank,
            cluster_info,
            settings,
            rpc_port,
            self.config.portal_program_id,
        )
        .map_err(|e| {
            error!("Failed to create ephemeral runtime: {}", e);
            NorthStarError::RuntimeCreationFailed(e)
        })?;

        info!("Ephemeral rollup started on port {}", rpc_port);
        self.active_runtime = Some(runtime);
        Ok(())
    }
}
