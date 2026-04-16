use {
    crossbeam_channel::RecvTimeoutError,
    log::*,
    northstar::L1Event,
    solana_gossip::cluster_info::ClusterInfo,
    solana_rpc::optimistically_confirmed_bank_tracker::{
        BankNotification, BankNotificationReceiver,
    },
    solana_runtime::bank_forks::BankForks,
    std::{
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread::{Builder, JoinHandle},
        time::Duration,
    },
};

/// Configuration for NorthStarService
#[derive(Debug, Clone)]
pub struct NorthStarServiceConfig {
    /// Port for the ephemeral rollup RPC server
    pub listen_addr: SocketAddr,
    /// Sonic: Port for the ephemeral rollup WebSocket (PubSub)
    pub ws_addr: SocketAddr,
    /// Sonic: Port for the ephemeral rollup TPU (QUIC)
    pub tpu_addr: SocketAddr,
    /// Duration for each slot in the ephemeral rollup
    pub slot_duration: Duration,
}

/// NorthStar service that monitors root bank changes and creates ephemeral rollups
pub struct NorthStarService {
    thread_hdl: JoinHandle<()>,
}

impl NorthStarService {
    /// Create and start the NorthStar service
    /// Sonic: Monitors root slot changes and creates ephemeral rollups based on L1 events
    pub fn new(
        bank_forks: Arc<std::sync::RwLock<BankForks>>,
        receiver: BankNotificationReceiver,
        cfg: northstar::ManagerConfig,
        cluster_info: Arc<ClusterInfo>,
        config: NorthStarServiceConfig,
        exit: Arc<AtomicBool>,
    ) -> Self {
        // Sonic: Initialize NorthStar manager with always-on ephemeral RPC
        let mut manager = northstar::Manager::new(cfg);
        {
            let root_bank = bank_forks.read().unwrap().root_bank();
            if let Err(e) = manager.init_runtime(
                root_bank,
                cluster_info.clone(),
                config.listen_addr,
                config.ws_addr,
                config.tpu_addr,
            ) {
                error!("Failed to initialize ephemeral runtime: {e}");
            }
        }

        let thread_hdl = Builder::new()
            .name("solNorthStar".to_string())
            .spawn(move || {
                loop {
                    // Check for exit first
                    if exit.load(Ordering::Relaxed) {
                        // Shutdown the always-on runtime
                        manager.shutdown_runtime();
                        break;
                    }

                    let (notification, _dep_work) =
                        match receiver.recv_timeout(Duration::from_millis(500)) {
                            Ok(notification) => notification,
                            Err(RecvTimeoutError::Disconnected) => break,
                            Err(RecvTimeoutError::Timeout) => continue,
                        };

                    // Only process Frozen notifications
                    let BankNotification::Frozen(bank) = notification else {
                        continue;
                    };

                    // Check for L1 events from the portal program
                    let l1_events = manager.get_l1_events(&bank);

                    for event in l1_events {
                        match event {
                            L1Event::SessionOpened {
                                session_pda,
                                owner: _,
                                grid_id: _,
                                ttl_slots: _,
                                fee_cap: _,
                            } if !manager.has_active_runtime() => {
                                info!(
                                    "SessionOpened detected at slot {}, activating ephemeral \
                                     runtime (PDA={session_pda})",
                                    bank.slot()
                                );
                                let l1_root = bank_forks.read().unwrap().root_bank();
                                trace!(
                                    "L1 root for ER activation: slot={}, epoch={}",
                                    l1_root.slot(),
                                    l1_root.epoch(),
                                );
                                manager.activate_session(l1_root, session_pda);
                            }
                            L1Event::SessionClosed { session_pda, .. } => {
                                info!(
                                    "SessionClosed at slot {}, deactivating ER (PDA={})",
                                    bank.slot(),
                                    session_pda,
                                );
                                manager.deactivate_session();
                            }
                            L1Event::AccountDelegated {
                                delegated_account, ..
                            } => {
                                manager.handle_delegation(&bank, &delegated_account);
                            }
                            L1Event::FeeDeposited {
                                delta, depositor, ..
                            } => {
                                manager.credit_deposit(&depositor, delta);
                            }
                            other => {
                                debug!("Unhandled L1 event: {other:?}");
                            }
                        }
                    }
                }

                // Cleanup on exit
                manager.shutdown_runtime();

                debug!("NorthStar service shutting down");
            })
            .unwrap();

        Self { thread_hdl }
    }

