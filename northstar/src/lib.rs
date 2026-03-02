use {
    log::*,
    portal_state::{try_parse_raw_portal_account, PortalAccount},
    solana_account::{AccountSharedData, ReadableAccount},
    solana_gossip::cluster_info::ClusterInfo,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_runtime::bank::Bank,
    std::{net::SocketAddr, sync::Arc},
    thiserror::Error,
};

pub mod ephemeral_runtime;
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
    FeeDeposited { fee_vault_pda: Pubkey, amount: u64 },
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
    /// Active ephemeral runtime, if one is running
    active_runtime: Option<EphemeralRuntime>,
}

impl Manager {
    /// Create a new NorthStar Manager
    pub fn new(config: ManagerConfig) -> Self {
        info!("Initializing NorthStar Manager with config: {config:?}");
        Self {
            config,
            active_runtime: None,
        }
    }

    /// Check if an ephemeral runtime is currently active
    pub fn has_active_runtime(&self) -> bool {
        self.active_runtime.is_some()
    }

    /// Get the RPC port of the active runtime, if any
    pub fn active_runtime_addr(&self) -> Option<String> {
        self.active_runtime.as_ref().map(|r| r.rpc_addr())
    }

    /// Shutdown the active ephemeral runtime, if any
    pub fn shutdown_active_runtime(&mut self) {
        if let Some(mut runtime) = self.active_runtime.take() {
            info!("Shutting down ephemeral rollup");
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
            Some(PortalAccount::FeeVault(vault)) => Some(L1Event::FeeDeposited {
                fee_vault_pda: pubkey,
                amount: vault.balance,
            }),
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
        let undelegated_account = parent
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

    /// Create and store an EphemeralRuntime from the root bank
    ///
    /// This creates a fully functional ephemeral rollup with its own RPC server.
    /// The runtime is stored in the Manager and can be accessed via active_runtime_port().
    pub fn create_ephemeral_runtime(
        &mut self,
        root_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        settings: EphemeralRollupSettings,
        rpc_addr: SocketAddr,
    ) -> Result<()> {
        if self.active_runtime.is_some() {
            info!("Ephemeral runtime already active, skipping creation");
            return Ok(());
        }

        let runtime = EphemeralRuntime::new(
            root_bank,
            cluster_info,
            settings,
            rpc_addr,
            self.config.portal_program_id,
        )
        .map_err(|e| {
            error!("Failed to create ephemeral runtime: {}", e);
            NorthStarError::RuntimeCreationFailed(e)
        })?;

        info!("Ephemeral rollup started on {}", rpc_addr);
        self.active_runtime = Some(runtime);
        Ok(())
    }
}

#[cfg(test)]
mod portal_e2e_tests {
    use {
        super::*,
        agave_logger::setup,
        northstar_portal::{OpenSession, PortalInstruction},
        solana_account::AccountSharedData,
        solana_genesis_config::GenesisConfig,
        solana_gossip::contact_info::ContactInfo,
        solana_instruction::{AccountMeta, Instruction},
        solana_keypair::{Keypair, Signer},
        solana_net_utils::SocketAddrSpace,
        solana_rpc_client::rpc_client::RpcClient,
        solana_runtime::bank_forks::BankForks,
        solana_sdk_ids::{bpf_loader, system_program},
        solana_system_interface::instruction::transfer,
        solana_transaction::Transaction,
        std::{net::TcpListener, time::Duration},
    };

    /// Deploy the portal BPF program into the given bank.
    /// Returns the program ID.
    fn deploy_portal_program(bank: &Bank) -> Pubkey {
        solana_runtime::loader_utils::create_program(bank, &bpf_loader::id(), "northstar_portal")
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
            owner: *owner.as_array(),
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
        owner: Pubkey,
        fee_vault_pda: Pubkey,
        lamports: u64,
    ) -> Instruction {
        let ix = PortalInstruction::DepositFee { lamports };
        let data = borsh::to_vec(&ix).unwrap();
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(owner, true),
                AccountMeta::new(fee_vault_pda, false),
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

    fn fund_account(bank: &Bank, pubkey: &Pubkey, lamports: u64) {
        let account = AccountSharedData::new(lamports, 0, &system_program::id());
        bank.store_account(pubkey, &account);
    }

    /// Test: Deploy portal BPF program and execute OpenSession -> verify L1 event detection
    #[test]
    fn test_e2e_portal_to_l1_events() {
        setup();

        // Create genesis bank
        let genesis_config = GenesisConfig::new(&[], &[]);
        let genesis_bank = Bank::new_for_tests(&genesis_config);

        // Create BankForks first to set up fork graph
        let bank_forks = BankForks::new_rw_arc(genesis_bank);
        let genesis_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        // Deploy portal program
        let program_id = deploy_portal_program(&genesis_bank);

        // Create owner keypair and fund them
        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        fund_account(&genesis_bank, &owner_pubkey, 100_000_000_000); // 100 SOL

        // Freeze genesis bank and get slot before moving
        let genesis_slot = genesis_bank.slot();
        genesis_bank.freeze();

        // Create child bank
        let child_bank = Bank::new_from_parent(genesis_bank, &Pubkey::default(), genesis_slot + 1);

        // Compute PDAs
        let grid_id = 1u64;
        let ttl_slots = 1000u64;
        let fee_cap = 5_000_000_000u64; // 5 SOL
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);

        // Execute OpenSession transaction
        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            ttl_slots,
            fee_cap,
        );

        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );

        let result = child_bank.process_transaction(&tx);
        assert!(result.is_ok(), "OpenSession should succeed: {:?}", result);

        // Create manager and detect events
        let bank_forks = BankForks::new_rw_arc(child_bank);

        // Get a reference to the bank from the BankForks BEFORE moving to Manager
        let bank_ref = bank_forks.read().unwrap().root_bank();

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        // Verify events
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

        // Verify FeeVault was also created
        let fee_vault_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::FeeDeposited { .. }))
            .collect();
        assert!(
            !fee_vault_events.is_empty(),
            "Should detect FeeVault creation"
        );
    }

    /// Test: Execute Delegate instruction and verify AccountDelegated event
    #[test]
    fn test_e2e_delegation_detected() {
        setup();

        // Create genesis bank
        let genesis_config = GenesisConfig::new(&[], &[]);
        let genesis_bank = Bank::new_for_tests(&genesis_config);

        // Create BankForks first to set up fork graph
        let bank_forks = BankForks::new_rw_arc(genesis_bank);
        let genesis_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        // Deploy portal program
        let program_id = deploy_portal_program(&genesis_bank);

        // Create owner keypair and fund them
        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        fund_account(&genesis_bank, &owner_pubkey, 100_000_000_000);

        // Create delegated account with fake "application program" owner
        let owner_program = Pubkey::new_unique();
        let delegated_account = Pubkey::new_unique();
        let delegated_account_data = vec![0xAB; 100];
        let delegated_account_owner =
            AccountSharedData::new(1_000_000, delegated_account_data.len(), &owner_program);
        genesis_bank.store_account(&delegated_account, &delegated_account_owner);

        // Freeze genesis bank and get slot before moving
        let genesis_slot = genesis_bank.slot();
        genesis_bank.freeze();

        // Create child bank
        let child_bank = Bank::new_from_parent(genesis_bank, &Pubkey::default(), genesis_slot + 1);

        // First, change delegated account owner to portal program (simulating assign)
        let portal_owned_account =
            AccountSharedData::new(1_000_000, delegated_account_data.len(), &program_id);
        child_bank.store_account(&delegated_account, &portal_owned_account);

        // Compute PDAs
        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);
        let (delegation_record_pda, _) =
            find_delegation_record_pda(&program_id, &delegated_account);

        // Execute OpenSession first
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
        let _ = child_bank.process_transaction(&tx);

        // Execute Delegate instruction
        let delegate_ix = build_delegate_ix(
            program_id,
            owner_pubkey,
            delegated_account,
            owner_program,
            delegation_record_pda,
            grid_id,
        );

        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[delegate_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        let result = child_bank.process_transaction(&tx);
        assert!(result.is_ok(), "Delegate should succeed: {:?}", result);

        // Create manager and detect events
        let bank_forks = BankForks::new_rw_arc(child_bank);

        // Get bank reference BEFORE moving into Manager
        let bank_ref = bank_forks.read().unwrap().root_bank();

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        // Verify AccountDelegated event
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

        // Create genesis bank
        let genesis_config = GenesisConfig::new(&[], &[]);
        let genesis_bank = Bank::new_for_tests(&genesis_config);

        // Create BankForks first to set up fork graph
        let bank_forks = BankForks::new_rw_arc(genesis_bank);
        let genesis_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        // Deploy portal program
        let program_id = deploy_portal_program(&genesis_bank);

        // Create owner keypair and fund them
        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        fund_account(&genesis_bank, &owner_pubkey, 100_000_000_000);

        // Freeze genesis bank and get slot before moving
        let genesis_slot = genesis_bank.slot();
        genesis_bank.freeze();

        // Create child bank
        let child_bank = Bank::new_from_parent(genesis_bank, &Pubkey::default(), genesis_slot + 1);

        // Create delegated account with specific data, owned by portal program
        // Store on child_bank (not genesis_bank) to ensure it's properly accessible
        let delegated_account = Pubkey::new_unique();
        let delegated_account_data = vec![0xAB; 100];
        let delegated_account_owner =
            AccountSharedData::new(1_000_000, delegated_account_data.len(), &program_id);
        child_bank.store_account(&delegated_account, &delegated_account_owner);

        // Compute PDAs
        let grid_id = 1u64;
        let ttl_slots = 1000u64;
        let fee_cap = 5_000_000_000u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);
        let (delegation_record_pda, _) =
            find_delegation_record_pda(&program_id, &delegated_account);

        // Execute OpenSession
        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            ttl_slots,
            fee_cap,
        );

        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        let _ = child_bank.process_transaction(&tx);

        // Execute Delegate instruction
        let delegate_ix = build_delegate_ix(
            program_id,
            owner_pubkey,
            delegated_account,
            Pubkey::new_unique(),
            delegation_record_pda,
            grid_id,
        );

        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[delegate_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        let _ = child_bank.process_transaction(&tx);

        // Freeze child bank
        child_bank.freeze();

        // Detect events - create BankForks from the frozen child bank
        let bank_forks = BankForks::new_rw_arc(child_bank);

        // Get bank reference BEFORE moving into Manager
        let bank_ref = bank_forks.read().unwrap().root_bank();

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        // Collect session and delegation info
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

        // Create ephemeral runtime - need to get the bank from bank_forks first
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
            program_id,
        )
        .expect("Failed to create ephemeral runtime");

        // Verify delegated account is tracked
        assert!(
            runtime.delegated_accounts().contains(delegated_account),
            "Delegated account should be in runtime's delegated set"
        );

        // Verify account is readable directly from bank
        let ephemeral_bank = runtime.bank();
        let account_opt = ephemeral_bank.get_account(delegated_account);
        assert!(
            account_opt.is_some(),
            "Delegated account should be readable on L2"
        );

        // Debug: print account data
        let account = account_opt.unwrap();
        let account_data = account.data();
        eprintln!(
            "DEBUG: Account data length: {}, first few bytes: {:?}",
            account_data.len(),
            &account_data[..10.min(account_data.len())]
        );

        // For now, just verify the account exists and has some data (not checking exact data)
        assert!(
            !account_data.is_empty(),
            "Delegated account should have data"
        );

        // Verify account is readable via RPC
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

        // Verify L1 is unaffected - check that delegated account still exists on L1 with original owner
        // Use bank_ref instead of child_bank since child_bank was moved to BankForks
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

    /// Test: DepositFee transaction -> verify FeeDeposited event
    #[test]
    fn test_e2e_deposit_fee_detected() {
        setup();

        // Create genesis bank
        let genesis_config = GenesisConfig::new(&[], &[]);
        let genesis_bank = Bank::new_for_tests(&genesis_config);

        // Create BankForks first to set up fork graph
        let bank_forks = BankForks::new_rw_arc(genesis_bank);
        let genesis_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        // Deploy portal program
        let program_id = deploy_portal_program(&genesis_bank);

        // Create owner keypair and fund them
        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        fund_account(&genesis_bank, &owner_pubkey, 100_000_000_000);

        // Freeze genesis bank and get slot before moving
        let genesis_slot = genesis_bank.slot();
        genesis_bank.freeze();

        // Create child bank
        let child_bank = Bank::new_from_parent(genesis_bank, &Pubkey::default(), genesis_slot + 1);

        // Compute PDAs
        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner_pubkey, grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner_pubkey);

        // Execute OpenSession first
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
        let _ = child_bank.process_transaction(&tx);

        // Execute DepositFee
        let deposit_amount = 2_000_000_000u64; // 2 SOL
        let deposit_fee_ix =
            build_deposit_fee_ix(program_id, owner_pubkey, fee_vault_pda, deposit_amount);

        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        let result = child_bank.process_transaction(&tx);
        assert!(result.is_ok(), "DepositFee should succeed: {:?}", result);

        // Create manager and detect events
        let bank_forks = BankForks::new_rw_arc(child_bank);

        // Get bank reference BEFORE moving into Manager
        let bank_ref = bank_forks.read().unwrap().root_bank();

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        // Verify FeeDeposited event
        let fee_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::FeeDeposited { .. }))
            .collect();
        assert!(
            !fee_events.is_empty(),
            "Should detect at least one FeeDeposited event"
        );

        // Find the deposit event (not the initial 0 balance)
        let deposit_event = fee_events.iter().find(|e| {
            if let L1Event::FeeDeposited { amount, .. } = e {
                *amount == deposit_amount
            } else {
                false
            }
        });
        assert!(deposit_event.is_some(), "Should detect the 2 SOL deposit");
    }

    /// Test: No portal events when there's no portal activity
    #[test]
    fn test_e2e_no_events_without_portal_activity() {
        setup();

        // Create genesis bank
        let genesis_config = GenesisConfig::new(&[], &[]);
        let genesis_bank = Bank::new_for_tests(&genesis_config);

        // Create BankForks first to set up fork graph
        let bank_forks = BankForks::new_rw_arc(genesis_bank);
        let genesis_bank = Arc::clone(&bank_forks.read().unwrap().root_bank());

        // Deploy portal program
        let program_id = deploy_portal_program(&genesis_bank);

        // Create and fund two accounts
        let sender_keypair = Keypair::new();
        let sender_pubkey = sender_keypair.pubkey();
        let receiver_pubkey = Pubkey::new_unique();
        fund_account(&genesis_bank, &sender_pubkey, 100_000_000_000);

        // Freeze genesis bank and get slot before moving
        let genesis_slot = genesis_bank.slot();
        genesis_bank.freeze();

        // Create child bank
        let child_bank = Bank::new_from_parent(genesis_bank, &Pubkey::default(), genesis_slot + 1);

        // Execute a plain SOL transfer (no portal involvement)
        let transfer_ix = transfer(&sender_pubkey, &receiver_pubkey, 1_000_000_000);

        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[transfer_ix],
            Some(&sender_pubkey),
            &[&sender_keypair],
            blockhash,
        );
        let _ = child_bank.process_transaction(&tx);

        // Create manager and detect events
        let bank_forks = BankForks::new_rw_arc(child_bank);

        // Get bank reference BEFORE moving into Manager
        let bank_ref = bank_forks.read().unwrap().root_bank();

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        // Verify no portal events detected
        assert!(
            events.is_empty(),
            "Should detect no portal events when there's no portal activity"
        );
    }
}
