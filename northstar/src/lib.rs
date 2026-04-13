use {
    log::*,
    portal_state::{PortalAccount, try_parse_raw_portal_account},
    solana_account::{AccountSharedData, ReadableAccount},
    solana_gossip::cluster_info::ClusterInfo,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_runtime::bank::Bank,
    std::{net::SocketAddr, sync::Arc},
    thiserror::Error,
};

pub mod ephemeral_runtime;
pub mod ephemeral_tpu;
pub mod ephemeral_tx_client;
pub mod portal_state;
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
    FeeDeposited {
        session_pda: Pubkey,
        /// Total vault balance after the deposit
        amount: u64,
        /// Deposit amount this slot (current - parent balance)
        delta: u64,
        /// Who gets credited on L2
        depositor: Pubkey,
    },
}

/// Configuration for NorthStar Manager
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Portal program ID (to read L1 state from)
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
    /// Sonic: Always-on ephemeral runtime. Created once at startup via
    /// `init_runtime()`, stays alive for the validator's lifetime.
    /// The `active` flag inside gates transaction acceptance.
    runtime: Option<EphemeralRuntime>,
}

impl Manager {
    /// Create a new NorthStar Manager
    pub fn new(config: ManagerConfig) -> Self {
        info!("Initializing NorthStar Manager with config: {config:?}");
        Self {
            config,
            runtime: None,
        }
    }

    /// Sonic: Check if an ephemeral session is currently active (accepting transactions)
    pub fn has_active_runtime(&self) -> bool {
        self.runtime.as_ref().is_some_and(|r| r.is_active())
    }

    /// Sonic: Check if the always-on runtime has been initialized
    pub fn has_runtime(&self) -> bool {
        self.runtime.is_some()
    }

    /// Get the RPC address of the runtime, if initialized
    pub fn runtime_addr(&self) -> Option<String> {
        self.runtime.as_ref().map(|r| r.rpc_addr())
    }

    /// Get the WebSocket address of the runtime, if initialized
    pub fn runtime_ws_addr(&self) -> Option<String> {
        self.runtime.as_ref().map(|r| r.ws_addr())
    }

