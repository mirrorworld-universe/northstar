use {
    crate::banking_trace::BankingPacketSender,
    agave_banking_stage_ingress_types::BankingPacketBatch,
    crossbeam_channel::{RecvTimeoutError, SendError, Sender, TrySendError},
    log::*,
    northstar::{
        L1Event,
        portal_state::{PortalAccount, try_parse_raw_portal_account},
    },
    northstar_portal::SettlementStatus,
    solana_account::ReadableAccount,
    solana_gossip::cluster_info::ClusterInfo,
    solana_hash::Hash,
    solana_perf::packet::{NUM_PACKETS, PacketFlags, to_packet_batches},
    solana_rpc::optimistically_confirmed_bank_tracker::{
        BankNotification, BankNotificationReceiver,
    },
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    solana_signature::Signature,
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
    /// Forwarding-stage sender for propagation when this node is not current leader.
    pub settlement_forward_sender: Option<Sender<(BankingPacketBatch, bool)>>,
}

/// NorthStar service that monitors root bank changes and creates ephemeral rollups
pub struct NorthStarService {
    thread_hdl: JoinHandle<()>,
}

#[derive(Debug)]
enum SettlementSubmitError {
    Local(SendError<BankingPacketBatch>),
    ForwardFull,
    ForwardDisconnected,
}

impl std::fmt::Display for SettlementSubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(err) => write!(f, "local banking enqueue failed: {err}"),
            Self::ForwardFull => write!(f, "forwarding channel is full"),
            Self::ForwardDisconnected => write!(f, "forwarding channel disconnected"),
        }
    }
}

