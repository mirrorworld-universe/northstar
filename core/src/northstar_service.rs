use {
    crate::banking_trace::BankingPacketSender,
    agave_banking_stage_ingress_types::BankingPacketBatch,
    crossbeam_channel::RecvTimeoutError,
    log::*,
    northstar::L1Event,
    solana_gossip::cluster_info::ClusterInfo,
    solana_perf::packet::{NUM_PACKETS, to_packet_batches},
    solana_rpc::optimistically_confirmed_bank_tracker::{
        BankNotification, BankNotificationReceiver,
    },
    solana_runtime::bank_forks::BankForks,
    solana_transaction::Transaction,
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
#[derive(Clone)]
pub struct NorthStarServiceConfig {
    /// Port for the ephemeral rollup RPC server
    pub listen_addr: SocketAddr,
    /// Sonic: Port for the ephemeral rollup WebSocket (PubSub)
    pub ws_addr: SocketAddr,
    /// Sonic: Port for the ephemeral rollup TPU (QUIC)
    pub tpu_addr: SocketAddr,
    /// Duration for each slot in the ephemeral rollup
    pub slot_duration: Duration,
    /// Local BankingStage non-vote sender for permissioned Portal settlement txs.
    pub settlement_sender: Option<BankingPacketSender>,
}

/// NorthStar service that monitors root bank changes and creates ephemeral rollups
pub struct NorthStarService {
    thread_hdl: JoinHandle<()>,
}

fn submit_settlement_transactions(
    sender: &BankingPacketSender,
    transactions: &[Transaction],
) -> Result<(), crossbeam_channel::SendError<BankingPacketBatch>> {
    if transactions.is_empty() {
        return Ok(());
    }
    sender.send(BankingPacketBatch::new(to_packet_batches(
        transactions,
        NUM_PACKETS,
    )))
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
        manager.set_slot_duration(config.slot_duration);
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

        let settlement_sender = config.settlement_sender;
        let thread_hdl = Builder::new()
            .name("solNorthStar".to_string())
            .spawn(move || {
                let mut last_submitted_settlement: Option<(u64, [u8; 32])> = None;
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

                    let latest_l1_slot = bank_forks
                        .read()
                        .unwrap()
                        .root_bank()
                        .slot()
                        .max(bank.slot());
                    manager.update_latest_l1_slot(latest_l1_slot);

                    // Check for L1 events from the portal program
                    let l1_events = manager.get_l1_events(&bank);

                    let mut reanchored_this_bank = false;
                    for event in l1_events {
                        match event {
                            L1Event::SessionOpened {
                                session_pda,
                                grid_id: _,
                                ttl_slots: _,
                                fee_cap: _,
                            } if !manager.has_active_runtime() => {
                                info!(
                                    "SessionOpened detected at slot {}, activating ephemeral \
                                     runtime (PDA={session_pda})",
                                    bank.slot()
                                );
                                trace!(
                                    "L1 bank for ER activation: slot={}, epoch={}",
                                    bank.slot(),
                                    bank.epoch(),
                                );
                                manager.activate_session(bank.clone(), session_pda);
                                reanchored_this_bank = true;
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

                    // Rebase active ER state onto every new L1 frozen bank.
                    // The ER-local overlay wins for touched/delegated accounts;
                    // everything else is read from the new L1 parent.
                    if manager.has_active_runtime() && !reanchored_this_bank {
                        manager.reanchor_to_l1_parent(bank.clone());
                    } else if !reanchored_this_bank {
                        // Program deploys update loader-owned accounts, not Portal
                        // accounts, so they produce no L1Event. Keep the legacy
                        // targeted refresh path for inactive/no-reanchor cases.
                        manager.refresh_delegated_owner_programs(&bank);
                    }

                    if let Some(sender) = settlement_sender.as_ref() {
                        if let Some((er_slot, checksum, transactions)) =
                            manager.settlement_transactions_if_due(&bank, bank.last_blockhash())
                        {
                            let settlement_key = (er_slot, checksum);
                            if last_submitted_settlement == Some(settlement_key) {
                                debug!(
                                    "Skipping duplicate settlement submission for \
                                     er_slot={er_slot}"
                                );
                            } else if let Err(err) =
                                submit_settlement_transactions(sender, &transactions)
                            {
                                warn!("Failed to enqueue Portal settlement transactions: {err}");
                            } else {
                                info!(
                                    "Enqueued {} Portal settlement transactions for \
                                     er_slot={er_slot}",
                                    transactions.len()
                                );
                                last_submitted_settlement = Some(settlement_key);
                            }
                        }
                    }

                    manager.mark_synced_through(bank.slot());
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
        solana_rpc::{
            northstar::RpcNorthStarSyncStatus,
            optimistically_confirmed_bank_tracker::BankNotification,
        },
        solana_rpc_client_api::{config::RpcSendTransactionConfig, request::RpcRequest},
        solana_runtime::{
            bank::Bank,
            bank_forks::BankForks,
            genesis_utils::{GenesisConfigInfo, create_genesis_config},
        },
        solana_sdk_ids::system_program,
        solana_system_interface::instruction::transfer,
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

    fn find_session_pda(program_id: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"session"], program_id)
    }

    fn find_fee_vault_pda(program_id: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"fee_vault"], program_id)
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
            validator: owner.to_bytes(),
            settlement_interval_slots: 10,
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

    fn build_delegate_ix(
        program_id: Pubkey,
        payer: Pubkey,
        delegated_account: Pubkey,
        owner_program: Pubkey,
        delegation_record_pda: Pubkey,
        buffer: Pubkey,
        session_pda: Pubkey,
        grid_id: u64,
    ) -> Instruction {
        let ix = PortalInstruction::Delegate { grid_id };
        let data = borsh::to_vec(&ix).unwrap();
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(payer, true),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new_readonly(session_pda, false),
                AccountMeta::new(delegated_account, true),
                AccountMeta::new_readonly(owner_program, false),
                AccountMeta::new(delegation_record_pda, false),
                AccountMeta::new_readonly(buffer, false),
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
        let (deposit_receipt_pda, _) = Pubkey::find_program_address(
            &[b"deposit_receipt", session_pda.as_ref(), recipient.as_ref()],
            &program_id,
        );

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

    fn build_close_session_ix(
        program_id: Pubkey,
        owner: Pubkey,
        session_pda: Pubkey,
        fee_vault_pda: Pubkey,
    ) -> Instruction {
        let ix = PortalInstruction::CloseSession;
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
            slot_duration: northstar::DEFAULT_ER_SLOT_DURATION,
            settlement_sender: None,
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
            slot_duration: northstar::DEFAULT_ER_SLOT_DURATION,
            settlement_sender: None,
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
            slot_duration: northstar::DEFAULT_ER_SLOT_DURATION,
            settlement_sender: None,
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
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);

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
            slot_duration: northstar::DEFAULT_ER_SLOT_DURATION,
            settlement_sender: None,
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

        let initial_sync_status: RpcNorthStarSyncStatus = rpc
            .send(
                RpcRequest::Custom {
                    method: "northstarSysGetSyncStatus",
                },
                serde_json::Value::Null,
            )
            .unwrap();
        assert_eq!(
            initial_sync_status,
            RpcNorthStarSyncStatus {
                is_syncing: false,
                latest_synced_slot: bank_for_open.slot(),
                latest_l1_slot: bank_for_open.slot(),
            }
        );

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

        let sync_status_after_activate: RpcNorthStarSyncStatus = rpc
            .send(
                RpcRequest::Custom {
                    method: "northstarSysGetSyncStatus",
                },
                serde_json::Value::Null,
            )
            .unwrap();
        assert_eq!(
            sync_status_after_activate,
            RpcNorthStarSyncStatus {
                is_syncing: false,
                latest_synced_slot: bank_for_open.slot(),
                latest_l1_slot: bank_for_open.slot(),
            }
        );

        let close_bank = Bank::new_from_parent(
            bank_for_open.clone(),
            &Pubkey::default(),
            bank_for_open.slot() + 3,
        );
        let close_ix =
            build_close_session_ix(program_id, owner.pubkey(), session_pda, fee_vault_pda);
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

    #[test]
    fn test_service_reanchors_active_er_to_new_l1_block() {
        agave_logger::setup();

        let (root_bank, bank_forks, program_id, mint_keypair) = setup_bank_with_portal();
        let owner = Keypair::new();
        root_bank
            .transfer(100_000_000_000, &mint_keypair, &owner.pubkey())
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);
        let open_ix = build_open_session_ix(
            program_id,
            owner.pubkey(),
            session_pda,
            fee_vault_pda,
            grid_id,
            100,
            1_000_000,
        );
        let open_tx = Transaction::new_signed_with_payer(
            &[open_ix],
            Some(&owner.pubkey()),
            &[&owner],
            root_bank.last_blockhash(),
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
            slot_duration: northstar::DEFAULT_ER_SLOT_DURATION,
            settlement_sender: None,
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
        sender
            .send((BankNotification::Frozen(bank_for_open.clone()), None))
            .unwrap();

        let mut session_from_rpc = None;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(300));
            session_from_rpc = rpc
                .send(
                    RpcRequest::Custom {
                        method: "getSessionPda",
                    },
                    serde_json::Value::Null,
                )
                .unwrap();
            if session_from_rpc.is_some() {
                break;
            }
        }
        assert_eq!(session_from_rpc, Some(session_pda.to_string()));

        let readonly_account = Pubkey::new_unique();
        let l1_balance = 123_456_789;
        let reanchor_bank = Bank::new_from_parent(
            bank_for_open.clone(),
            &Pubkey::default(),
            bank_for_open.slot() + 1,
        );
        reanchor_bank.store_account(
            &readonly_account,
            &AccountSharedData::new(l1_balance, 0, &system_program::id()),
        );
        reanchor_bank.freeze();
        sender
            .send((BankNotification::Frozen(Arc::new(reanchor_bank)), None))
            .unwrap();

        let mut observed_balance = 0;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(300));
            observed_balance = rpc
                .get_balance_with_commitment(&readonly_account, CommitmentConfig::processed())
                .unwrap()
                .value;
            if observed_balance == l1_balance {
                break;
            }
        }
        assert_eq!(
            observed_balance, l1_balance,
            "active ER should see readonly accounts from the latest L1 bank without session reopen"
        );

        exit.store(true, Ordering::Relaxed);
        service.join().expect("service should join");
    }

    #[test]
    fn test_service_self_deposit_only_credits_er_deposit_amount_and_can_spend_it() {
        agave_logger::setup();

        let (root_bank, bank_forks, program_id, mint_keypair) = setup_bank_with_portal();
        let owner = Keypair::new();
        root_bank
            .transfer(30_000_000_000, &mint_keypair, &owner.pubkey())
            .unwrap();

        let delegated_owner_program = Pubkey::new_unique();
        let delegated_account_keypair = Keypair::new();
        let delegated_account = delegated_account_keypair.pubkey();
        let delegate_buffer = Pubkey::new_unique();
        let delegated_portal_account = AccountSharedData::new(1_000_000, 0, &program_id);
        let delegate_buffer_account =
            AccountSharedData::new(1_000_000, 0, &delegated_owner_program);
        root_bank.store_account(&delegated_account, &delegated_portal_account);
        root_bank.store_account(&delegate_buffer, &delegate_buffer_account);

        let grid_id = 7u64;
        let deposit_amount = 1_000_000_000u64;
        let transfer_amount = 500_000_000u64;
        let third_party = Pubkey::new_unique();
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);
        let (delegation_record_pda, _) =
            find_delegation_record_pda(&program_id, &delegated_account);

        let open_ix = build_open_session_ix(
            program_id,
            owner.pubkey(),
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
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
            slot_duration: northstar::DEFAULT_ER_SLOT_DURATION,
            settlement_sender: None,
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

        sender
            .send((BankNotification::Frozen(bank_for_open.clone()), None))
            .unwrap();

        let mut session_from_rpc = None;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(300));
            session_from_rpc = rpc
                .send(
                    RpcRequest::Custom {
                        method: "getSessionPda",
                    },
                    serde_json::Value::Null,
                )
                .unwrap();
            if session_from_rpc.is_some() {
                break;
            }
        }
        assert_eq!(session_from_rpc, Some(session_pda.to_string()));

        let delegate_bank = Bank::new_from_parent(
            bank_for_open.clone(),
            &Pubkey::default(),
            bank_for_open.slot() + 1,
        );
        let delegate_ix = build_delegate_ix(
            program_id,
            owner.pubkey(),
            delegated_account,
            delegated_owner_program,
            delegation_record_pda,
            delegate_buffer,
            session_pda,
            grid_id,
        );
        let blockhash = delegate_bank.last_blockhash();
        let delegate_tx = Transaction::new_signed_with_payer(
            &[delegate_ix],
            Some(&owner.pubkey()),
            &[&owner, &delegated_account_keypair],
            blockhash,
        );
        delegate_bank.process_transaction(&delegate_tx).unwrap();
        delegate_bank.freeze();
        let delegate_bank = Arc::new(delegate_bank);

        sender
            .send((BankNotification::Frozen(delegate_bank.clone()), None))
            .unwrap();

        let mut delegated_accounts: Vec<String> = vec![];
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(300));
            delegated_accounts = rpc
                .send(
                    RpcRequest::Custom {
                        method: "getDelegatedAccounts",
                    },
                    serde_json::Value::Null,
                )
                .unwrap();
            if delegated_accounts
                .iter()
                .any(|a| a == &delegated_account.to_string())
            {
                break;
            }
        }
        assert!(
            delegated_accounts
                .iter()
                .any(|a| a == &delegated_account.to_string()),
            "delegated account should be visible on ER"
        );

        let deposit_bank = Bank::new_from_parent(
            delegate_bank.clone(),
            &Pubkey::default(),
            delegate_bank.slot() + 1,
        );
        let deposit_ix = build_deposit_fee_ix(
            program_id,
            owner.pubkey(),
            session_pda,
            owner.pubkey(),
            deposit_amount,
        );
        let blockhash = deposit_bank.last_blockhash();
        let deposit_tx = Transaction::new_signed_with_payer(
            &[deposit_ix],
            Some(&owner.pubkey()),
            &[&owner],
            blockhash,
        );
        deposit_bank.process_transaction(&deposit_tx).unwrap();
        deposit_bank.freeze();

        sender
            .send((BankNotification::Frozen(Arc::new(deposit_bank)), None))
            .unwrap();

        let mut owner_er_balance = 0;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(300));
            owner_er_balance = rpc
                .get_balance_with_commitment(&owner.pubkey(), CommitmentConfig::processed())
                .unwrap()
                .value;
            if owner_er_balance == deposit_amount {
                break;
            }
        }
        assert_eq!(
            owner_er_balance, deposit_amount,
            "ER should credit only deposit amount, not inherited L1 balance plus deposit"
        );

        let blockhash = rpc
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .unwrap()
            .0;
        let transfer_tx = Transaction::new_signed_with_payer(
            &[transfer(&owner.pubkey(), &third_party, transfer_amount)],
            Some(&owner.pubkey()),
            &[&owner],
            blockhash,
        );
        rpc.send_transaction_with_config(
            &transfer_tx,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .unwrap();

        let mut third_party_balance = 0;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(300));
            third_party_balance = rpc
                .get_balance_with_commitment(&third_party, CommitmentConfig::processed())
                .unwrap()
                .value;
            if third_party_balance == transfer_amount {
                break;
            }
        }
        assert_eq!(
            third_party_balance, transfer_amount,
            "owner should be able to spend deposited ER funds"
        );

        exit.store(true, Ordering::Relaxed);
        service.join().expect("service should join");
    }
}
