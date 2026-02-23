use {
    arc_swap::ArcSwap,
    log::{debug, warn},
    solana_keypair::Keypair,
    solana_quic_definitions::NotifyKeyUpdate,
    solana_runtime::bank::Bank,
    solana_send_transaction_service::{
        send_transaction_service_stats::SendTransactionServiceStats,
        transaction_client::TransactionClient,
    },
    solana_svm::transaction_processor::ExecutionRecordingConfig,
    solana_svm_timings::ExecuteTimings,
    solana_transaction::versioned::VersionedTransaction,
    std::{error::Error, sync::Arc},
};

pub struct EphemeralTransactionClient {
    bank: ArcSwap<Bank>,
}

impl Clone for EphemeralTransactionClient {
    fn clone(&self) -> Self {
        Self {
            bank: ArcSwap::new(self.bank.load().clone()),
        }
    }
}

impl EphemeralTransactionClient {
    pub fn new(bank: Arc<Bank>) -> Self {
        Self {
            bank: ArcSwap::new(bank),
        }
    }

    pub fn set_bank(&self, bank: Arc<Bank>) {
        self.bank.store(bank);
    }
}

impl TransactionClient for EphemeralTransactionClient {
    fn send_transactions_in_batch(
        &self,
        wire_transactions: Vec<Vec<u8>>,
        _stats: &SendTransactionServiceStats,
    ) {
        let bank = self.bank.load();
        for wire_tx in wire_transactions {
            let tx = match bincode::deserialize(&wire_tx) {
                Ok(tx) => tx,
                Err(e) => {
                    warn!("Failed to deserialize tx: {e}");
                    continue;
                }
            };
            match Self::execute_transaction(&bank, tx) {
                Ok(()) => {}
                Err(e) => {
                    debug!("Tx execution failed: {e}");
                }
            }
        }
    }
}

impl EphemeralTransactionClient {
    fn execute_transaction(
        bank: &Bank,
        tx: VersionedTransaction,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let batch = bank.prepare_entry_batch(vec![tx])?;
        let _results = bank.load_execute_and_commit_transactions(
            &batch,
            usize::MAX,
            ExecutionRecordingConfig::default(),
            &mut ExecuteTimings::default(),
            None,
        );
        Ok(())
    }
}

impl NotifyKeyUpdate for EphemeralTransactionClient {
    fn update_key(&self, _key: &Keypair) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}