    /// Sonic: Shutdown the always-on runtime (called at validator exit)
    pub fn shutdown_runtime(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            info!("Shutting down ephemeral rollup runtime");
            runtime.shutdown();
        }
    }

    fn parse_event(
        &self,
        bank: &Bank,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) -> Option<L1Event> {
        let data = account.data();
        // Account was zeroed — determine type from previous state
        if data.iter().all(|b| *b == 0) {
            return self.parse_zeroed_account(bank, &pubkey);
        }

        match try_parse_raw_portal_account(data) {
            // Check if this is a new session (didn't exist in parent)
            Some(PortalAccount::Session(session)) => {
                self.is_new_in_slot(bank, &pubkey)
                    .then_some(L1Event::SessionOpened {
                        session_pda: pubkey,
                        owner: session.owner.into(),
                        grid_id: session.grid_id,
                        ttl_slots: session.ttl_slots,
                        fee_cap: session.fee_cap,
                    })
            }
            Some(PortalAccount::DelegationRecord(_)) if !self.is_new_in_slot(bank, &pubkey) => None,
            Some(PortalAccount::DelegationRecord(record)) => self
                .find_delegated_account(bank, &pubkey)
                .map(|delegated| L1Event::AccountDelegated {
                    delegation_record_pda: pubkey,
                    delegated_account: delegated,
                    owner_program: record.owner_program.into(),
                    grid_id: record.grid_id,
                }),
            Some(PortalAccount::FeeVault(_vault)) => {
                // FeeVault balance tracking removed; events come from DepositReceipts
                None
            }
            Some(PortalAccount::DepositReceipt(receipt)) => {
                let prev_balance = bank
                    .parent()
                    .and_then(|parent| parent.get_account(&pubkey))
                    .and_then(|account| {
                        portal_state::try_parse_raw_portal_account(account.data()).and_then(|p| {
                            if let portal_state::PortalAccount::DepositReceipt(r) = p {
                                Some(r.balance)
                            } else {
                                None
                            }
                        })
                    })
                    .unwrap_or(0);

                let delta = receipt.balance.saturating_sub(prev_balance);
                if delta == 0 {
                    return None;
                }

                Some(L1Event::FeeDeposited {
                    session_pda: receipt.session.into(),
                    amount: receipt.balance,
                    delta,
                    depositor: receipt.recipient.into(),
                })
            }
            None => {
                // Unrecognized — log and skip
                debug!("Unrecognized portal account at {pubkey}");
                None
            }
        }
    }

    pub fn get_l1_events(&self, bank: &Bank) -> Vec<L1Event> {
        bank.get_program_accounts_modified_since_parent(&self.config.portal_program_id)
            .into_iter()
            .filter_map(|(pubkey, account)| self.parse_event(bank, pubkey, account))
            .collect()
    }

    /// Check if an account existed in the parent bank
    fn is_new_in_slot(&self, bank: &Bank, pubkey: &Pubkey) -> bool {
        bank.parent()
            .map(|parent| parent.get_account(pubkey).is_none())
            .unwrap_or(true)
    }

    fn find_delegated_account(
        &self,
        bank: &Bank,
        delegation_record_pda: &Pubkey,
    ) -> Option<Pubkey> {
        let undelegated_account = bank
            .get_all_accounts_modified_since_parent()
            .into_iter()
            .filter(|(pubkey, _)| pubkey != delegation_record_pda)
            .filter(|(_, account)| account.owner() == &self.config.portal_program_id)
            .find_map(|(pubkey, _)| {
                // Verify PDA derivation
                let (expected_pda, _) = Pubkey::find_program_address(
                    &[b"delegation", pubkey.as_ref()],
                    &self.config.portal_program_id,
                );
                (&expected_pda == delegation_record_pda).then_some(pubkey)
            });

        if undelegated_account.is_none() {
            warn!(
                "Could not find undelegated account for delegation record {}",
                delegation_record_pda
            );
        }
        undelegated_account
    }

    fn find_undelegated_account(
        &self,
        bank: &Bank,
        delegation_record_pda: &Pubkey,
    ) -> Option<Pubkey> {
        let parent = bank.parent()?;
        let undelegated_account = bank
            .get_all_accounts_modified_since_parent()
            .into_iter()
            .filter(|(pubkey, _)| pubkey != delegation_record_pda)
            .filter(|(pubkey, account)| {
                // Check that account is not owned by portal now,
                // but was owned a block ago
                account.owner() != &self.config.portal_program_id
                    && parent
                        .get_account(pubkey)
                        .map(|a| a.owner() == &self.config.portal_program_id)
                        .unwrap_or_default()
            })
            .find_map(|(pubkey, _)| {
                // Verify PDA derivation
                let (expected_pda, _) = Pubkey::find_program_address(
                    &[b"delegation", pubkey.as_ref()],
                    &self.config.portal_program_id,
                );
                (&expected_pda == delegation_record_pda).then_some(pubkey)
            });

        if undelegated_account.is_none() {
            warn!(
                "Could not find undelegated account for delegation record {}",
                delegation_record_pda
            );
        }
        undelegated_account
    }

    /// When an account's data is zeroed, determine what type it was from the parent bank
    fn parse_zeroed_account(&self, bank: &Bank, pubkey: &Pubkey) -> Option<L1Event> {
        let prev_account = bank.parent()?.get_account(pubkey)?;

        match try_parse_raw_portal_account(prev_account.data())? {
            PortalAccount::Session(session) => Some(L1Event::SessionClosed {
                session_pda: *pubkey,
                owner: session.owner.into(),
                grid_id: session.grid_id,
            }),
            // Find the delegated account that was undelegated
            // by scanning for accounts whose owner changed FROM portal
            PortalAccount::DelegationRecord(_record) => Some(L1Event::AccountUndelegated {
                delegation_record_pda: *pubkey,
                delegated_account: self.find_undelegated_account(bank, pubkey)?,
            }),
            _ => None,
        }
    }

    /// Sonic: Initialize the always-on ephemeral RPC runtime.
    /// Called once at validator startup. RPC starts listening immediately
    /// but rejects transactions until `activate_session()` is called.
    pub fn init_runtime(
        &mut self,
        root_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        rpc_addr: SocketAddr,
        ws_addr: SocketAddr,
        tpu_addr: SocketAddr,
    ) -> Result<()> {
        if self.runtime.is_some() {
            info!("Ephemeral runtime already initialized, skipping");
            return Ok(());
        }

        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::default(),
            owner: Pubkey::default(),
            grid_id: 0,
            ttl_slots: 0,
            fee_cap: 0,
            delegated_accounts: vec![],
        };

        let runtime = EphemeralRuntime::new(
            root_bank,
            cluster_info,
            settings,
            rpc_addr,
            ws_addr,
            tpu_addr,
            self.config.portal_program_id,
            self.config.manager_account.clone(),
        )
        .map_err(|e| {
            error!("Failed to create ephemeral runtime: {}", e);
            NorthStarError::RuntimeCreationFailed(e)
        })?;

        info!(
            "Always-on ephemeral RPC initialized at {rpc_addr}, WS at {ws_addr}, TPU at \
             {tpu_addr} (inactive)"
        );
        self.runtime = Some(runtime);
        Ok(())
    }

    /// Sonic: Activate the ephemeral session — resets bank to current L1 root
    /// and starts accepting transactions.
    pub fn activate_session(&mut self, root_bank: Arc<Bank>) {
        if let Some(runtime) = &mut self.runtime {
            runtime.reset_to_new_parent(root_bank);
            runtime.activate();
        } else {
            warn!("Cannot activate session: runtime not initialized");
        }
    }

    /// Sonic: Deactivate the ephemeral session — transactions will be rejected.
    pub fn deactivate_session(&mut self) {
        if let Some(runtime) = &self.runtime {
            runtime.deactivate();
        } else {
            warn!("Cannot deactivate session: runtime not initialized");
        }
    }

    /// Create and store an EphemeralRuntime from the root bank
    ///
    /// This creates a fully functional ephemeral rollup with its own RPC server.
    /// The runtime is stored in the Manager and can be accessed via runtime_addr().
    #[cfg(test)]
    pub fn create_ephemeral_runtime(
        &mut self,
        root_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        settings: EphemeralRollupSettings,
        rpc_addr: SocketAddr,
    ) -> Result<()> {
        if self.runtime.is_some() {
            info!("Ephemeral runtime already exists, skipping creation");
            return Ok(());
        }

        let runtime = EphemeralRuntime::new(
            root_bank,
            cluster_info,
            settings,
            rpc_addr,
            // Tests: no WS or TPU — use unbound addrs that won't be used
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
            self.config.portal_program_id,
            self.config.manager_account.clone(),
        )
        .map_err(|e| {
            error!("Failed to create ephemeral runtime: {}", e);
            NorthStarError::RuntimeCreationFailed(e)
        })?;

        info!("Ephemeral rollup started on {}", rpc_addr);
        runtime.activate();
        self.runtime = Some(runtime);
        Ok(())
    }

    /// Credit a deposit to a depositor's account on the ephemeral bank.
    /// Called by NorthStarService when a FeeDeposited event is detected on L1.
    /// Only processes when a session is active.
    pub fn credit_deposit(&self, depositor: &Pubkey, lamports: u64) {
        if let Some(runtime) = &self.runtime {
            if !runtime.is_active() {
                warn!("Ignoring deposit for {depositor}: no active session");
                return;
            }
            runtime.credit_deposit(depositor, lamports);
        }
    }

    /// Handle a new account delegation from L1.
    /// Called by NorthStarService when an AccountDelegated event is detected on L1.
    /// Copies the account data from L1 into the ephemeral bank and adds it to
    /// the delegated set so transactions are allowed to write to it.
    /// Only processes when a session is active.
    pub fn handle_delegation(&self, bank: &Bank, delegated_account: &Pubkey) {
        if let Some(runtime) = &self.runtime {
            if !runtime.is_active() {
                warn!("Ignoring delegation for {delegated_account}: no active session");
                return;
            }
            if let Some(account_data) = bank.get_account(delegated_account) {
                runtime.handle_delegation(delegated_account, account_data);
            } else {
                warn!(
                    "Cannot handle delegation: account {} not found on L1",
                    delegated_account
                );
            }
        }
    }
}

