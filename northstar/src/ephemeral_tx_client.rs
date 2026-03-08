use {
    log::{debug, warn},
    solana_keypair::Keypair,
    solana_quic_definitions::NotifyKeyUpdate,
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    solana_send_transaction_service::{
        send_transaction_service_stats::SendTransactionServiceStats,
        transaction_client::TransactionClient,
    },
    solana_svm::transaction_processor::ExecutionRecordingConfig,
    solana_svm_timings::ExecuteTimings,
    solana_transaction::versioned::VersionedTransaction,
    std::{
        collections::HashSet,
        error::Error,
        sync::{Arc, RwLock},
    },
};

pub struct EphemeralTransactionClient {
    bank_forks: Arc<RwLock<BankForks>>,
    /// Set of delegated account pubkeys for filtering
    delegated_accounts: Arc<HashSet<solana_pubkey::Pubkey>>,
}

impl Clone for EphemeralTransactionClient {
    fn clone(&self) -> Self {
        Self {
            bank_forks: Arc::clone(&self.bank_forks),
            delegated_accounts: Arc::clone(&self.delegated_accounts),
        }
    }
}

impl EphemeralTransactionClient {
    pub fn new(
        bank_forks: Arc<RwLock<BankForks>>,
        delegated_accounts: Arc<HashSet<solana_pubkey::Pubkey>>,
    ) -> Self {
        Self {
            bank_forks,
            delegated_accounts,
        }
    }

    pub fn bank(&self) -> Arc<Bank> {
        self.bank_forks.read().unwrap().working_bank()
    }
}

impl TransactionClient for EphemeralTransactionClient {
    fn send_transactions_in_batch(
        &self,
        wire_transactions: Vec<Vec<u8>>,
        _stats: &SendTransactionServiceStats,
    ) {
        let bank = self.bank();
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
        let results = bank.load_execute_and_commit_transactions(
            &batch,
            usize::MAX, // TODO: Use appropriate age limit for ephemeral rollup
            ExecutionRecordingConfig::default(),
            &mut ExecuteTimings::default(),
            None,
        );
        for (tx_idx, result) in results.0.iter().enumerate() {
            if let Err(e) = result {
                debug!("Tx {tx_idx} failed: {e}");
            }
        }
        Ok(())
    }
}

impl NotifyKeyUpdate for EphemeralTransactionClient {
    fn update_key(&self, _key: &Keypair) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}