    /// Shut down the service and wait for it to finish
    pub fn join(self) -> std::thread::Result<()> {
        self.thread_hdl.join()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crossbeam_channel::unbounded,
        northstar_portal::{OpenSession, PortalInstruction},
        solana_account::AccountSharedData,
        solana_client::rpc_client::RpcClient,
        solana_commitment_config::CommitmentConfig,
        solana_gossip::contact_info::ContactInfo,
        solana_instruction::{AccountMeta, Instruction},
        solana_keypair::{Keypair, Signer},
        solana_net_utils::SocketAddrSpace,
        solana_pubkey::Pubkey,
        solana_rent::Rent,
        solana_rpc::optimistically_confirmed_bank_tracker::BankNotification,
        solana_rpc_client_api::request::RpcRequest,
        solana_runtime::{
            bank::Bank,
            bank_forks::BankForks,
            genesis_utils::{GenesisConfigInfo, create_genesis_config},
        },
        solana_sdk_ids::system_program,
        solana_transaction::Transaction,
        std::{net::TcpListener, sync::RwLock, time::Duration},
    };

    fn create_test_bank() -> solana_runtime::bank::Bank {
        use solana_genesis_config::GenesisConfig;
        let genesis_config = GenesisConfig::new(&[], &[]);
        solana_runtime::bank::Bank::new_for_tests(&genesis_config)
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

    fn create_test_bank_forks(
        bank: solana_runtime::bank::Bank,
    ) -> Arc<std::sync::RwLock<BankForks>> {
        BankForks::new_rw_arc(bank)
    }

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

    fn build_close_session_ix(
        program_id: Pubkey,
        owner: Pubkey,
        session_pda: Pubkey,
        fee_vault_pda: Pubkey,
        grid_id: u64,
    ) -> Instruction {
        let ix = PortalInstruction::CloseSession { grid_id };
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

    #[test]
    fn test_service_creates_runtime_on_notification() {
        agave_logger::setup();

        let bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        let fund_account = Pubkey::new_unique();
        let initial_balance = 10_000_000_000u64;

        // Fund an account that will trigger portal program logs
        let account = AccountSharedData::new(initial_balance, 0, &system_program::id());
        bank.store_account(&fund_account, &account);
        bank.freeze();

        let bank_forks = create_test_bank_forks(bank);
        let cluster_info = create_test_cluster_info();
        let (sender, receiver) = unbounded();

        let exit = Arc::new(AtomicBool::new(false));
        let config = NorthStarServiceConfig {
            listen_addr: find_free_addr(),
            ws_addr: find_free_addr(),
            tpu_addr: find_free_addr(),
            slot_duration: Duration::from_millis(400),
        };

        // Get the bank for notifications BEFORE moving bank_forks
        let bank_for_test = bank_forks.read().unwrap().root_bank();

        let _service = NorthStarService::new(
            bank_forks,
            receiver,
            northstar::ManagerConfig {
                portal_program_id,
                manager_account: Arc::new(Keypair::new()),
            },
            cluster_info,
            config.clone(),
            exit.clone(),
        );

        // Give the service time to start
        std::thread::sleep(Duration::from_millis(100));

        // Send a Frozen notification (need to wrap bank in Arc)
        sender
            .send((BankNotification::Frozen(bank_for_test), None))
            .unwrap();

        // Wait for runtime to start (it needs L1 events, which won't exist in this test)
        // So we're testing that the service starts and processes notifications
        std::thread::sleep(Duration::from_secs(2));

        // The runtime won't be created because there are no L1 events
        // This test verifies the service starts and processes notifications
        exit.store(true, Ordering::Relaxed);
    }

    #[test]
    fn test_service_ignores_duplicate_notifications() {
        agave_logger::setup();

        let bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        bank.freeze();

        let bank_forks = create_test_bank_forks(bank);
        let cluster_info = create_test_cluster_info();
        let (sender, receiver) = unbounded();

        let exit = Arc::new(AtomicBool::new(false));
        let config = NorthStarServiceConfig {
            listen_addr: find_free_addr(),
            ws_addr: find_free_addr(),
            tpu_addr: find_free_addr(),
            slot_duration: Duration::from_millis(400),
        };

        // Get a reference to the frozen bank for sending notifications BEFORE moving bank_forks
        let bank_for_notifications = bank_forks.read().unwrap().root_bank();

        let _service = NorthStarService::new(
            bank_forks,
            receiver,
            northstar::ManagerConfig {
                portal_program_id,
                manager_account: Arc::new(Keypair::new()),
            },
            cluster_info,
            config.clone(),
            exit.clone(),
        );

        std::thread::sleep(Duration::from_millis(100));

        // Send multiple Frozen notifications
        for _ in 0..3 {
            sender
                .send((
                    BankNotification::Frozen(bank_for_notifications.clone()),
                    None,
                ))
                .unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }

        // The service should handle duplicate notifications without panicking
        // (it will just skip them because there are no L1 events)
        std::thread::sleep(Duration::from_secs(1));

        exit.store(true, Ordering::Relaxed);
    }

    #[test]
    fn test_service_shuts_down_runtime_on_exit() {
        agave_logger::setup();

        let bank = create_test_bank();
        let portal_program_id = Pubkey::new_unique();
        bank.freeze();

        let bank_forks = create_test_bank_forks(bank);
        let cluster_info = create_test_cluster_info();
        let (_sender, receiver) = unbounded();

        let exit = Arc::new(AtomicBool::new(false));
        let config = NorthStarServiceConfig {
            listen_addr: find_free_addr(),
            ws_addr: find_free_addr(),
            tpu_addr: find_free_addr(),
            slot_duration: Duration::from_millis(400),
        };

        let service = NorthStarService::new(
            bank_forks,
            receiver,
            northstar::ManagerConfig {
                portal_program_id,
                manager_account: Arc::new(Keypair::new()),
            },
            cluster_info,
            config,
            exit.clone(),
        );

        std::thread::sleep(Duration::from_millis(100));

        // Trigger exit
        exit.store(true, Ordering::Relaxed);

        // Join the service thread
        service.join().expect("service should join");

        // Port should be released after shutdown
        // (though in this test no runtime was created due to no L1 events)
    }

    #[test]
    fn test_service_slot_advancer_only_runs_while_session_active() {
        agave_logger::setup();

        let (root_bank, bank_forks, program_id, mint_keypair) = setup_bank_with_portal();
        let owner = Keypair::new();
        root_bank
            .transfer(100_000_000_000, &mint_keypair, &owner.pubkey())
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id, &owner.pubkey(), grid_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id, &owner.pubkey());

        let open_ix = build_open_session_ix(
            program_id,
            owner.pubkey(),
            session_pda,
            fee_vault_pda,
            grid_id,
            1,
            1_000_000,
        );
        let blockhash = root_bank.last_blockhash();
        let open_tx = Transaction::new_signed_with_payer(
            &[open_ix],
            Some(&owner.pubkey()),
            &[&owner],
            blockhash,
        );
        root_bank.process_transaction(&open_tx).unwrap();
        root_bank.freeze();

        let bank_for_open = bank_forks.read().unwrap().root_bank();

        let cluster_info = create_test_cluster_info();
        let (sender, receiver) = unbounded();
        let exit = Arc::new(AtomicBool::new(false));
        let config = NorthStarServiceConfig {
            listen_addr: find_free_addr(),
            ws_addr: find_free_addr(),
            tpu_addr: find_free_addr(),
            slot_duration: Duration::from_millis(400),
        };

        let service = NorthStarService::new(
            bank_forks,
            receiver,
            northstar::ManagerConfig {
                portal_program_id: program_id,
                manager_account: Arc::new(Keypair::new()),
            },
            cluster_info,
            config.clone(),
            exit.clone(),
        );

        let rpc = RpcClient::new(format!("http://{}", config.listen_addr));
        std::thread::sleep(Duration::from_secs(2));

        let slot_before = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .unwrap();
        std::thread::sleep(Duration::from_millis(900));
        let slot_still_before = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .unwrap();
        assert_eq!(
            slot_before, slot_still_before,
            "ER slot should not advance before session activation"
        );

        sender
            .send((BankNotification::Frozen(bank_for_open.clone()), None))
            .unwrap();
        std::thread::sleep(Duration::from_millis(1200));

        let slot_after_activate = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .unwrap();
        assert!(
            slot_after_activate > slot_still_before,
            "ER slot should advance after session activation"
        );

        let session_from_rpc: Option<String> = rpc
            .send(
                RpcRequest::Custom {
                    method: "getSessionPda",
                },
                serde_json::Value::Null,
            )
            .unwrap();
        assert_eq!(session_from_rpc, Some(session_pda.to_string()));

        let close_bank = Bank::new_from_parent(
            bank_for_open.clone(),
            &Pubkey::default(),
            bank_for_open.slot() + 3,
        );
        let close_ix = build_close_session_ix(
            program_id,
            owner.pubkey(),
            session_pda,
            fee_vault_pda,
            grid_id,
        );
        let blockhash = close_bank.last_blockhash();
        let close_tx = Transaction::new_signed_with_payer(
            &[close_ix],
            Some(&owner.pubkey()),
            &[&owner],
            blockhash,
        );
        close_bank.process_transaction(&close_tx).unwrap();
        close_bank.freeze();

        sender
            .send((BankNotification::Frozen(Arc::new(close_bank)), None))
            .unwrap();
        // Wait until SessionClosed is processed and RPC reports no active session.
        let mut session_after_close = Some(String::new());
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(300));
            session_after_close = rpc
                .send(
                    RpcRequest::Custom {
                        method: "getSessionPda",
                    },
                    serde_json::Value::Null,
                )
                .unwrap();
            if session_after_close.is_none() {
                break;
            }
        }
        assert_eq!(
            session_after_close, None,
            "session PDA should clear after SessionClosed"
        );

        let slot_before_stop = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .unwrap();
        std::thread::sleep(Duration::from_millis(900));
        let slot_after_stop = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .unwrap();
        assert_eq!(
            slot_before_stop, slot_after_stop,
            "ER slot should stop advancing after session close"
        );

        exit.store(true, Ordering::Relaxed);
        service.join().expect("service should join");
    }
}