#[cfg(test)]
mod portal_e2e_tests {
    use {
        super::*,
        agave_logger::setup,
        northstar_portal::{OpenSession, PortalInstruction},
        solana_account::{AccountSharedData, WritableAccount},
        solana_gossip::contact_info::ContactInfo,
        solana_instruction::{AccountMeta, Instruction},
        solana_keypair::{Keypair, Signer},
        solana_net_utils::SocketAddrSpace,
        solana_rent::Rent,
        solana_rpc_client::rpc_client::RpcClient,
        solana_runtime::{
            bank_forks::BankForks,
            genesis_utils::{GenesisConfigInfo, create_genesis_config},
        },
        solana_sdk_ids::system_program,
        solana_system_interface::instruction::transfer,
        solana_transaction::Transaction,
        std::{net::TcpListener, sync::RwLock, time::Duration},
    };

    /// Set up a test bank with portal program in genesis.
    /// Returns (bank, bank_forks, program_id, mint_keypair).
    fn setup_bank_with_portal() -> (Arc<Bank>, Arc<RwLock<BankForks>>, Pubkey, Keypair) {
        let GenesisConfigInfo {
            mut genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(1_000_000_000_000);
        genesis_config.rent = Rent::default();

        let program_id = Pubkey::new_unique();
        let program_data = solana_runtime::loader_utils::load_program_from_file("northstar_portal");
        genesis_config.accounts.insert(
            program_id,
            solana_account::Account {
                lamports: genesis_config
                    .rent
                    .minimum_balance(program_data.len())
                    .max(1),
                data: program_data,
                owner: solana_sdk_ids::bpf_loader::id(),
                executable: true,
                rent_epoch: 0,
            },
        );

        let (bank, _) = Bank::new_with_bank_forks_for_tests(&genesis_config);
        bank.fill_bank_with_ticks_for_tests();
        let bank = Bank::new_from_parent(bank.clone(), bank.leader_id(), bank.slot() + 1);
        let bank_forks = BankForks::new_rw_arc(bank);
        let bank = Arc::clone(&bank_forks.read().unwrap().root_bank());
        (bank, bank_forks, program_id, mint_keypair)
    }

    fn find_session_pda(program_id: &Pubkey, owner: &Pubkey, grid_id: u64) -> (Pubkey, u8) {
        let grid_id_bytes = grid_id.to_le_bytes();
        Pubkey::find_program_address(&[b"session", owner.as_ref(), &grid_id_bytes], program_id)
    }

    fn find_fee_vault_pda(program_id: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"fee_vault", owner.as_ref()], program_id)
    }

    fn find_delegation_record_pda(program_id: &Pubkey, delegated_account: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"delegation", delegated_account.as_ref()], program_id)
    }

    fn find_deposit_receipt_pda(
        program_id: &Pubkey,
        session: &Pubkey,
        recipient: &Pubkey,
    ) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[b"deposit_receipt", session.as_ref(), recipient.as_ref()],
            program_id,
        )
    }

    fn build_open_session_ix(
        program_id: Pubkey,
        owner: Pubkey,
        session_pda: Pubkey,
        fee_vault_pda: Pubkey,
        grid_id: u64,
        ttl_slots: u64,
        fee_cap: u64,
    ) -> Instruction {
        let ix = PortalInstruction::OpenSession(OpenSession {
            grid_id,
            ttl_slots,
            fee_cap,
        });
        let data = borsh::to_vec(&ix).unwrap();
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(owner, true),
                AccountMeta::new(session_pda, false),
                AccountMeta::new(fee_vault_pda, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
        }
    }

    fn build_deposit_fee_ix(
        program_id: Pubkey,
        depositor: Pubkey,
        session_pda: Pubkey,
        recipient: Pubkey,
        lamports: u64,
    ) -> Instruction {
        let (deposit_receipt_pda, _) =
            find_deposit_receipt_pda(&program_id, &session_pda, &recipient);

        let ix = PortalInstruction::DepositFee { lamports };
        let data = borsh::to_vec(&ix).unwrap();
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(depositor, true),
                AccountMeta::new_readonly(session_pda, false),
                AccountMeta::new(deposit_receipt_pda, false),
                AccountMeta::new_readonly(recipient, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
        }
    }

    fn build_delegate_ix(
        program_id: Pubkey,
        payer: Pubkey,
        delegated_account: Pubkey,
        owner_program: Pubkey,
        delegation_record_pda: Pubkey,
        grid_id: u64,
    ) -> Instruction {
        let ix = PortalInstruction::Delegate { grid_id };
        let data = borsh::to_vec(&ix).unwrap();
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(delegated_account, false),
                AccountMeta::new_readonly(owner_program, false),
                AccountMeta::new(delegation_record_pda, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
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

    /// Test: Deploy portal BPF program and execute OpenSession -> verify L1 event detection
    #[test]
    fn test_e2e_portal_to_l1_events() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let ttl_slots = 1000u64;
        let fee_cap = 5_000_000_000u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            ttl_slots,
            fee_cap,
        );

        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );

        let result = bank.process_transaction(&tx);
        assert!(result.is_ok(), "OpenSession should succeed: {:?}", result);

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let session_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::SessionOpened { .. }))
            .collect();
        assert_eq!(
            session_events.len(),
            1,
            "Should detect exactly one SessionOpened event"
        );

        if let L1Event::SessionOpened {
            session_pda: _,
            owner,
            grid_id: detected_grid_id,
            ttl_slots: detected_ttl,
            fee_cap: detected_fee,
        } = session_events[0]
        {
            assert_eq!(*owner, owner_pubkey, "Owner should match");
            assert_eq!(*detected_grid_id, grid_id, "Grid ID should match");
            assert_eq!(*detected_ttl, ttl_slots, "TTL should match");
            assert_eq!(*detected_fee, fee_cap, "Fee cap should match");
        } else {
            panic!("Expected SessionOpened event");
        }
    }

    /// Test: Execute Delegate instruction and verify AccountDelegated event
    #[test]
    fn test_e2e_delegation_detected() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let owner_program = Pubkey::new_unique();
        let delegated_account = Pubkey::new_unique();
        let portal_owned_account = AccountSharedData::new(1_000_000, 100, &program_id);
        bank.store_account(&delegated_account, &portal_owned_account);

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);
        let (delegation_record_pda, _) =
            find_delegation_record_pda(&program_id, &delegated_account);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let delegate_ix = build_delegate_ix(
            program_id,
            owner_pubkey,
            delegated_account,
            owner_program,
            delegation_record_pda,
            grid_id,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[delegate_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        let result = bank.process_transaction(&tx);
        assert!(result.is_ok(), "Delegate should succeed: {:?}", result);

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let delegation_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::AccountDelegated { .. }))
            .collect();
        assert_eq!(
            delegation_events.len(),
            1,
            "Should detect exactly one AccountDelegated event"
        );

        if let L1Event::AccountDelegated {
            delegation_record_pda: _,
            delegated_account: detected_delegated,
            owner_program: detected_owner_program,
            grid_id: detected_grid_id,
        } = delegation_events[0]
        {
            assert_eq!(
                *detected_delegated, delegated_account,
                "Delegated account should match"
            );
            assert_eq!(
                *detected_owner_program, owner_program,
                "Owner program should match"
            );
            assert_eq!(*detected_grid_id, grid_id, "Grid ID should match");
        } else {
            panic!("Expected AccountDelegated event");
        }
    }

    /// Test: Full vertical slice - portal execution -> event detection -> ephemeral runtime -> account visibility
    #[test]
    fn test_e2e_delegated_account_visible_on_l2() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let delegated_account = Pubkey::new_unique();
        let delegated_account_data = vec![0xAB; 100];
        let mut delegated_account_owner =
            AccountSharedData::new(1_000_000, delegated_account_data.len(), &program_id);
        delegated_account_owner
            .data_as_mut_slice()
            .copy_from_slice(&delegated_account_data);
        bank.store_account(&delegated_account, &delegated_account_owner);

        let grid_id = 1u64;
        let ttl_slots = 1000u64;
        let fee_cap = 5_000_000_000u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);
        let (delegation_record_pda, _) =
            find_delegation_record_pda(&program_id, &delegated_account);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            ttl_slots,
            fee_cap,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let delegate_ix = build_delegate_ix(
            program_id,
            owner_pubkey,
            delegated_account,
            Pubkey::new_unique(),
            delegation_record_pda,
            grid_id,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[delegate_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();
        bank.freeze();

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let session_event = events
            .iter()
            .find(|e| matches!(e, L1Event::SessionOpened { .. }))
            .expect("Should have SessionOpened event");

        let L1Event::SessionOpened {
            session_pda,
            owner,
            grid_id,
            ttl_slots,
            fee_cap,
        } = session_event
        else {
            panic!("Expected SessionOpened");
        };

        let delegation_event = events
            .iter()
            .find(|e| matches!(e, L1Event::AccountDelegated { .. }))
            .expect("Should have AccountDelegated event");

        let L1Event::AccountDelegated {
            delegated_account, ..
        } = delegation_event
        else {
            panic!("Expected AccountDelegated");
        };

        let parent_bank = Arc::clone(&bank_ref);

        let settings = EphemeralRollupSettings {
            session_pda: *session_pda,
            owner: *owner,
            grid_id: *grid_id,
            ttl_slots: *ttl_slots,
            fee_cap: *fee_cap,
            delegated_accounts: vec![*delegated_account],
        };

        let cluster_info = create_test_cluster_info();
        let mut runtime = EphemeralRuntime::new(
            parent_bank,
            cluster_info,
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            program_id,
            Arc::new(Keypair::new()),
        )
        .expect("Failed to create ephemeral runtime");

        assert!(
            runtime.delegated_accounts().contains(delegated_account),
            "Delegated account should be in runtime's delegated set"
        );

        let ephemeral_bank = runtime.bank();
        let account_opt = ephemeral_bank.get_account(delegated_account);
        assert!(
            account_opt.is_some(),
            "Delegated account should be readable on L2"
        );

        let account = account_opt.unwrap();
        let account_data = account.data();
        eprintln!(
            "DEBUG: Account data length: {}, first few bytes: {:?}",
            account_data.len(),
            &account_data[..10.min(account_data.len())]
        );
        assert!(
            !account_data.is_empty(),
            "Delegated account should have data"
        );

        std::thread::sleep(Duration::from_secs(2));
        let rpc_client = RpcClient::new(runtime.rpc_addr());
        let rpc_account = rpc_client
            .get_account_data(delegated_account)
            .expect("Delegated account should be readable via RPC");
        eprintln!(
            "DEBUG: RPC account data length: {}, first few bytes: {:?}",
            rpc_account.len(),
            &rpc_account[..10.min(rpc_account.len())]
        );

        let l1_account = bank_ref.get_account(delegated_account);
        assert!(
            l1_account.is_some(),
            "Delegated account should still exist on L1"
        );
        assert_eq!(
            l1_account.unwrap().owner(),
            &program_id,
            "L1 account owner should still be portal program"
        );

        runtime.shutdown();
    }

    /// Test: handle_delegation adds a new delegated account to a running ER at runtime
    #[test]
    fn test_e2e_handle_delegation_at_runtime() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let delegated_account_pubkey = Pubkey::new_unique();
        let delegated_data = vec![0xDE; 64];
        let mut delegated_account =
            AccountSharedData::new(5_000_000_000, delegated_data.len(), &program_id);
        delegated_account
            .data_as_mut_slice()
            .copy_from_slice(&delegated_data);
        bank.store_account(&delegated_account_pubkey, &delegated_account);

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();
        bank.freeze();

        let parent_bank = bank;
        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda,
            owner: owner_pubkey,
            grid_id,
            ttl_slots: 1000,
            fee_cap: 5_000_000_000,
            delegated_accounts: vec![],
        };

        let mut runtime = EphemeralRuntime::new(
            parent_bank.clone(),
            cluster_info,
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            program_id,
            Arc::new(Keypair::new()),
        )
        .expect("Failed to create ephemeral runtime");

        assert!(
            !runtime
                .delegated_accounts()
                .contains(&delegated_account_pubkey),
            "Account should not be delegated yet"
        );

        let account_data = parent_bank
            .get_account(&delegated_account_pubkey)
            .expect("Account should exist on L1");
        runtime.handle_delegation(&delegated_account_pubkey, account_data.clone());

        assert!(
            runtime
                .delegated_accounts()
                .contains(&delegated_account_pubkey),
            "Account should be delegated after handle_delegation"
        );

        let er_bank = runtime.bank();
        let er_account = er_bank
            .get_account(&delegated_account_pubkey)
            .expect("Delegated account should be readable on ER");
        assert_eq!(
            er_account.data(),
            &delegated_data[..],
            "Account data should match L1 data"
        );
        assert_eq!(er_account.lamports(), 5_000_000_000);

        std::thread::sleep(Duration::from_secs(2));
        let rpc_client = RpcClient::new(runtime.rpc_addr());
        let rpc_balance = rpc_client
            .get_balance(&delegated_account_pubkey)
            .expect("Should be able to get balance via RPC");
        assert_eq!(rpc_balance, 5_000_000_000, "RPC balance should match");

        runtime.shutdown();
    }
    #[test]
    fn test_e2e_deposit_fee_detected() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let deposit_amount = 2_000_000_000u64;
        let deposit_fee_ix = build_deposit_fee_ix(
            program_id,
            owner_pubkey,
            session_pda,
            owner_pubkey,
            deposit_amount,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        let result = bank.process_transaction(&tx);
        assert!(result.is_ok(), "DepositFee should succeed: {:?}", result);

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let fee_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::FeeDeposited { .. }))
            .collect();
        assert!(
            !fee_events.is_empty(),
            "Should detect at least one FeeDeposited event"
        );

        let deposit_event = fee_events.iter().find(|e| {
            if let L1Event::FeeDeposited {
                delta, depositor, ..
            } = e
            {
                *delta == deposit_amount && *depositor == owner_pubkey
            } else {
                false
            }
        });
        assert!(
            deposit_event.is_some(),
            "Should detect the 2 SOL deposit with delta and depositor"
        );

        if let Some(L1Event::FeeDeposited {
            delta,
            depositor,
            amount,
            ..
        }) = deposit_event
        {
            assert_eq!(*delta, deposit_amount, "Delta should equal deposit amount");
            assert_eq!(
                *depositor, owner_pubkey,
                "Depositor should be the vault authority (owner)"
            );
            assert_eq!(
                *amount, deposit_amount,
                "Amount should be total vault balance"
            );
        }
    }

    /// Test: No portal events when there's no portal activity
    #[test]
    fn test_e2e_no_events_without_portal_activity() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let sender_keypair = Keypair::new();
        let sender_pubkey = sender_keypair.pubkey();
        let receiver_pubkey = Pubkey::new_unique();
        bank.transfer(100_000_000_000, &mint_keypair, &sender_pubkey)
            .unwrap();

        let transfer_ix = transfer(&sender_pubkey, &receiver_pubkey, 1_000_000_000);
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[transfer_ix],
            Some(&sender_pubkey),
            &[&sender_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        assert!(
            events.is_empty(),
            "Should detect no portal events when there's no portal activity"
        );
    }

    /// Test: Third party deposits to a FeeVault -> verify FeeDeposited event
    #[test]
    fn test_e2e_third_party_deposit_detected() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let depositor_keypair = Keypair::new();
        let depositor_pubkey = depositor_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &depositor_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let deposit_amount = 3_000_000_000u64;
        let deposit_fee_ix = build_deposit_fee_ix(
            program_id,
            depositor_pubkey,
            session_pda,
            depositor_pubkey,
            deposit_amount,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix],
            Some(&depositor_pubkey),
            &[&depositor_keypair],
            blockhash,
        );
        let result = bank.process_transaction(&tx);
        assert!(
            result.is_ok(),
            "Third party DepositFee should succeed: {:?}",
            result
        );

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let fee_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::FeeDeposited { .. }))
            .collect();
        assert!(
            !fee_events.is_empty(),
            "Should detect at least one FeeDeposited event"
        );

        let deposit_event = fee_events.iter().find(|e| {
            if let L1Event::FeeDeposited {
                delta, depositor, ..
            } = e
            {
                *delta == deposit_amount && *depositor == depositor_pubkey
            } else {
                false
            }
        });
        assert!(
            deposit_event.is_some(),
            "Should detect the 3 SOL third party deposit with correct delta and depositor"
        );

        if let Some(L1Event::FeeDeposited { delta, .. }) = deposit_event {
            assert_eq!(
                *delta, deposit_amount,
                "Delta should equal deposit amount (not cumulative)"
            );
        }
    }

    /// Test: Multiple deposits across slots - verify delta is incremental, not cumulative
    #[test]
    fn test_e2e_deposit_delta_computed_correctly() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let bank_slot = bank.slot();
        bank.freeze();
        let child_bank = Bank::new_from_parent(bank, &Pubkey::default(), bank_slot + 1);

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        child_bank.process_transaction(&tx).unwrap();

        let deposit1_amount = 2_000_000_000u64;
        let deposit_fee_ix1 = build_deposit_fee_ix(
            program_id,
            owner_pubkey,
            session_pda,
            owner_pubkey,
            deposit1_amount,
        );
        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix1],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        child_bank.process_transaction(&tx).unwrap();

        child_bank.freeze();
        let child_bank =
            Bank::new_from_parent(Arc::new(child_bank), &Pubkey::default(), bank_slot + 2);

        let deposit2_amount = 3_000_000_000u64;
        let deposit_fee_ix2 = build_deposit_fee_ix(
            program_id,
            owner_pubkey,
            session_pda,
            owner_pubkey,
            deposit2_amount,
        );
        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix2],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        child_bank.process_transaction(&tx).unwrap();

        let bank_forks = BankForks::new_rw_arc(child_bank);
        let bank_ref = bank_forks.read().unwrap().root_bank();

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let fee_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::FeeDeposited { .. }))
            .collect();

        assert!(
            !fee_events.is_empty(),
            "Should detect at least one FeeDeposited event"
        );

        let second_deposit_event = fee_events.iter().find(|e| {
            if let L1Event::FeeDeposited { delta, amount, .. } = e {
                *delta == deposit2_amount && *amount == (deposit1_amount + deposit2_amount)
            } else {
                false
            }
        });

        assert!(
            second_deposit_event.is_some(),
            "Should detect delta as 3 SOL (not 5 SOL cumulative)"
        );
    }
}
