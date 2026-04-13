/// Sonic: Ephemeral rollup TPU — QUIC endpoint for direct transaction submission.
///
/// Spawns a QUIC server using `solana-streamer`, receives PacketBatches,
/// deserialises transactions, and feeds them into `EphemeralTransactionClient`.
use {
    crate::ephemeral_tx_client::EphemeralTransactionClient,
    crossbeam_channel::{unbounded, Receiver},
    log::{debug, info, warn},
    solana_keypair::Keypair,
    solana_perf::packet::PacketBatch,
    solana_send_transaction_service::send_transaction_service_stats::SendTransactionServiceStats,
    solana_send_transaction_service::transaction_client::TransactionClient,
    solana_streamer::{
        nonblocking::simple_qos::SimpleQosConfig,
        quic::{spawn_simple_qos_server, QuicStreamerConfig, SpawnServerResult},
        streamer::StakedNodes,
    },
    std::{
        net::UdpSocket,
        num::NonZeroUsize,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, RwLock,
        },
        thread::{Builder, JoinHandle},
    },
    tokio_util::sync::CancellationToken,
};

/// Manages the ephemeral rollup TPU QUIC endpoint.
pub struct EphemeralTpu {
    /// Thread that reads PacketBatches and feeds them to the tx client.
    receiver_thread: Option<JoinHandle<()>>,
    /// QUIC server spawned by `solana-streamer`.
    _quic_server: SpawnServerResult,
    /// Cancellation token shared with the QUIC server.
    cancel: CancellationToken,
    /// Exit flag for the receiver thread.
    exit: Arc<AtomicBool>,
}

impl EphemeralTpu {
    /// Spawn the TPU QUIC endpoint.
    ///
    /// `tpu_socket` must be a **bound** UDP socket on the desired port.
    /// The QUIC server uses this socket for its underlying transport.
    pub fn new(
        tpu_socket: UdpSocket,
        keypair: &Keypair,
        tx_client: EphemeralTransactionClient,
        exit: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let (packet_sender, packet_receiver) = unbounded();
        let cancel = CancellationToken::new();

        let staked_nodes = Arc::new(RwLock::new(StakedNodes::default()));

        let quic_config = QuicStreamerConfig {
            num_threads: NonZeroUsize::new(1).unwrap(),
            ..QuicStreamerConfig::default()
        };

        let qos_config = SimpleQosConfig::default();

        let quic_server = spawn_simple_qos_server(
            "solErTpu",
            "er_tpu",
            vec![tpu_socket],
            keypair,
            packet_sender,
            staked_nodes,
            quic_config,
            qos_config,
            cancel.clone(),
        )
        .map_err(|e| format!("Failed to spawn ER TPU QUIC server: {e}"))?;

        let receiver_exit = exit.clone();
        let receiver_thread = Builder::new()
            .name("solErTpuRecv".to_string())
            .spawn(move || {
                Self::receiver_loop(packet_receiver, tx_client, receiver_exit);
            })
            .map_err(|e| format!("Failed to spawn ER TPU receiver thread: {e}"))?;

        info!("Ephemeral TPU QUIC server started");

        Ok(Self {
            receiver_thread: Some(receiver_thread),
            _quic_server: quic_server,
            cancel,
            exit,
        })
    }

    /// Main loop: drain PacketBatches from the QUIC server, send to tx client.
    fn receiver_loop(
        receiver: Receiver<PacketBatch>,
        tx_client: EphemeralTransactionClient,
        exit: Arc<AtomicBool>,
    ) {
        let stats = SendTransactionServiceStats::default();
        loop {
            if exit.load(Ordering::Relaxed) {
                break;
            }
            match receiver.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(batch) => {
                    let wire_txs: Vec<Vec<u8>> = batch
                        .iter()
                        .filter(|pkt| !pkt.meta().discard())
                        .map(|pkt| pkt.data(..).unwrap_or_default().to_vec())
                        .collect();

                    if !wire_txs.is_empty() {
                        debug!("ER TPU received {} transaction(s)", wire_txs.len());
                        tx_client.send_transactions_in_batch(wire_txs, &stats);
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
        debug!("ER TPU receiver loop exiting");
    }

    /// Shut down the TPU endpoint and join threads.
    pub fn shutdown(&mut self) {
        info!("Shutting down ER TPU");
        self.exit.store(true, Ordering::Relaxed);
        self.cancel.cancel();
        if let Some(thread) = self.receiver_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for EphemeralTpu {
    fn drop(&mut self) {
        if !self.exit.load(Ordering::Relaxed) {
            warn!("EphemeralTpu dropped without explicit shutdown");
        }
    }
}
