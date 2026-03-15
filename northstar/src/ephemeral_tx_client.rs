use {
    log::{debug, warn},
    solana_account::{ReadableAccount, WritableAccount},
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
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
    delegated_accounts: Arc<HashSet<Pubkey>>,
    /// Accounts that have been written to on this ER.
    /// Once touched, their balance is "real" (not inherited from L1).
    touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
}

impl Clone for EphemeralTransactionClient {
    fn clone(&self) -> Self {
        Self {
            bank_forks: Arc::clone(&self.bank_forks),
            delegated_accounts: Arc::clone(&self.delegated_accounts),
            touched_accounts: Arc::clone(&self.touched_accounts),
        }
    }
}

impl EphemeralTransactionClient {
    pub fn new(
        bank_forks: Arc<RwLock<BankForks>>,
        delegated_accounts: Arc<HashSet<Pubkey>>,
        touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    ) -> Self {
        Self {
            bank_forks,
            delegated_accounts,
            touched_accounts,
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

            // Zero untouched writable accounts before execution
            Self::zero_untouched_writable_accounts(
                &bank,
                &tx,
                &self.touched_accounts,
                &self.delegated_accounts,
            );

            match Self::execute_transaction(&bank, tx.clone()) {
                Ok(()) => {}
                Err(e) => {
                    debug!("Tx execution failed: {e}");
                }
            }

            // Mark writable accounts as touched (even on failure, since the fee payer was debited)
            Self::mark_writable_as_touched(&tx, &self.touched_accounts);
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

    /// Check if a key is an infrastructure account (system program, sysvars, etc.)
    fn is_infrastructure_account(key: &Pubkey) -> bool {
        use solana_sdk_ids::{
            address_lookup_table, bpf_loader, bpf_loader_upgradeable, compute_budget, config,
            stake, system_program, sysvar, vote,
        };

        system_program::check_id(key)
            || sysvar::check_id(key)
            || bpf_loader::check_id(key)
            || bpf_loader_upgradeable::check_id(key)
            || vote::check_id(key)
            || stake::check_id(key)
            || config::check_id(key)
            || compute_budget::check_id(key)
            || address_lookup_table::check_id(key)
    }

    /// Zero the balance of untouched writable accounts before transaction execution.
    /// This prevents users from spending inherited L1 balances on the ER.
    fn zero_untouched_writable_accounts(
        bank: &Bank,
        tx: &VersionedTransaction,
        touched: &RwLock<HashSet<Pubkey>>,
        delegated: &HashSet<Pubkey>,
    ) {
        // Unrestricted mode - no zeroing (empty delegation set means dev/test mode)
        if delegated.is_empty() {
            return;
        }

        let touched_read = touched.read().unwrap();
        let message = &tx.message;
        let static_keys = message.static_account_keys();

        // Zero writable accounts (skip fee payer at index 0, handled separately below)
        for (i, key) in static_keys.iter().enumerate() {
            // Skip fee payer (index 0) - handled separately
            if i == 0 {
                continue;
            }

            // Only process writable accounts
            if !message.is_maybe_writable(i, None) {
                continue;
            }

            // Skip delegated accounts - they keep their L1 balance
            if delegated.contains(key) {
                continue;
            }

            // Skip already-touched accounts - their balance is real
            if touched_read.contains(key) {
                continue;
            }

            // Skip infrastructure accounts
            if Self::is_infrastructure_account(key) {
                continue;
            }

            // Zero the inherited L1 balance
            if let Some(mut account) = bank.get_account(key) {
                if account.lamports() > 0 {
                    account.set_lamports(0);
                    bank.store_account(key, &account);
                }
            }
        }

        // Also zero fee payer (index 0) if untouched and not delegated
        if let Some(fee_payer) = static_keys.first() {
            if !delegated.contains(fee_payer)
                && !touched_read.contains(fee_payer)
                && !Self::is_infrastructure_account(fee_payer)
            {
                if let Some(mut account) = bank.get_account(fee_payer) {
                    if account.lamports() > 0 {
                        account.set_lamports(0);
                        bank.store_account(fee_payer, &account);
                    }
                }
            }
        }
    }

    /// Mark all writable accounts in a transaction as touched.
    fn mark_writable_as_touched(tx: &VersionedTransaction, touched: &RwLock<HashSet<Pubkey>>) {
        let mut touched_write = touched.write().unwrap();
        let message = &tx.message;
        let static_keys = message.static_account_keys();

        for (i, key) in static_keys.iter().enumerate() {
            if message.is_maybe_writable(i, None) {
                touched_write.insert(*key);
            }
        }
    }
}

impl NotifyKeyUpdate for EphemeralTransactionClient {
    fn update_key(&self, _key: &Keypair) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, solana_account::AccountSharedData, solana_sdk_ids::system_program};

    fn create_test_bank() -> solana_runtime::bank::Bank {
        use solana_genesis_config::GenesisConfig;
        let genesis_config = GenesisConfig::new(&[], &[]);
        solana_runtime::bank::Bank::new_for_tests(&genesis_config)
    }

    fn fund_account(bank: &solana_runtime::bank::Bank, pubkey: &Pubkey, lamports: u64) {
        let account = AccountSharedData::new(lamports, 0, &system_program::id());
        bank.store_account(pubkey, &account);
    }

    #[test]
    fn test_untouched_account_zeroed_before_execution() {
        let bank = create_test_bank();
        let delegated_pubkey = Pubkey::new_unique();

        // Create a delegated account owned by a different program
        let delegated_account = AccountSharedData::new(1_000_000, 0, &Pubkey::new_unique());
        bank.store_account(&delegated_pubkey, &delegated_account);

        // Create a regular user account with 100 SOL
        let user_pubkey = Pubkey::new_unique();
        fund_account(&bank, &user_pubkey, 100_000_000_000);

        let delegated_set: Arc<HashSet<Pubkey>> =
            Arc::new(vec![delegated_pubkey].into_iter().collect());
        let touched = Arc::new(RwLock::new(HashSet::new()));

        // Create a test transaction that transfers from user
        use {
            solana_message::Message, solana_system_interface::instruction::transfer,
            solana_transaction::Transaction,
        };

        let blockhash = bank.last_blockhash();
        let ix = transfer(&user_pubkey, &Pubkey::new_unique(), 1_000_000_000);
        let tx = Transaction::new_unsigned(Message::new_with_blockhash(
            &[ix],
            Some(&user_pubkey),
            &blockhash,
        ));
        let tx = VersionedTransaction::from(tx);

        // Zero untouched accounts
        EphemeralTransactionClient::zero_untouched_writable_accounts(
            &bank,
            &tx,
            &touched,
            &delegated_set,
        );

        // Verify user's balance is zeroed (or account is removed/zeroed)
        // When lamports is set to 0, some banks may remove the account
        let user_account_opt = bank.get_account(&user_pubkey);
        if let Some(user_account) = user_account_opt {
            assert_eq!(
                user_account.lamports(),
                0,
                "Untouched account should be zeroed"
            );
        }
        // If account is None, it was removed (also acceptable - 0 lamports means deleted)
    }

    #[test]
    fn test_touched_account_keeps_balance() {
        let bank = create_test_bank();
        let delegated_pubkey = Pubkey::new_unique();

        // Create a delegated account
        let delegated_account = AccountSharedData::new(1_000_000, 0, &Pubkey::new_unique());
        bank.store_account(&delegated_pubkey, &delegated_account);

        // Create a regular user account with 100 SOL
        let user_pubkey = Pubkey::new_unique();
        fund_account(&bank, &user_pubkey, 100_000_000_000);

        let delegated_set: Arc<HashSet<Pubkey>> =
            Arc::new(vec![delegated_pubkey].into_iter().collect());
        let touched = Arc::new(RwLock::new(HashSet::new()));

        // Mark the user account as touched
        touched.write().unwrap().insert(user_pubkey);

        // Create a test transaction
        use {
            solana_message::Message, solana_system_interface::instruction::transfer,
            solana_transaction::Transaction,
        };

        let blockhash = bank.last_blockhash();
        let ix = transfer(&user_pubkey, &Pubkey::new_unique(), 1_000_000_000);
        let tx = Transaction::new_unsigned(Message::new_with_blockhash(
            &[ix],
            Some(&user_pubkey),
            &blockhash,
        ));
        let tx = VersionedTransaction::from(tx);

        // Zero untouched accounts
        EphemeralTransactionClient::zero_untouched_writable_accounts(
            &bank,
            &tx,
            &touched,
            &delegated_set,
        );

        // Verify user's balance is preserved
        let user_account = bank.get_account(&user_pubkey).unwrap();
        assert_eq!(
            user_account.lamports(),
            100_000_000_000,
            "Touched account should keep balance"
        );
    }

    #[test]
    fn test_delegated_account_not_zeroed() {
        let bank = create_test_bank();
        let delegated_pubkey = Pubkey::new_unique();

        // Create a delegated account with 50 SOL
        let delegated_account = AccountSharedData::new(50_000_000_000, 0, &Pubkey::new_unique());
        bank.store_account(&delegated_pubkey, &delegated_account);

        let delegated_set: Arc<HashSet<Pubkey>> =
            Arc::new(vec![delegated_pubkey].into_iter().collect());
        let touched = Arc::new(RwLock::new(HashSet::new()));

        // Create a test transaction that uses the delegated account as writable
        use {solana_message::Message, solana_transaction::Transaction};

        let blockhash = bank.last_blockhash();
        // Create a simple transaction with the delegated account as the fee payer (writable)
        let tx = Transaction::new_unsigned(Message::new_with_blockhash(
            &[],
            Some(&delegated_pubkey),
            &blockhash,
        ));
        let tx = VersionedTransaction::from(tx);

        // Zero untouched accounts
        EphemeralTransactionClient::zero_untouched_writable_accounts(
            &bank,
            &tx,
            &touched,
            &delegated_set,
        );

        // Verify delegated account keeps its balance
        let delegated_account = bank.get_account(&delegated_pubkey).unwrap();
        assert_eq!(
            delegated_account.lamports(),
            50_000_000_000,
            "Delegated account should keep balance"
        );
    }

    #[test]
    fn test_unrestricted_mode_skips_zeroing() {
        let bank = create_test_bank();

        // Create a regular user account with 100 SOL
        let user_pubkey = Pubkey::new_unique();
        fund_account(&bank, &user_pubkey, 100_000_000_000);

        // Empty delegated set = unrestricted mode
        let delegated_set: Arc<HashSet<Pubkey>> = Arc::new(HashSet::new());
        let touched = Arc::new(RwLock::new(HashSet::new()));

        // Create a test transaction
        use {
            solana_message::Message, solana_system_interface::instruction::transfer,
            solana_transaction::Transaction,
        };

        let blockhash = bank.last_blockhash();
        let ix = transfer(&user_pubkey, &Pubkey::new_unique(), 1_000_000_000);
        let tx = Transaction::new_unsigned(Message::new_with_blockhash(
            &[ix],
            Some(&user_pubkey),
            &blockhash,
        ));
        let tx = VersionedTransaction::from(tx);

        // Zero untouched accounts
        EphemeralTransactionClient::zero_untouched_writable_accounts(
            &bank,
            &tx,
            &touched,
            &delegated_set,
        );

        // Verify user's balance is preserved (no zeroing in unrestricted mode)
        let user_account = bank.get_account(&user_pubkey).unwrap();
        assert_eq!(
            user_account.lamports(),
            100_000_000_000,
            "Account should keep balance in unrestricted mode"
        );
    }

    #[test]
    fn test_infrastructure_account_not_zeroed() {
        let bank = create_test_bank();

        // Create a delegated set to enable zeroing
        let delegated_pubkey = Pubkey::new_unique();
        let delegated_set: Arc<HashSet<Pubkey>> =
            Arc::new(vec![delegated_pubkey].into_iter().collect());
        let touched: Arc<RwLock<HashSet<Pubkey>>> = Arc::new(RwLock::new(HashSet::new()));

        // Test system program - should not be zeroed even if it were writable
        let system_program_id = system_program::id();
        assert!(
            EphemeralTransactionClient::is_infrastructure_account(&system_program_id),
            "System program should be infrastructure"
        );

        // Test that infrastructure check works correctly
        let test_key = Pubkey::new_unique();
        assert!(
            !EphemeralTransactionClient::is_infrastructure_account(&test_key),
            "Regular key should not be infrastructure"
        );
    }
}
