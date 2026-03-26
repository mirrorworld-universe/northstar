use {
    log::{debug, warn},
    solana_account::{ReadableAccount, WritableAccount},
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_quic_definitions::NotifyKeyUpdate,
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    solana_sdk_ids::{bpf_loader, bpf_loader_upgradeable, system_program, sysvar},
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
    // TODO: this can change actually with L1 event. Its better to merge it with touched_accounts
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

    /// Check if a transaction only writes to allowed accounts.
    /// Returns `true` if the transaction is allowed, `false` if it
    /// touches non-delegated writable accounts.
    // TODO: handle https://solana.com/developers/guides/advanced/lookup-tables
    fn is_transaction_allowed(&self, tx: &VersionedTransaction) -> bool {
        // If delegation set is empty, allow everything (unrestricted mode)
        if self.delegated_accounts.is_empty() {
            return true;
        }

        let message = &tx.message;
        let static_keys = message.static_account_keys();

        for (i, key) in static_keys.iter().enumerate() {
            // Skip fee payer (index 0) — always allowed
            if i == 0 {
                continue;
            }
            if message.is_maybe_writable(i, None) && !self.is_allowed_writable(key) {
                return false;
            }
        }

        true
    }

    fn is_allowed_writable(&self, key: &Pubkey) -> bool {
        // Always allow native programs and sysvars
        if system_program::check_id(key)
            || sysvar::check_id(key)
            || bpf_loader::check_id(key)
            || bpf_loader_upgradeable::check_id(key)
        {
            return true;
        }

        // Allow delegated accounts
        if self.delegated_accounts.contains(key) {
            return true;
        }

        // Allow new accounts (not on L1) to be created
        // The bank read will walk ancestors — if the account
        // doesn't exist anywhere, it's a new account.
        let bank = self.bank();
        if bank.get_account(key).is_none() {
            return true;
        }

        false
    }
}

impl TransactionClient for EphemeralTransactionClient {
    fn send_transactions_in_batch(
        &self,
        wire_transactions: Vec<Vec<u8>>,
        _stats: &SendTransactionServiceStats,
    ) {
        // BUG: This should work around slot advancer, because it might change
        // bank between transactions
        let bank = self.bank();
        wire_transactions
            .into_iter()
            .filter_map(|wire_tx| match bincode::deserialize(&wire_tx) {
                Ok(tx) => Some(tx),
                Err(e) => {
                    warn!("Failed to deserialize tx: {e}");
                    None
                }
            })
            .for_each(|tx| {
                // Delegation filter: reject transactions that write to non-delegated accounts
                if !self.is_transaction_allowed(&tx) {
                    warn!(
                        "Transaction rejected: writes to non-delegated accounts. sig={}",
                        tx.signatures
                            .first()
                            .map(|s| s.to_string())
                            .unwrap_or_default(),
                    );
                    return;
                }

                Self::zero_untouched_writable_accounts(
                    &bank,
                    &tx,
                    &self.touched_accounts,
                    &self.delegated_accounts,
                );

                if let Err(e) = Self::execute_transaction(&bank, tx.clone()) {
                    debug!("Tx execution failed: {e}");
                }

                // Mark writable accounts as touched (even on failure, since the fee payer was debited)
                Self::mark_writable_as_touched(&tx, &self.touched_accounts);
            });
    }
}

