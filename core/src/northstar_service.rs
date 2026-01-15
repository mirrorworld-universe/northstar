use {
    crossbeam_channel::RecvTimeoutError,
    log::*,
    solana_rpc::optimistically_confirmed_bank_tracker::{
        BankNotification, BankNotificationReceiver,
    },
    solana_runtime::bank_forks::BankForks,
    std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread::{Builder, JoinHandle},
        time::Duration,
    },
};

/// NorthStar service that monitors root bank changes
pub struct NorthStarService {
    thread_hdl: JoinHandle<()>,
}

impl NorthStarService {
    /// Create and start the NorthStar service
    /// Sonic: Monitors root slot changes by polling bank_forks
    pub fn new(
        bank_forks: Arc<std::sync::RwLock<BankForks>>,
        receiver: BankNotificationReceiver,
        cfg: northstar::ManagerConfig,
        exit: Arc<AtomicBool>,
    ) -> Self {
        // Sonic: Initialize NorthStar manager
        let mut manager = northstar::Manager::new(cfg);

        let thread_hdl = Builder::new()
            .name("solNorthStar".to_string())
            .spawn(move || {
                loop {
                    if exit.load(Ordering::Relaxed) {
                        break;
                    }

                    let (notification, _dep_work) =
                        match receiver.recv_timeout(Duration::from_secs(1)) {
                            Ok(notification) => notification,
                            Err(RecvTimeoutError::Disconnected) => break,
                            Err(RecvTimeoutError::Timeout) => continue,
                        };

                    Self::process_notification(&mut manager, bank_forks.clone(), notification);
                }

                debug!("NorthStar service shutting down");
            })
            .unwrap();

        Self { thread_hdl }
    }

    fn process_notification(
        manager: &mut northstar::Manager,
        bank_forks: Arc<std::sync::RwLock<BankForks>>,
        notification: BankNotification,
    ) {
        match notification {
            BankNotification::Frozen(_bank) => {}
            BankNotification::NewRootBank(_bank) => {}
            BankNotification::NewRootedChain(_chain) => {}
            BankNotification::OptimisticallyConfirmed(slot) => {
                // TODO: new root slots
                debug!("optimistically confirmed {slot:?}");
                let bank = bank_forks.read().unwrap().get(slot).unwrap();
                manager.create_ephemeral_fork_from_root(bank).unwrap();
            }
        }
    }

    /// Shut down the service and wait for it to finish
    pub fn join(self) -> std::thread::Result<()> {
        self.thread_hdl.join()
    }
}