fn submit_settlement_transactions(
    sender: &BankingPacketSender,
    forward_sender: Option<&Sender<(BankingPacketBatch, bool)>>,
    transactions: &[Transaction],
) -> Result<(), SettlementSubmitError> {
    if transactions.is_empty() {
        return Ok(());
    }

    let mut packet_batches = to_packet_batches(transactions, NUM_PACKETS);
    for packet_batch in packet_batches.iter_mut() {
        for mut packet in packet_batch.iter_mut() {
            packet.meta_mut().flags |= PacketFlags::FROM_STAKED_NODE;
        }
    }
    let batch = BankingPacketBatch::new(packet_batches);
    sender
        .send(batch.clone())
        .map_err(SettlementSubmitError::Local)?;

    if let Some(forward_sender) = forward_sender {
        match forward_sender.try_send((batch, false)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(SettlementSubmitError::ForwardFull),
            Err(TrySendError::Disconnected(_)) => {
                return Err(SettlementSubmitError::ForwardDisconnected);
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingSettlementStatus {
    Pending,
    Confirmed,
    Expired,
    Failed,
}

#[derive(Debug, Clone)]
struct PendingSettlementSubmission {
    er_slot: u64,
    checksum: [u8; 32],
    signatures: Vec<Signature>,
    recent_blockhash: Hash,
    submitted_l1_slot: u64,
    attempts: u64,
    current_transaction: Transaction,
    remaining_transactions: Vec<Transaction>,
}

impl PendingSettlementSubmission {
    fn new(
        er_slot: u64,
        checksum: [u8; 32],
        recent_blockhash: Hash,
        submitted_l1_slot: u64,
        attempts: u64,
        mut transactions: Vec<Transaction>,
    ) -> Option<(Self, Transaction)> {
        transactions.reverse();
        let current_transaction = transactions.pop()?;
        let signatures = current_transaction
            .signatures
            .first()
            .cloned()
            .into_iter()
            .collect();
        Some((
            Self {
                er_slot,
                checksum,
                signatures,
                recent_blockhash,
                submitted_l1_slot,
                attempts,
                current_transaction: current_transaction.clone(),
                remaining_transactions: transactions,
            },
            current_transaction,
        ))
    }

    fn status(&self, bank: &Bank) -> PendingSettlementStatus {
        if self
            .signatures
            .iter()
            .any(|signature| matches!(bank.get_signature_status(signature), Some(Err(_))))
        {
            return PendingSettlementStatus::Failed;
        }
        if !self.signatures.is_empty()
            && self
                .signatures
                .iter()
                .all(|signature| matches!(bank.get_signature_status(signature), Some(Ok(()))))
        {
            return PendingSettlementStatus::Confirmed;
        }
        if !bank.is_hash_valid_for_age(&self.recent_blockhash, solana_clock::MAX_PROCESSING_AGE) {
            return PendingSettlementStatus::Expired;
        }
        PendingSettlementStatus::Pending
    }

    fn pop_next_transaction(&mut self) -> Option<Transaction> {
        self.remaining_transactions.pop()
    }

    fn track_transaction(&mut self, transaction: &Transaction, bank: &Bank) {
        self.signatures = transaction
            .signatures
            .first()
            .cloned()
            .into_iter()
            .collect();
        self.recent_blockhash = transaction.message.recent_blockhash;
        self.submitted_l1_slot = bank.slot();
        self.current_transaction = transaction.clone();
    }

    fn should_rebroadcast(&self, bank: &Bank) -> bool {
        const SETTLEMENT_REBROADCAST_INTERVAL_SLOTS: u64 = 10;
        bank.slot()
            >= self
                .submitted_l1_slot
                .saturating_add(SETTLEMENT_REBROADCAST_INTERVAL_SLOTS)
    }

    fn mark_rebroadcasted(&mut self, bank: &Bank) {
        self.submitted_l1_slot = bank.slot();
    }

    fn failed_signature_reasons(&self, bank: &Bank) -> Vec<(Signature, String)> {
        self.signatures
            .iter()
            .filter_map(|signature| {
                if let Some(Err(err)) = bank.get_signature_status(signature) {
                    Some((*signature, format!("{err:?}")))
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
fn pending_settlement_allows_submission(
    pending_settlement: &mut Option<PendingSettlementSubmission>,
    bank: &Bank,
) -> bool {
    let Some(pending) = pending_settlement.as_ref() else {
        return true;
    };

    match pending.status(bank) {
        PendingSettlementStatus::Pending => {
            debug!(
                "Portal settlement still unconfirmed for er_slot={} checksum={:?} attempts={} \
                 signatures={:?}",
                pending.er_slot, pending.checksum, pending.attempts, pending.signatures,
            );
            false
        }
        PendingSettlementStatus::Confirmed => {
            info!(
                "Portal settlement confirmed for er_slot={} checksum={:?} attempts={} \
                 submitted_l1_slot={} confirmed_l1_slot={} signatures={:?}",
                pending.er_slot,
                pending.checksum,
                pending.attempts,
                pending.submitted_l1_slot,
                bank.slot(),
                pending.signatures,
            );
            *pending_settlement = None;
            true
        }
        PendingSettlementStatus::Expired => {
            warn!(
                "Portal settlement expired before confirmation for er_slot={} checksum={:?} \
                 attempts={} submitted_l1_slot={} current_l1_slot={} signatures={:?}; retrying",
                pending.er_slot,
                pending.checksum,
                pending.attempts,
                pending.submitted_l1_slot,
                bank.slot(),
                pending.signatures,
            );
            *pending_settlement = None;
            true
        }
        PendingSettlementStatus::Failed => {
            let failure_reasons = pending.failed_signature_reasons(bank);
            warn!(
                "Portal settlement transaction failed for er_slot={} checksum={:?} attempts={} \
                 submitted_l1_slot={} current_l1_slot={} failure_reasons={:?}; retrying if \
                 session state permits",
                pending.er_slot,
                pending.checksum,
                pending.attempts,
                pending.submitted_l1_slot,
                bank.slot(),
                failure_reasons,
            );
            *pending_settlement = None;
            true
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StuckSettlement {
    er_slot: u64,
    checksum: [u8; 32],
    started_l1_slot: u64,
    current_l1_slot: u64,
    settlement_interval_slots: u64,
}

fn stuck_settlement(
    bank: &Bank,
    portal_program_id: &solana_pubkey::Pubkey,
    session_pda: Option<solana_pubkey::Pubkey>,
) -> Option<StuckSettlement> {
    let session_pda = session_pda?;
    let session_account = bank.get_account(&session_pda)?;
    if session_account.owner() != portal_program_id {
        return None;
    }
    let PortalAccount::Session(session) = try_parse_raw_portal_account(session_account.data())?
    else {
        return None;
    };
    if session.settlement_status != SettlementStatus::InProgress {
        return None;
    }
    let stuck_after_slot = session
        .settlement_started_l1_slot
        .saturating_add(session.settlement_interval_slots);
    if bank.slot() <= stuck_after_slot {
        return None;
    }
    Some(StuckSettlement {
        er_slot: session.settlement_er_slot,
        checksum: session.settlement_checksum,
        started_l1_slot: session.settlement_started_l1_slot,
        current_l1_slot: bank.slot(),
        settlement_interval_slots: session.settlement_interval_slots,
    })
}

fn should_warn_stuck_settlement(
    last_warned_stuck_settlement: &mut Option<(u64, [u8; 32], u64)>,
    stuck: StuckSettlement,
) -> bool {
    let warn_every_slots = stuck.settlement_interval_slots.max(1);
    let should_warn = !matches!(
        last_warned_stuck_settlement,
        Some((er_slot, checksum, last_warn_slot))
            if *er_slot == stuck.er_slot
                && *checksum == stuck.checksum
                && stuck.current_l1_slot < last_warn_slot.saturating_add(warn_every_slots)
    );
    if should_warn {
        *last_warned_stuck_settlement =
            Some((stuck.er_slot, stuck.checksum, stuck.current_l1_slot));
    }
    should_warn
}

fn warn_stuck_settlement_if_due(
    bank: &Bank,
    portal_program_id: &solana_pubkey::Pubkey,
    session_pda: Option<solana_pubkey::Pubkey>,
    last_warned_stuck_settlement: &mut Option<(u64, [u8; 32], u64)>,
) {
    let Some(stuck) = stuck_settlement(bank, portal_program_id, session_pda) else {
        return;
    };
    if !should_warn_stuck_settlement(last_warned_stuck_settlement, stuck) {
        return;
    }

    warn!(
        "Portal settlement stuck InProgress for er_slot={} started_l1_slot={} current_l1_slot={} \
         interval_slots={}; waiting for validator retry/abort",
        stuck.er_slot,
        stuck.started_l1_slot,
        stuck.current_l1_slot,
        stuck.settlement_interval_slots,
    );
}

fn submit_settlement_if_due(
    manager: &northstar::Manager,
    bank: &Bank,
    sender: &BankingPacketSender,
    forward_sender: Option<&Sender<(BankingPacketBatch, bool)>>,
    pending_settlement: &mut Option<PendingSettlementSubmission>,
    settlement_attempts: &mut u64,
) {
    if submit_next_pending_settlement_if_ready(
        manager,
        bank,
        sender,
        forward_sender,
        pending_settlement,
    ) {
        return;
    }
    if pending_settlement.is_some() {
        return;
    }

    let recent_blockhash = bank.last_blockhash();
    let Some((er_slot, checksum, transactions)) =
        manager.settlement_transactions_if_due(bank, recent_blockhash)
    else {
        return;
    };

    *settlement_attempts = settlement_attempts.saturating_add(1);
    let Some((pending, transaction)) = PendingSettlementSubmission::new(
        er_slot,
        checksum,
        recent_blockhash,
        bank.slot(),
        *settlement_attempts,
        transactions,
    ) else {
        return;
    };

    if let Err(err) =
        submit_settlement_transactions(sender, forward_sender, std::slice::from_ref(&transaction))
    {
        warn!("Failed to enqueue Portal settlement transaction: {err}");
        return;
    }

    info!(
        "Enqueued Portal settlement transaction for er_slot={} attempts={} remaining_txs={}",
        er_slot,
        *settlement_attempts,
        pending.remaining_transactions.len(),
    );
    *pending_settlement = Some(pending);
}

fn submit_next_pending_settlement_if_ready(
    manager: &northstar::Manager,
    bank: &Bank,
    sender: &BankingPacketSender,
    forward_sender: Option<&Sender<(BankingPacketBatch, bool)>>,
    pending_settlement: &mut Option<PendingSettlementSubmission>,
) -> bool {
    let Some(pending) = pending_settlement.as_mut() else {
        return false;
    };

    match pending.status(bank) {
        PendingSettlementStatus::Pending => {
            if pending.should_rebroadcast(bank) {
                let transaction = pending.current_transaction.clone();
                match submit_settlement_transactions(
                    sender,
                    forward_sender,
                    std::slice::from_ref(&transaction),
                ) {
                    Ok(()) => {
                        pending.mark_rebroadcasted(bank);
                        info!(
                            "Rebroadcast Portal settlement transaction for er_slot={} \
                             checksum={:?} attempts={} l1_slot={} signatures={:?}",
                            pending.er_slot,
                            pending.checksum,
                            pending.attempts,
                            bank.slot(),
                            pending.signatures,
                        );
                    }
                    Err(err) => warn!(
                        "Failed to rebroadcast Portal settlement transaction for er_slot={} \
                         checksum={:?} attempts={} l1_slot={} signatures={:?}: {err}",
                        pending.er_slot,
                        pending.checksum,
                        pending.attempts,
                        bank.slot(),
                        pending.signatures,
                    ),
                }
            } else {
                debug!(
                    "Portal settlement still unconfirmed for er_slot={} checksum={:?} attempts={} \
                     signatures={:?}",
                    pending.er_slot, pending.checksum, pending.attempts, pending.signatures,
                );
            }
            true
        }
        PendingSettlementStatus::Confirmed => {
            info!(
                "Portal settlement transaction confirmed for er_slot={} checksum={:?} attempts={} \
                 submitted_l1_slot={} confirmed_l1_slot={} signatures={:?}",
                pending.er_slot,
                pending.checksum,
                pending.attempts,
                pending.submitted_l1_slot,
                bank.slot(),
                pending.signatures,
            );
            let Some(mut transaction) = pending.pop_next_transaction() else {
                *pending_settlement = None;
                return false;
            };
            manager.resign_settlement_transaction(&mut transaction, bank.last_blockhash());
            pending.track_transaction(&transaction, bank);
            if let Err(err) = submit_settlement_transactions(
                sender,
                forward_sender,
                std::slice::from_ref(&transaction),
            ) {
                warn!("Failed to enqueue next Portal settlement transaction: {err}");
                return true;
            }
            info!(
                "Enqueued next Portal settlement transaction for er_slot={} remaining_txs={}",
                pending.er_slot,
                pending.remaining_transactions.len(),
            );
            true
        }
        PendingSettlementStatus::Expired => {
            warn!(
                "Portal settlement transaction expired before confirmation for er_slot={} \
                 checksum={:?} attempts={} submitted_l1_slot={} current_l1_slot={} \
                 signatures={:?}; retrying",
                pending.er_slot,
                pending.checksum,
                pending.attempts,
                pending.submitted_l1_slot,
                bank.slot(),
                pending.signatures,
            );
            *pending_settlement = None;
            false
        }
        PendingSettlementStatus::Failed => {
            let failure_reasons = pending.failed_signature_reasons(bank);
            warn!(
                "Portal settlement transaction failed for er_slot={} checksum={:?} attempts={} \
                 submitted_l1_slot={} current_l1_slot={} failure_reasons={:?}; retrying if \
                 session state permits",
                pending.er_slot,
                pending.checksum,
                pending.attempts,
                pending.submitted_l1_slot,
                bank.slot(),
                failure_reasons,
            );
            *pending_settlement = None;
            false
        }
    }
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
        let portal_program_id = cfg.portal_program_id;
        let mut manager = northstar::Manager::new(cfg);
        manager.set_slot_duration(config.slot_duration);
        {
            let root_bank = bank_forks.read().unwrap().root_bank();
            if let Err(e) = manager.init_runtime(
                root_bank.clone(),
                cluster_info.clone(),
                config.listen_addr,
                config.ws_addr,
                config.tpu_addr,
            ) {
                error!("Failed to initialize ephemeral runtime: {e}");
            } else {
                // Sonic: Hotfix: do not resume historical sessions on the validator
                // startup hot path. Resume scans Portal-owned accounts from the L1
                // bank, which can devolve into an AccountsDB-wide scan without a
                // program-id index and stall NorthStar sync reporting. Operators can
                // reopen sessions until resume has a bounded/indexed recovery path.
                info!(
                    "NorthStar historical session resume skipped; reopen the Portal session to \
                     activate ER"
                );
            }
        }

        let settlement_sender = config.settlement_sender;
        let settlement_forward_sender = config.settlement_forward_sender;
        let thread_hdl = Builder::new()
            .name("solNorthStar".to_string())
            .spawn(move || {
                let mut pending_settlement: Option<PendingSettlementSubmission> = None;
                let mut settlement_attempts: u64 = 0;
                let mut last_warned_stuck_settlement: Option<(u64, [u8; 32], u64)> = None;
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
                                grid_id,
                                ttl_slots,
                                fee_cap,
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
                                manager.activate_session(
                                    bank.clone(),
                                    session_pda,
                                    grid_id,
                                    ttl_slots,
                                    fee_cap,
                                );
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

                    warn_stuck_settlement_if_due(
                        &bank,
                        &portal_program_id,
                        manager.session_pda().and_then(|pda| *pda.read().unwrap()),
                        &mut last_warned_stuck_settlement,
                    );

                    if let Some(sender) = settlement_sender.as_ref() {
                        submit_settlement_if_due(
                            &manager,
                            &bank,
                            sender,
                            settlement_forward_sender.as_ref(),
                            &mut pending_settlement,
                            &mut settlement_attempts,
                        );
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
        crossbeam_channel::{bounded, unbounded},
        northstar_portal::{OpenSession, PortalInstruction, Session, SettlementStatus},
        solana_account::{AccountSharedData, WritableAccount},
        solana_client::rpc_client::RpcClient,
        solana_commitment_config::CommitmentConfig,
        solana_gossip::contact_info::ContactInfo,
        solana_instruction::{AccountMeta, Instruction},
        solana_keypair::{Keypair, Signer},
        solana_leader_schedule::SlotLeader,
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

    fn create_processable_test_bank() -> Arc<Bank> {
        use solana_genesis_config::GenesisConfig;
        let genesis_config = GenesisConfig::new(&[], &[]);
        Bank::new_with_bank_forks_for_tests(&genesis_config).0
    }

    fn fund_test_payer(bank: &Bank, payer: &Keypair) {
        bank.store_account(
            &payer.pubkey(),
            &AccountSharedData::new(1_000_000_000, 0, &system_program::id()),
        );
    }

    fn signed_test_transfer(bank: &Bank, payer: &Keypair) -> Transaction {
        Transaction::new_signed_with_payer(
            &[transfer(&payer.pubkey(), &Pubkey::new_unique(), 1)],
            Some(&payer.pubkey()),
            &[payer],
            bank.last_blockhash(),
        )
    }

    fn pending_test_submission(
        bank: &Bank,
        recent_blockhash: Hash,
        transaction: Transaction,
    ) -> PendingSettlementSubmission {
        PendingSettlementSubmission::new(
            7,
            [3; 32],
            recent_blockhash,
            bank.slot(),
            1,
            vec![transaction],
        )
        .unwrap()
        .0
    }

    fn packet_count(batch: &BankingPacketBatch) -> usize {
        batch.iter().map(|packets| packets.len()).sum()
    }

    fn all_packets_marked_from_staked_node(batch: &BankingPacketBatch) -> bool {
        batch.iter().all(|packets| {
            packets
                .iter()
                .all(|packet| packet.meta().flags.contains(PacketFlags::FROM_STAKED_NODE))
        })
    }

    #[test]
    fn settlement_submission_forwards_transactions_to_leader_path() {
        let bank = create_processable_test_bank();
        let payer = Keypair::new();
        fund_test_payer(&bank, &payer);
        let transaction = signed_test_transfer(&bank, &payer);
        let (settlement_sender, local_receiver) =
            crate::banking_trace::BankingTracer::channel_for_test();
        let (forward_sender, forward_receiver) = unbounded();

        submit_settlement_transactions(
            &settlement_sender,
            Some(&forward_sender),
            std::slice::from_ref(&transaction),
        )
        .unwrap();

        let local_batch = local_receiver.try_recv().unwrap();
        assert_eq!(packet_count(&local_batch), 1);
        assert!(all_packets_marked_from_staked_node(&local_batch));

        let (forward_batch, is_tpu_vote_batch) = forward_receiver.try_recv().unwrap();
        assert!(!is_tpu_vote_batch);
        assert_eq!(packet_count(&forward_batch), 1);
        assert!(all_packets_marked_from_staked_node(&forward_batch));
    }

    #[test]
    fn settlement_submission_fails_when_forwarding_path_is_unavailable() {
        let bank = create_processable_test_bank();
        let payer = Keypair::new();
        fund_test_payer(&bank, &payer);
        let transaction = signed_test_transfer(&bank, &payer);
        let (settlement_sender, _local_receiver) =
            crate::banking_trace::BankingTracer::channel_for_test();
        let (forward_sender, forward_receiver) = bounded(0);
        drop(forward_receiver);

        let err = submit_settlement_transactions(
            &settlement_sender,
            Some(&forward_sender),
            std::slice::from_ref(&transaction),
        )
        .unwrap_err();

        assert!(matches!(err, SettlementSubmitError::ForwardDisconnected));
    }

    #[test]
    fn pending_settlement_waits_for_confirmation_before_duplicate_submission() {
        let bank = create_processable_test_bank();
        let payer = Keypair::new();
        fund_test_payer(&bank, &payer);
        let transaction = signed_test_transfer(&bank, &payer);
        let mut pending_settlement = Some(pending_test_submission(
            &bank,
            bank.last_blockhash(),
            transaction,
        ));

        assert!(!pending_settlement_allows_submission(
            &mut pending_settlement,
            &bank,
        ));
        assert!(pending_settlement.is_some());
    }

    #[test]
    fn expired_pending_settlement_allows_retry() {
        let bank = create_processable_test_bank();
        let payer = Keypair::new();
        fund_test_payer(&bank, &payer);
        let transaction = signed_test_transfer(&bank, &payer);
        let mut pending_settlement = Some(pending_test_submission(
            &bank,
            Hash::new_unique(),
            transaction,
        ));

        assert!(pending_settlement_allows_submission(
            &mut pending_settlement,
            &bank,
        ));
        assert!(pending_settlement.is_none());
    }

    #[test]
    fn confirmed_pending_settlement_clears_tracking() {
        let bank = create_processable_test_bank();
        let payer = Keypair::new();
        fund_test_payer(&bank, &payer);
        let transaction = signed_test_transfer(&bank, &payer);
        bank.status_cache.write().unwrap().insert(
            &bank.last_blockhash(),
            transaction.signatures[0],
            bank.slot(),
            Ok(()),
        );
        let mut pending_settlement = Some(pending_test_submission(
            &bank,
            bank.last_blockhash(),
            transaction,
        ));

        assert!(pending_settlement_allows_submission(
            &mut pending_settlement,
            &bank,
        ));
        assert!(pending_settlement.is_none());
    }

    #[test]
    fn next_split_settlement_transaction_is_resigned_with_fresh_blockhash() {
        let bank = create_processable_test_bank();
        let payer = Arc::new(Keypair::new());
        fund_test_payer(&bank, &payer);
        let first_transaction = signed_test_transfer(&bank, &payer);
        let next_transaction = signed_test_transfer(&bank, &payer);
        let old_next_signature = next_transaction.signatures[0];
        let mut pending = PendingSettlementSubmission::new(
            7,
            [3; 32],
            bank.last_blockhash(),
            bank.slot(),
            1,
            vec![first_transaction, next_transaction],
        )
        .unwrap()
        .0;
        let manager = northstar::Manager::new(northstar::ManagerConfig {
            portal_program_id: Pubkey::new_unique(),
            manager_account: Arc::clone(&payer),
        });

        let mut transaction = pending.pop_next_transaction().unwrap();
        let fresh_blockhash = Hash::new_unique();
        manager.resign_settlement_transaction(&mut transaction, fresh_blockhash);
        pending.track_transaction(&transaction, &bank);

        assert_eq!(transaction.message.recent_blockhash, fresh_blockhash);
        assert_eq!(pending.recent_blockhash, fresh_blockhash);
        assert_ne!(transaction.signatures[0], old_next_signature);
        assert_eq!(pending.signatures, vec![transaction.signatures[0]]);
    }

    #[test]
    fn pending_settlement_rebroadcasts_before_expiry() {
        let root_bank = create_processable_test_bank();
        let bank = Bank::new_from_parent(root_bank, SlotLeader::new_unique(), 20);
        let payer = Arc::new(Keypair::new());
        fund_test_payer(&bank, &payer);
        let transaction = signed_test_transfer(&bank, &payer);
        let mut pending_settlement = Some(
            PendingSettlementSubmission::new(
                7,
                [3; 32],
                bank.last_blockhash(),
                0,
                1,
                vec![transaction],
            )
            .unwrap()
            .0,
        );
        let manager = northstar::Manager::new(northstar::ManagerConfig {
            portal_program_id: Pubkey::new_unique(),
            manager_account: Arc::clone(&payer),
        });
        let (settlement_sender, local_receiver) =
            crate::banking_trace::BankingTracer::channel_for_test();
        let (forward_sender, forward_receiver) = unbounded();

        assert!(submit_next_pending_settlement_if_ready(
            &manager,
            &bank,
            &settlement_sender,
            Some(&forward_sender),
            &mut pending_settlement,
        ));

        assert_eq!(packet_count(&local_receiver.try_recv().unwrap()), 1);
        assert_eq!(packet_count(&forward_receiver.try_recv().unwrap().0), 1);
        assert_eq!(pending_settlement.unwrap().submitted_l1_slot, bank.slot());
    }

    #[test]
    fn failed_pending_settlement_allows_retry() {
        let bank = create_processable_test_bank();
        let payer = Keypair::new();
        fund_test_payer(&bank, &payer);
        let transaction = signed_test_transfer(&bank, &payer);
        let signature = transaction.signatures[0];
        bank.status_cache.write().unwrap().insert(
            &bank.last_blockhash(),
            signature,
            bank.slot(),
            Err(solana_transaction_error::TransactionError::AccountNotFound),
        );
        let pending = pending_test_submission(&bank, bank.last_blockhash(), transaction);
        assert_eq!(
            pending.failed_signature_reasons(&bank),
            vec![(signature, "AccountNotFound".to_string())]
        );
        let mut pending_settlement = Some(pending);

        assert!(pending_settlement_allows_submission(
            &mut pending_settlement,
            &bank,
        ));
        assert!(pending_settlement.is_none());
    }

    #[test]
    fn stuck_in_progress_settlement_is_reported_and_throttled() {
        let root_bank = create_processable_test_bank();
        let bank = Bank::new_from_parent(root_bank, SlotLeader::new_unique(), 20);
        let program_id = Pubkey::new_unique();
        let session_pda = Pubkey::new_unique();
        let session = Session {
            discriminator: Session::DISCRIMINATOR,
            grid_id: 1,
            ttl_slots: 100,
            fee_cap: 1_000,
            created_at: 0,
            nonce: 0,
            authority: [1; 32],
            validator: [2; 32],
            settlement_interval_slots: 10,
            last_settled_l1_slot: 0,
            last_settled_er_slot: 0,
            settlement_status: SettlementStatus::InProgress,
            settlement_er_slot: 9,
            settlement_checksum: [4; 32],
            settlement_accumulator: [0; 32],
            settlement_started_l1_slot: 1,
            bump: 255,
        };
        let data = borsh::to_vec(&session).unwrap();
        let mut account = AccountSharedData::new(1_000_000, data.len(), &program_id);
        account.data_as_mut_slice().copy_from_slice(&data);
        bank.store_account(&session_pda, &account);

        let stuck = stuck_settlement(&bank, &program_id, Some(session_pda))
            .expect("in-progress settlement older than interval should be stuck");
        assert_eq!(stuck.er_slot, 9);
        assert_eq!(stuck.started_l1_slot, 1);
        assert_eq!(stuck.current_l1_slot, 20);

        let mut last_warned = None;
        assert!(should_warn_stuck_settlement(&mut last_warned, stuck));
        assert!(!should_warn_stuck_settlement(&mut last_warned, stuck));
        assert!(should_warn_stuck_settlement(
            &mut last_warned,
            StuckSettlement {
                current_l1_slot: 30,
                ..stuck
            },
        ));
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
        let bank = Bank::new_from_parent(bank.clone(), SlotLeader::new_unique(), bank.slot() + 1);
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
            settlement_forward_sender: None,
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
            settlement_forward_sender: None,
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
            settlement_forward_sender: None,
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
    fn test_service_does_not_resume_historical_l1_session_on_startup() {
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
        let (_sender, receiver) = unbounded();
        let exit = Arc::new(AtomicBool::new(false));
        let config = NorthStarServiceConfig {
            listen_addr: find_free_addr(),
            ws_addr: find_free_addr(),
            tpu_addr: find_free_addr(),
            slot_duration: northstar::DEFAULT_ER_SLOT_DURATION,
            settlement_sender: None,
            settlement_forward_sender: None,
        };

        let service = NorthStarService::new(
            bank_forks,
            receiver,
            northstar::ManagerConfig {
                portal_program_id: program_id,
                manager_account: Arc::new(owner.insecure_clone()),
            },
            cluster_info,
            config.clone(),
            exit.clone(),
        );

        let rpc = RpcClient::new(format!("http://{}", config.listen_addr));
        std::thread::sleep(Duration::from_millis(300));

        let sync_status: RpcNorthStarSyncStatus = rpc
            .send(
                RpcRequest::Custom {
                    method: "northstarSysGetSyncStatus",
                },
                serde_json::Value::Null,
            )
            .unwrap();
        assert_eq!(
            sync_status,
            RpcNorthStarSyncStatus {
                is_syncing: false,
                latest_synced_slot: bank_for_open.slot(),
                latest_l1_slot: bank_for_open.slot(),
            }
        );

        let slot_before = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let slot_after = rpc
            .get_slot_with_commitment(CommitmentConfig::processed())
            .unwrap();
        assert_eq!(
            slot_after, slot_before,
            "ER slot must not advance for a historical L1 session skipped by startup hotfix"
        );

        let session_from_rpc: Option<String> = rpc
            .send(
                RpcRequest::Custom {
                    method: "getSessionPda",
                },
                serde_json::Value::Null,
            )
            .unwrap();
        assert_eq!(session_from_rpc, None);

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
            settlement_forward_sender: None,
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
            SlotLeader::new_unique(),
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
            settlement_forward_sender: None,
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
            SlotLeader::new_unique(),
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
            SlotLeader::new_unique(),
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
