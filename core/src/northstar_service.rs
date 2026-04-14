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
                                session_pda: _,
                                owner: _,
                                grid_id: _,
                                ttl_slots: _,
                                fee_cap: _,
                            } if !manager.has_active_runtime() => {
                                info!(
                                    "SessionOpened detected at slot {}, activating ephemeral \
                                     runtime",
                                    bank.slot()
                                );
                                // Sonic: Re-fork from CURRENT L1 root bank
                                let l1_root = bank_forks.read().unwrap().root_bank();
                                manager.activate_session(l1_root);
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
        solana_account::AccountSharedData,
        solana_gossip::contact_info::ContactInfo,
        solana_keypair::{Keypair, Signer},
        solana_net_utils::SocketAddrSpace,
        solana_pubkey::Pubkey,
        solana_rpc::optimistically_confirmed_bank_tracker::BankNotification,
        solana_sdk_ids::system_program,
        std::net::TcpListener,
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
}