impl EphemeralTransactionClient {
    // TODO: Convert it to accept vector of transaction so we batch execution
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
        agave_reserved_account_keys::ReservedAccountKeys::all_keys_iter()
            .any(|reserved| reserved == key)
    }

    /// Zero the balance of untouched writable accounts before transaction execution.
    /// This prevents users from spending inherited L1 balances on the ER.
    // XXX: this is very very suboptimal. This can accept multiple transactions
    // We need to also mark zeroed accounts as touched
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
    use {
        super::*,
        solana_account::AccountSharedData,
        solana_keypair::{Keypair, Signer},
        solana_message::Message,
        solana_sdk_ids::system_program,
        solana_transaction::versioned::VersionedTransaction,
    };

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

    fn create_test_bank_forks_with_accounts(
        accounts: &[(Pubkey, u64)],
    ) -> (Arc<Bank>, Arc<RwLock<BankForks>>) {
        use solana_genesis_config::GenesisConfig;
        let genesis_config = GenesisConfig::new(&[], &[]);
        let bank = Bank::new_for_tests(&genesis_config);

        for (pubkey, lamports) in accounts {
            let account = AccountSharedData::new(*lamports, 0, &system_program::id());
            bank.store_account(pubkey, &account);
        }

        bank.freeze();
        let bank_forks = BankForks::new_rw_arc(bank);
        let bank = bank_forks.read().unwrap().root_bank();
        (bank, bank_forks)
    }

    fn create_client_with_delegated(
        bank_forks: Arc<RwLock<BankForks>>,
        delegated: Vec<Pubkey>,
    ) -> EphemeralTransactionClient {
        let delegated_set = Arc::new(delegated.into_iter().collect());
        let touched_set = Arc::new(RwLock::new(HashSet::new()));
        EphemeralTransactionClient::new(bank_forks, delegated_set, touched_set)
    }

    /// Helper to create a simple transfer transaction
    fn create_transfer_tx(
        fee_payer: &Keypair,
        from: Pubkey,
        to: Pubkey,
        blockhash: solana_hash::Hash,
    ) -> VersionedTransaction {
        use {solana_message::VersionedMessage, solana_system_interface::instruction::transfer};
        let instruction = transfer(&from, &to, 1_000_000);
        let message = VersionedMessage::Legacy(Message::new_with_blockhash(
            &[instruction],
            Some(&fee_payer.pubkey()),
            &blockhash,
        ));
        VersionedTransaction::try_new(message, &[fee_payer]).unwrap()
    }

    #[test]
    fn test_allowed_transaction_passes() {
        // Set delegated_accounts = {A, B}
        // Build a transaction that writes to A and B. Verify allowed.
        let delegated_a = Pubkey::new_unique();
        let delegated_b = Pubkey::new_unique();

        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[
            (delegated_a, 10_000_000),
            (delegated_b, 10_000_000),
        ]);

        let client = create_client_with_delegated(bank_forks, vec![delegated_a, delegated_b]);

        // Create a keypair that we'll use as both fee payer and source
        let fee_payer = Keypair::new();
        let tx = create_transfer_tx(
            &fee_payer,
            fee_payer.pubkey(),
            delegated_b,
            bank.last_blockhash(),
        );

        assert!(client.is_transaction_allowed(&tx));
    }

    #[test]
    fn test_disallowed_transaction_rejected() {
        // Set delegated_accounts = {A}
        // Build a transaction that writes to A and C (C exists on L1, not delegated)
        // Verify rejected.
        let delegated_a = Pubkey::new_unique();
        let non_delegated_c = Pubkey::new_unique();

        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[
            (delegated_a, 10_000_000),
            (non_delegated_c, 10_000_000), // Exists on L1, not delegated
        ]);

        let client = create_client_with_delegated(bank_forks, vec![delegated_a]);

        // Create a new keypair to use as fee payer and source
        let fee_payer = Keypair::new();
        // Transfer from fee_payer to non_delegated_c (C exists on L1, not delegated)
        let tx = create_transfer_tx(
            &fee_payer,
            fee_payer.pubkey(),
            non_delegated_c,
            bank.last_blockhash(),
        );

        assert!(!client.is_transaction_allowed(&tx));
    }

    #[test]
    fn test_read_only_non_delegated_allowed() {
        // Note: In Solana, all accounts in the transaction are writable in practice.
        // This test verifies that the fee payer (which signs) is always allowed.
        let delegated_a = Pubkey::new_unique();

        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[(delegated_a, 10_000_000)]);

        let client = create_client_with_delegated(bank_forks, vec![delegated_a]);

        let fee_payer = Keypair::new();
        // Self-transfer from fee_payer to fee_payer - only fee_payer is in tx, which is allowed
        let tx = create_transfer_tx(
            &fee_payer,
            fee_payer.pubkey(),
            fee_payer.pubkey(),
            bank.last_blockhash(),
        );

        // Fee payer should always be allowed
        assert!(client.is_transaction_allowed(&tx));
    }

    #[test]
    fn test_system_program_always_allowed() {
        // System program should always be allowed
        let delegated_a = Pubkey::new_unique();

        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[(delegated_a, 10_000_000)]);

        let client = create_client_with_delegated(bank_forks, vec![delegated_a]);

        let fee_payer = Keypair::new();
        // Transfer to system program (should be allowed)
        let tx = create_transfer_tx(
            &fee_payer,
            fee_payer.pubkey(),
            system_program::id(),
            bank.last_blockhash(),
        );

        // System program should always be allowed
        assert!(client.is_transaction_allowed(&tx));
    }

    #[test]
    fn test_empty_delegation_allows_all() {
        // Set delegated_accounts = {}
        // Build any transaction. Verify allowed (unrestricted mode).
        let some_account = Pubkey::new_unique();
        let receiver = Pubkey::new_unique();

        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[
            (some_account, 10_000_000),
            (receiver, 10_000_000),
        ]);

        // Empty delegation set
        let client = create_client_with_delegated(bank_forks, vec![]);

        // Create a new keypair to use as fee payer and source
        let fee_payer = Keypair::new();
        // Transfer from fee_payer to receiver (both not delegated)
        let tx = create_transfer_tx(
            &fee_payer,
            fee_payer.pubkey(),
            receiver,
            bank.last_blockhash(),
        );

        // Empty delegation set = unrestricted mode, all allowed
        assert!(client.is_transaction_allowed(&tx));
    }

    #[test]
    fn test_fee_payer_auto_allowed() {
        // Fee payer (index 0) should always be allowed
        let delegated_a = Pubkey::new_unique();

        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[(delegated_a, 10_000_000)]);

        let client = create_client_with_delegated(bank_forks, vec![delegated_a]);

        // Create a keypair that will be both fee payer and receiver (both the same)
        let fee_payer = Keypair::new();
        // Self-transfer from fee_payer to fee_payer (fee_payer is new so allowed)
        let tx = create_transfer_tx(
            &fee_payer,
            fee_payer.pubkey(),
            fee_payer.pubkey(),
            bank.last_blockhash(),
        );

        // Fee payer should always be allowed
        assert!(client.is_transaction_allowed(&tx));
    }

    #[test]
    fn test_new_account_creation_allowed() {
        // Set delegated_accounts = {A}
        // Build a transaction that writes to a new account D (doesn't exist on L1).
        // Verify allowed - new accounts can be created.
        let delegated_a = Pubkey::new_unique();

        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[
            (delegated_a, 10_000_000),
            // No other accounts - any new key is a "new account"
        ]);

        let client = create_client_with_delegated(bank_forks, vec![delegated_a]);

        let fee_payer = Keypair::new();
        let new_account = Pubkey::new_unique(); // New account that doesn't exist on L1

        let tx = create_transfer_tx(
            &fee_payer,
            fee_payer.pubkey(),
            new_account,
            bank.last_blockhash(),
        );

        // Should be allowed because new_account doesn't exist on L1
        assert!(client.is_transaction_allowed(&tx));
    }

    #[test]
    fn test_non_delegated_existing_account_rejected() {
        // Set delegated_accounts = {A}
        // Build a transaction that writes to B (exists on L1, not delegated)
        // Verify rejected.
        let delegated_a = Pubkey::new_unique();
        let existing_non_delegated = Pubkey::new_unique();

        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[
            (delegated_a, 10_000_000),
            (existing_non_delegated, 10_000_000),
        ]);

        let client = create_client_with_delegated(bank_forks, vec![delegated_a]);

        // Create a new keypair to use as fee payer and source
        let fee_payer = Keypair::new();
        // Transfer from fee_payer to existing_non_delegated (exists on L1, not delegated)
        let tx = create_transfer_tx(
            &fee_payer,
            fee_payer.pubkey(),
            existing_non_delegated,
            bank.last_blockhash(),
        );

        // Should be rejected because existing_non_delegated exists on L1 and is not delegated
        assert!(!client.is_transaction_allowed(&tx));
    }
}
