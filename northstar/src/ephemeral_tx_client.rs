use {
    log::{debug, trace, warn},
    solana_account::{AccountSharedData, ReadableAccount, WritableAccount},
    solana_keypair::Keypair,
    solana_ledger::transaction_balances::compile_collected_balances,
    solana_message::{v0::LoadedAddresses, AddressLoader, VersionedMessage},
    solana_pubkey::Pubkey,
    solana_rpc::{er_history::ErHistoryStore, rpc_subscriptions::RpcSubscriptions},
    solana_runtime::{
        bank::Bank,
        bank_forks::BankForks,
        commitment::{BlockCommitmentCache, CommitmentSlots},
    },
    solana_sdk_ids::{bpf_loader, bpf_loader_upgradeable, system_program, sysvar},
    solana_send_transaction_service::{
        send_transaction_service_stats::SendTransactionServiceStats,
        transaction_client::TransactionClient,
    },
    solana_svm::{
        transaction_balances::BalanceCollector, transaction_commit_result::TransactionCommitResult,
        transaction_processor::ExecutionRecordingConfig,
    },
    solana_svm_timings::ExecuteTimings,
    solana_tls_utils::NotifyKeyUpdate,
    solana_transaction::versioned::VersionedTransaction,
    solana_transaction_status::{
        map_inner_instructions, TransactionStatusMeta, VersionedTransactionWithStatusMeta,
    },
    std::{
        collections::{HashMap, HashSet},
        error::Error,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex, RwLock,
        },
    },
};

pub struct EphemeralTransactionClient {
    bank_forks: Arc<RwLock<BankForks>>,
    /// Serializes ER bank mutations with SlotAdvancer and deposit/delegation writes.
    bank_operation_lock: Arc<Mutex<()>>,
    /// Set of delegated account pubkeys for filtering.
    /// Wrapped in RwLock because new delegations can arrive from L1 at runtime.
    delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    /// Accounts that have been written to on this ER.
    /// Once touched, their balance is "real" (not inherited from L1).
    touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    /// Sonic: Thin in-memory ER account overlay. This is the source of truth
    /// for ER-local writes across L1 reanchors; the current Bank is rehydrated
    /// from it so existing SVM/RPC paths keep working.
    er_account_overlay: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
    /// When false, all transactions are rejected.
    /// Shared with EphemeralRuntime — set to true when a session is active.
    active: Arc<AtomicBool>,
    /// In-memory ER transaction history shared with RPC handlers.
    er_history_store: Arc<ErHistoryStore>,
    /// ER PubSub notifier. Wired after `RpcSubscriptions` is constructed.
    rpc_subscriptions: Arc<RwLock<Option<Arc<RpcSubscriptions>>>>,
    /// ER RPC commitment cache.
    /// Transaction execution updates only the processed slot; confirmed/finalized
    /// continue to advance through the slot advancer's frozen-bank path.
    block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
    transaction_max_age: usize,
}

impl Clone for EphemeralTransactionClient {
    fn clone(&self) -> Self {
        Self {
            bank_forks: Arc::clone(&self.bank_forks),
            bank_operation_lock: Arc::clone(&self.bank_operation_lock),
            delegated_accounts: Arc::clone(&self.delegated_accounts),
            touched_accounts: Arc::clone(&self.touched_accounts),
            er_account_overlay: Arc::clone(&self.er_account_overlay),
            active: Arc::clone(&self.active),
            er_history_store: Arc::clone(&self.er_history_store),
            rpc_subscriptions: Arc::clone(&self.rpc_subscriptions),
            block_commitment_cache: Arc::clone(&self.block_commitment_cache),
            transaction_max_age: self.transaction_max_age,
        }
    }
}

impl EphemeralTransactionClient {
    pub fn new(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        active: Arc<AtomicBool>,
    ) -> Self {
        Self::new_with_history_and_overlay(
            bank_forks,
            bank_operation_lock,
            delegated_accounts,
            touched_accounts,
            active,
            Arc::new(RwLock::new(HashMap::new())),
        )
    }

    pub(crate) fn new_with_history_and_overlay(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        active: Arc<AtomicBool>,
        er_account_overlay: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
    ) -> Self {
        Self::new_with_history_overlay(
            bank_forks,
            bank_operation_lock,
            delegated_accounts,
            touched_accounts,
            active,
            er_account_overlay,
            Arc::new(ErHistoryStore::default()),
        )
    }

    pub fn new_with_history(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        active: Arc<AtomicBool>,
        er_history_store: Arc<ErHistoryStore>,
    ) -> Self {
        Self::new_with_history_overlay(
            bank_forks,
            bank_operation_lock,
            delegated_accounts,
            touched_accounts,
            active,
            Arc::new(RwLock::new(HashMap::new())),
            er_history_store,
        )
    }

    pub(crate) fn new_with_history_overlay(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        active: Arc<AtomicBool>,
        er_account_overlay: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
        er_history_store: Arc<ErHistoryStore>,
    ) -> Self {
        let block_commitment_cache = Self::new_block_commitment_cache_for_bank_forks(&bank_forks);
        Self::new_with_history_overlay_and_commitment_cache(
            bank_forks,
            bank_operation_lock,
            delegated_accounts,
            touched_accounts,
            active,
            er_account_overlay,
            er_history_store,
            block_commitment_cache,
        )
    }

    pub fn new_with_history_and_commitment_cache(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        active: Arc<AtomicBool>,
        er_history_store: Arc<ErHistoryStore>,
        block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
    ) -> Self {
        Self::new_with_history_overlay_and_commitment_cache(
            bank_forks,
            bank_operation_lock,
            delegated_accounts,
            touched_accounts,
            active,
            Arc::new(RwLock::new(HashMap::new())),
            er_history_store,
            block_commitment_cache,
        )
    }

    pub(crate) fn new_with_history_overlay_and_commitment_cache(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        active: Arc<AtomicBool>,
        er_account_overlay: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
        er_history_store: Arc<ErHistoryStore>,
        block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
    ) -> Self {
        Self::new_with_history_overlay_commitment_cache_and_transaction_max_age(
            bank_forks,
            bank_operation_lock,
            delegated_accounts,
            touched_accounts,
            active,
            er_account_overlay,
            er_history_store,
            block_commitment_cache,
            crate::DEFAULT_ER_TRANSACTION_MAX_AGE,
        )
    }

    pub(crate) fn new_with_history_overlay_commitment_cache_and_transaction_max_age(
        bank_forks: Arc<RwLock<BankForks>>,
        bank_operation_lock: Arc<Mutex<()>>,
        delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        touched_accounts: Arc<RwLock<HashSet<Pubkey>>>,
        active: Arc<AtomicBool>,
        er_account_overlay: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
        er_history_store: Arc<ErHistoryStore>,
        block_commitment_cache: Arc<RwLock<BlockCommitmentCache>>,
        transaction_max_age: usize,
    ) -> Self {
        Self {
            bank_forks,
            bank_operation_lock,
            delegated_accounts,
            touched_accounts,
            er_account_overlay,
            active,
            er_history_store,
            rpc_subscriptions: Arc::new(RwLock::new(None)),
            block_commitment_cache,
            transaction_max_age,
        }
    }

    fn new_block_commitment_cache_for_bank_forks(
        bank_forks: &Arc<RwLock<BankForks>>,
    ) -> Arc<RwLock<BlockCommitmentCache>> {
        let slot = bank_forks.read().unwrap().working_bank().slot();
        Arc::new(RwLock::new(BlockCommitmentCache::new(
            HashMap::new(),
            0,
            CommitmentSlots {
                slot,
                root: slot,
                highest_confirmed_slot: slot,
                highest_super_majority_root: slot,
            },
        )))
    }

    pub fn set_rpc_subscriptions(&self, rpc_subscriptions: Arc<RpcSubscriptions>) {
        *self.rpc_subscriptions.write().unwrap() = Some(rpc_subscriptions);
    }

    pub fn bank(&self) -> Arc<Bank> {
        self.bank_forks.read().unwrap().working_bank()
    }

    /// Check if a transaction only writes to allowed accounts.
    /// Returns `true` if the transaction is allowed, `false` if it
    /// touches non-delegated writable accounts.
    fn is_transaction_allowed_on_bank(
        bank: &Bank,
        tx: &VersionedTransaction,
        delegated_accounts: &HashSet<Pubkey>,
        touched_accounts: &HashSet<Pubkey>,
    ) -> bool {
        // If delegation set is empty, allow everything (unrestricted mode)
        if delegated_accounts.is_empty() {
            return true;
        }

        let loaded_addresses = match Self::load_transaction_addresses(bank, tx) {
            Some(loaded_addresses) => loaded_addresses,
            None => return false,
        };

        let message = &tx.message;
        let static_keys = message.static_account_keys();

        for (i, key) in static_keys.iter().enumerate() {
            // Skip fee payer (index 0) — always allowed
            if i == 0 {
                continue;
            }
            if message.is_maybe_writable(i, None)
                && !Self::is_allowed_writable_on_bank(
                    bank,
                    key,
                    delegated_accounts,
                    touched_accounts,
                )
            {
                return false;
            }
        }

        if !loaded_addresses.writable.iter().all(|key| {
            Self::is_allowed_writable_on_bank(bank, key, delegated_accounts, touched_accounts)
        }) {
            return false;
        }

        true
    }

    fn is_allowed_writable_on_bank(
        bank: &Bank,
        key: &Pubkey,
        delegated_accounts: &HashSet<Pubkey>,
        touched_accounts: &HashSet<Pubkey>,
    ) -> bool {
        // Always allow native programs and sysvars
        if system_program::check_id(key)
            || sysvar::check_id(key)
            || bpf_loader::check_id(key)
            || bpf_loader_upgradeable::check_id(key)
        {
            return true;
        }

        // Allow delegated accounts and ER bridge accounts materialized by
        // deposit/withdrawal setup.
        if delegated_accounts.contains(key) || touched_accounts.contains(key) {
            return true;
        }

        // Allow new accounts (not on L1) to be created
        // The bank read will walk ancestors — if the account
        // doesn't exist anywhere, it's a new account.
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
        // Sonic: Reject all transactions when ephemeral rollup session is not active
        if !self.active.load(Ordering::Relaxed) {
            warn!(
                "Ephemeral rollup not active, rejecting {} transaction(s)",
                wire_transactions.len()
            );
            return;
        }
        let txs: Vec<VersionedTransaction> = wire_transactions
            .into_iter()
            .filter_map(|wire_tx| match bincode::deserialize(&wire_tx) {
                Ok(tx) => Some(tx),
                Err(e) => {
                    warn!("Failed to deserialize tx: {e}");
                    None
                }
            })
            .collect();

        if txs.is_empty() {
            return;
        }

        let _bank_operation_guard = self.bank_operation_lock.lock().unwrap();
        let bank = self.bank();
        let delegated_accounts = self.delegated_accounts.read().unwrap().clone();
        let touched_accounts = self.touched_accounts.read().unwrap().clone();
        let txs: Vec<_> = txs
            .into_iter()
            .filter(|tx| {
                let allowed = Self::is_transaction_allowed_on_bank(
                    &bank,
                    tx,
                    &delegated_accounts,
                    &touched_accounts,
                );
                if !allowed {
                    warn!(
                        "Transaction rejected: writes to non-delegated accounts. sig={}",
                        tx.signatures
                            .first()
                            .map(|s| s.to_string())
                            .unwrap_or_default(),
                    );
                }
                allowed
            })
            .collect();

        if txs.is_empty() {
            return;
        }

        let writable_accounts = Self::writable_accounts_for_batch(&bank, &txs);

        Self::zero_untouched_writable_accounts_for_batch(
            &bank,
            &txs,
            &self.touched_accounts,
            &delegated_accounts,
        );

        if let Err(e) = self.execute_transactions(&bank, txs.clone()) {
            warn!("ER tx batch execution failed: {e}");
        }

        self.capture_overlay_accounts(&bank, &writable_accounts);
        self.publish_processed_slot(&bank);

        // Mark writable accounts as touched (even on failure, since fee payers may be debited)
        Self::mark_writable_as_touched_for_batch(&txs, &self.touched_accounts);
    }
}

impl EphemeralTransactionClient {
    fn capture_overlay_accounts(&self, bank: &Bank, writable_accounts: &HashSet<Pubkey>) {
        if writable_accounts.is_empty() {
            return;
        }

        let overlay_updates = writable_accounts
            .iter()
            .map(|key| (*key, bank.get_account(key).unwrap_or_default()))
            .collect::<Vec<_>>();
        self.er_account_overlay
            .write()
            .unwrap()
            .extend(overlay_updates);
    }

    fn publish_processed_slot(&self, bank: &Bank) {
        let current_slots = self
            .block_commitment_cache
            .read()
            .unwrap()
            .commitment_slots();
        *self.block_commitment_cache.write().unwrap() = BlockCommitmentCache::new(
            HashMap::new(),
            0,
            CommitmentSlots {
                slot: bank.slot(),
                ..current_slots
            },
        );
    }

    fn execute_transactions(
        &self,
        bank: &Bank,
        txs: Vec<VersionedTransaction>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let tx_sigs = txs
            .iter()
            .map(|tx| {
                tx.signatures
                    .first()
                    .map(|sig| sig.to_string())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        if log::log_enabled!(log::Level::Trace) {
            let tx_programs = txs
                .iter()
                .map(|tx| {
                    tx.message
                        .instructions()
                        .iter()
                        .filter_map(|ix| {
                            tx.message
                                .static_account_keys()
                                .get(ix.program_id_index as usize)
                        })
                        .map(|program_id| program_id.to_string())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            trace!(
                "ER tx batch executing: slot={}, count={}, sigs={tx_sigs:?}, \
                 programs={tx_programs:?}",
                bank.slot(),
                txs.len(),
            );
        } else {
            debug!(
                "ER tx batch executing: slot={}, count={}, sigs={tx_sigs:?}",
                bank.slot(),
                txs.len(),
            );
        }

        let (valid_txs, expired_txs): (Vec<_>, Vec<_>) = txs.into_iter().partition(|tx| {
            let recent_blockhash = tx.message.recent_blockhash();
            bank.get_hash_age(recent_blockhash)
                .is_some_and(|age| age <= self.transaction_max_age as u64)
        });

        for tx in expired_txs {
            self.record_failed_transaction(
                bank,
                tx,
                solana_transaction::TransactionError::BlockhashNotFound,
            );
        }

        if valid_txs.is_empty() {
            return Err(solana_transaction::TransactionError::BlockhashNotFound.into());
        }

        let txs = valid_txs;

        let batch = match bank.prepare_entry_batch(txs.clone()) {
            Ok(batch) => batch,
            Err(e) => {
                warn!("ER tx batch preparation failed: {e}; sigs={tx_sigs:?}");
                // Record each failed transaction so callers can discover the error
                // via getSignatureStatuses / getTransaction instead of getting null.
                for tx in &txs {
                    self.record_failed_transaction(bank, tx.clone(), e.clone());
                }
                return Err(e.into());
            }
        };
        let (commit_results, balance_collector) = bank.load_execute_and_commit_transactions(
            &batch,
            Self::history_recording_config(),
            &mut ExecuteTimings::default(),
            None,
        );

        self.record_transaction_history_for_batch(bank, &txs, &commit_results, balance_collector);
        self.notify_transaction_subscribers(bank, &txs);

        for (tx_idx, result) in commit_results.iter().enumerate() {
            if let Err(e) = result {
                warn!(
                    "ER tx failed: index={tx_idx}, sig={}, err={e}",
                    tx_sigs.get(tx_idx).map(String::as_str).unwrap_or_default()
                );
            }
        }

        Ok(())
    }

    /// Record a transaction that failed before it could even be prepared
    /// (e.g. expired blockhash, sanitization error). Without this, callers
    /// who received the signature from `sendTransaction` would get `null`
    /// from `getSignatureStatuses` / `getTransaction` with no way to know
    /// what happened.
    fn record_failed_transaction(
        &self,
        bank: &Bank,
        tx: VersionedTransaction,
        err: solana_transaction::TransactionError,
    ) {
        let loaded_addresses = Self::load_transaction_addresses(bank, &tx).unwrap_or_default();
        let meta = TransactionStatusMeta {
            status: Err(err),
            fee: 0,
            pre_balances: vec![],
            post_balances: vec![],
            inner_instructions: None,
            log_messages: None,
            pre_token_balances: Some(vec![]),
            post_token_balances: Some(vec![]),
            rewards: Some(vec![]),
            loaded_addresses,
            return_data: None,
            compute_units_consumed: Some(0),
            cost_units: None,
        };

        let _ = self.er_history_store.record_transaction(
            bank,
            VersionedTransactionWithStatusMeta {
                transaction: tx,
                meta,
            },
        );
    }

    fn notify_transaction_subscribers(&self, bank: &Bank, txs: &[VersionedTransaction]) {
        let Some(rpc_subscriptions) = self.rpc_subscriptions.read().unwrap().clone() else {
            return;
        };
        let slot = bank.slot();
        let signatures = txs
            .iter()
            .flat_map(|tx| tx.signatures.iter().copied())
            .collect();
        rpc_subscriptions.notify_signatures_received((slot, signatures));
        rpc_subscriptions.notify_subscribers(CommitmentSlots {
            slot,
            root: slot,
            highest_confirmed_slot: slot,
            highest_super_majority_root: slot,
        });
        // Sonic: confirmed subscriptions are tracked on the gossip watcher path.
        rpc_subscriptions.notify_gossip_subscribers(slot);
    }

    fn history_recording_config() -> ExecutionRecordingConfig {
        ExecutionRecordingConfig {
            enable_cpi_recording: true,
            enable_log_recording: true,
            enable_return_data_recording: true,
            enable_transaction_balance_recording: true,
        }
    }

    fn load_transaction_addresses(
        bank: &Bank,
        tx: &VersionedTransaction,
    ) -> Option<LoadedAddresses> {
        match &tx.message {
            VersionedMessage::Legacy(_) => Some(LoadedAddresses::default()),
            VersionedMessage::V0(message) => {
                match bank.load_addresses(&message.address_table_lookups) {
                    Ok(loaded_addresses) => Some(loaded_addresses),
                    Err(err) => {
                        warn!(
                            "Failed to resolve ALT addresses, sig={}: {err}",
                            tx.signatures
                                .first()
                                .map(|signature| signature.to_string())
                                .unwrap_or_default()
                        );
                        None
                    }
                }
            }
            // Sonic: V1 currently has no ALT lookups on the ER history path; if
            // that changes, mirror the V0 load_addresses path before recording meta.
            VersionedMessage::V1(_) => Some(LoadedAddresses::default()),
        }
    }

    fn record_transaction_history_for_batch(
        &self,
        bank: &Bank,
        txs: &[VersionedTransaction],
        commit_results: &[TransactionCommitResult],
        balance_collector: Option<BalanceCollector>,
    ) {
        let (balances, token_balances) =
            compile_collected_balances(balance_collector.unwrap_or_default());

        for (tx_idx, (tx, commit_result)) in txs.iter().zip(commit_results).enumerate() {
            let Ok(committed_tx) = commit_result else {
                // Record failed transactions so callers can discover the error
                // instead of getting a null signature status.
                let err = commit_result.clone().unwrap_err();
                let loaded_addresses =
                    Self::load_transaction_addresses(bank, tx).unwrap_or_default();
                let meta = TransactionStatusMeta {
                    status: Err(err),
                    fee: 0,
                    pre_balances: vec![],
                    post_balances: vec![],
                    inner_instructions: None,
                    log_messages: None,
                    pre_token_balances: Some(vec![]),
                    post_token_balances: Some(vec![]),
                    rewards: Some(vec![]),
                    loaded_addresses,
                    return_data: None,
                    compute_units_consumed: Some(0),
                    cost_units: None,
                };
                let _ = self.er_history_store.record_transaction(
                    bank,
                    VersionedTransactionWithStatusMeta {
                        transaction: tx.clone(),
                        meta,
                    },
                );
                continue;
            };

            let pre_balances = balances
                .pre_balances
                .get(tx_idx)
                .cloned()
                .unwrap_or_default();
            let post_balances = balances
                .post_balances
                .get(tx_idx)
                .cloned()
                .unwrap_or_default();
            let pre_token_balances = token_balances
                .pre_token_balances
                .get(tx_idx)
                .cloned()
                .unwrap_or_default();
            let post_token_balances = token_balances
                .post_token_balances
                .get(tx_idx)
                .cloned()
                .unwrap_or_default();

            let loaded_addresses = Self::load_transaction_addresses(bank, tx).unwrap_or_default();
            let meta = TransactionStatusMeta {
                status: committed_tx.status.clone(),
                fee: committed_tx.fee_details.total_fee(),
                pre_balances,
                post_balances,
                inner_instructions: committed_tx
                    .inner_instructions
                    .clone()
                    .map(|inner| map_inner_instructions(inner).collect()),
                log_messages: committed_tx.log_messages.clone(),
                pre_token_balances: Some(pre_token_balances),
                post_token_balances: Some(post_token_balances),
                rewards: Some(vec![]),
                loaded_addresses,
                return_data: committed_tx.return_data.clone(),
                compute_units_consumed: Some(committed_tx.executed_units),
                cost_units: None,
            };

            let _ = self.er_history_store.record_transaction(
                bank,
                VersionedTransactionWithStatusMeta {
                    transaction: tx.clone(),
                    meta,
                },
            );
        }
    }

    /// Check if a key is an infrastructure account (system program, sysvars, etc.)
    fn is_infrastructure_account(key: &Pubkey) -> bool {
        agave_reserved_account_keys::ReservedAccountKeys::all_keys_iter()
            .any(|reserved| reserved == key)
    }

    /// Zero the balance of untouched writable accounts before transaction execution.
    /// This prevents users from spending inherited L1 balances on the ER.
    fn zero_untouched_writable_accounts_for_batch(
        bank: &Bank,
        txs: &[VersionedTransaction],
        touched: &RwLock<HashSet<Pubkey>>,
        delegated: &HashSet<Pubkey>,
    ) {
        // Unrestricted mode - no zeroing (empty delegation set means dev/test mode)
        if delegated.is_empty() {
            return;
        }

        let touched_read = touched.read().unwrap();
        let mut accounts_to_zero = HashSet::new();

        for tx in txs {
            let message = &tx.message;
            let static_keys = message.static_account_keys();

            for (i, key) in static_keys.iter().enumerate() {
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

                // Skip accounts already queued by an earlier tx in this batch
                if accounts_to_zero.contains(key) {
                    continue;
                }

                // Skip infrastructure accounts
                if Self::is_infrastructure_account(key) {
                    continue;
                }

                accounts_to_zero.insert(*key);
            }
        }
        drop(touched_read);

        if accounts_to_zero.is_empty() {
            return;
        }

        let mut zeroed_accounts = Vec::new();
        for key in &accounts_to_zero {
            // Zero the inherited L1 balance
            if let Some(mut account) = bank.get_account(key) {
                if account.lamports() > 0 {
                    account.set_lamports(0);
                    bank.store_account(key, &account);
                    zeroed_accounts.push(*key);
                }
            }
        }

        if !zeroed_accounts.is_empty() {
            touched.write().unwrap().extend(zeroed_accounts);
        }
    }

    fn writable_accounts_for_batch(bank: &Bank, txs: &[VersionedTransaction]) -> HashSet<Pubkey> {
        txs.iter()
            .flat_map(|tx| {
                tx.message
                    .static_account_keys()
                    .iter()
                    .enumerate()
                    .filter_map(|(i, key)| tx.message.is_maybe_writable(i, None).then_some(*key))
                    .chain(
                        Self::load_transaction_addresses(bank, tx)
                            .map(|loaded_addresses| loaded_addresses.writable)
                            .unwrap_or(vec![]),
                    )
            })
            .collect()
    }

    /// Mark all writable accounts in a transaction batch as touched.
    fn mark_writable_as_touched_for_batch(
        txs: &[VersionedTransaction],
        touched: &RwLock<HashSet<Pubkey>>,
    ) {
        let mut touched_write = touched.write().unwrap();

        for tx in txs {
            let message = &tx.message;
            let static_keys = message.static_account_keys();

            for (i, key) in static_keys.iter().enumerate() {
                if message.is_maybe_writable(i, None) {
                    touched_write.insert(*key);
                }
            }
        }
    }
}

impl NotifyKeyUpdate for EphemeralTransactionClient {
    fn update_key(&self, _key: &Keypair) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}

impl solana_rpc::rpc::ErTxExecutor for EphemeralTransactionClient {
    fn execute_wire(
        &self,
        wire_transaction: Vec<u8>,
    ) -> std::result::Result<(), solana_rpc::rpc::ErTxError> {
        if !self.active.load(Ordering::Relaxed) {
            warn!("Ephemeral rollup not active, rejecting RPC transaction");
            return Err(solana_rpc::rpc::ErTxError::NotActive);
        }

        let stats = SendTransactionServiceStats::default();
        <Self as TransactionClient>::send_transactions_in_batch(
            self,
            vec![wire_transaction],
            &stats,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_account::AccountSharedData,
        solana_address_lookup_table_interface::{
            self as address_lookup_table,
            state::{AddressLookupTable, LookupTableMeta},
        },
        solana_fee_structure::FeeDetails,
        solana_keypair::{Keypair, Signer},
        solana_leader_schedule::SlotLeader,
        solana_message::{
            v0::{self, MessageAddressTableLookup},
            Message, MessageHeader,
        },
        solana_sdk_ids::system_program,
        solana_svm::transaction_execution_result::TransactionLoadedAccountsStats,
        solana_transaction::{versioned::VersionedTransaction, Transaction},
        solana_transaction_context::transaction::TransactionReturnData,
        std::{borrow::Cow, sync::Arc},
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

        let delegated_set: Arc<RwLock<HashSet<Pubkey>>> =
            Arc::new(RwLock::new(vec![delegated_pubkey].into_iter().collect()));
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
        EphemeralTransactionClient::zero_untouched_writable_accounts_for_batch(
            &bank,
            std::slice::from_ref(&tx),
            &touched,
            &delegated_set.read().unwrap(),
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

        let delegated_set: Arc<RwLock<HashSet<Pubkey>>> =
            Arc::new(RwLock::new(vec![delegated_pubkey].into_iter().collect()));
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
        EphemeralTransactionClient::zero_untouched_writable_accounts_for_batch(
            &bank,
            std::slice::from_ref(&tx),
            &touched,
            &delegated_set.read().unwrap(),
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

        let delegated_set: Arc<RwLock<HashSet<Pubkey>>> =
            Arc::new(RwLock::new(vec![delegated_pubkey].into_iter().collect()));
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
        EphemeralTransactionClient::zero_untouched_writable_accounts_for_batch(
            &bank,
            std::slice::from_ref(&tx),
            &touched,
            &delegated_set.read().unwrap(),
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
        let delegated_set: Arc<RwLock<HashSet<Pubkey>>> = Arc::new(RwLock::new(HashSet::new()));
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
        EphemeralTransactionClient::zero_untouched_writable_accounts_for_batch(
            &bank,
            std::slice::from_ref(&tx),
            &touched,
            &delegated_set.read().unwrap(),
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
    fn test_zeroing_marks_zeroed_account_touched() {
        let bank = create_test_bank();
        let delegated_pubkey = Pubkey::new_unique();
        bank.store_account(
            &delegated_pubkey,
            &AccountSharedData::new(1_000_000, 0, &Pubkey::new_unique()),
        );

        let user_pubkey = Pubkey::new_unique();
        fund_account(&bank, &user_pubkey, 100_000_000_000);

        let delegated_set: Arc<RwLock<HashSet<Pubkey>>> =
            Arc::new(RwLock::new(vec![delegated_pubkey].into_iter().collect()));
        let touched = Arc::new(RwLock::new(HashSet::new()));

        let blockhash = bank.last_blockhash();
        let ix = solana_system_interface::instruction::transfer(
            &user_pubkey,
            &Pubkey::new_unique(),
            1_000_000,
        );
        let tx = Transaction::new_unsigned(Message::new_with_blockhash(
            &[ix],
            Some(&user_pubkey),
            &blockhash,
        ));
        let tx = VersionedTransaction::from(tx);

        EphemeralTransactionClient::zero_untouched_writable_accounts_for_batch(
            &bank,
            std::slice::from_ref(&tx),
            &touched,
            &delegated_set.read().unwrap(),
        );

        assert!(
            touched.read().unwrap().contains(&user_pubkey),
            "Zeroed account should be marked touched"
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
        let delegated_set = Arc::new(RwLock::new(delegated.into_iter().collect()));
        let touched_set = Arc::new(RwLock::new(HashSet::new()));
        EphemeralTransactionClient::new(
            bank_forks,
            Arc::new(Mutex::new(())),
            delegated_set,
            touched_set,
            Arc::new(AtomicBool::new(true)),
        )
    }

    fn create_client_with_history(
        bank_forks: Arc<RwLock<BankForks>>,
        delegated: Vec<Pubkey>,
        er_history_store: Arc<ErHistoryStore>,
    ) -> EphemeralTransactionClient {
        let delegated_set = Arc::new(RwLock::new(delegated.into_iter().collect()));
        let touched_set = Arc::new(RwLock::new(HashSet::new()));
        EphemeralTransactionClient::new_with_history(
            bank_forks,
            Arc::new(Mutex::new(())),
            delegated_set,
            touched_set,
            Arc::new(AtomicBool::new(true)),
            er_history_store,
        )
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
    fn test_send_transactions_in_batch_executes_multiple_transfers_and_records_history() {
        let fee_payer_a = Keypair::new();
        let fee_payer_b = Keypair::new();
        let recipient_a = Pubkey::new_unique();
        let recipient_b = Pubkey::new_unique();
        let bank = create_test_bank();
        fund_account(&bank, &fee_payer_a.pubkey(), 10_000_000);
        fund_account(&bank, &fee_payer_b.pubkey(), 10_000_000);
        let blockhash = bank.last_blockhash();
        let bank_forks = BankForks::new_rw_arc(bank);
        let bank = bank_forks.read().unwrap().root_bank();
        let er_history_store = Arc::new(ErHistoryStore::default());
        let client = create_client_with_history(bank_forks, vec![], er_history_store.clone());

        let tx_a = create_transfer_tx(&fee_payer_a, fee_payer_a.pubkey(), recipient_a, blockhash);
        let signature_a = tx_a.signatures[0];
        let tx_b = create_transfer_tx(&fee_payer_b, fee_payer_b.pubkey(), recipient_b, blockhash);
        let signature_b = tx_b.signatures[0];

        <EphemeralTransactionClient as TransactionClient>::send_transactions_in_batch(
            &client,
            vec![
                bincode::serialize(&tx_a).unwrap(),
                bincode::serialize(&tx_b).unwrap(),
            ],
            &SendTransactionServiceStats::default(),
        );

        assert_eq!(bank.get_balance(&recipient_a), 1_000_000);
        assert_eq!(bank.get_balance(&recipient_b), 1_000_000);

        er_history_store.finalize_slot(&bank);
        assert!(
            er_history_store
                .get_transaction(
                    &signature_a,
                    solana_rpc_client_types::config::CommitmentConfig::confirmed(),
                )
                .is_some(),
            "first transaction should be recorded in ER history"
        );
        assert!(
            er_history_store
                .get_transaction(
                    &signature_b,
                    solana_rpc_client_types::config::CommitmentConfig::confirmed(),
                )
                .is_some(),
            "second transaction should be recorded in ER history"
        );
    }

    #[test]
    fn test_send_transactions_in_batch_rejects_expired_recent_blockhash() {
        let fee_payer = Keypair::new();
        let recipient = Pubkey::new_unique();
        let mut bank = create_test_bank();
        fund_account(&bank, &fee_payer.pubkey(), 10_000_000);

        let old_blockhash = bank.last_blockhash();
        let tx = create_transfer_tx(&fee_payer, fee_payer.pubkey(), recipient, old_blockhash);
        let signature = tx.signatures[0];

        // ER slots are shorter than L1 slots, so keep Solana's processing-age
        // window in wall-clock time rather than raw slot count.
        assert_eq!(
            crate::DEFAULT_ER_SLOT_DURATION,
            std::time::Duration::from_millis(50)
        );
        let recent_blockhash_max_age =
            crate::er_recent_blockhash_max_age_for_slot_duration(crate::DEFAULT_ER_SLOT_DURATION);
        bank.configure_er(
            &crate::EphemeralRollupSettings::zero_fee_structure(),
            recent_blockhash_max_age,
        );
        assert_eq!(crate::DEFAULT_ER_TRANSACTION_MAX_AGE, 1200);
        assert_eq!(recent_blockhash_max_age, 2400);
        for _ in 0..=crate::DEFAULT_ER_TRANSACTION_MAX_AGE {
            bank.register_unique_recent_blockhash_for_test();
        }
        assert!(bank.is_hash_valid_for_age(&old_blockhash, usize::MAX));
        assert!(!bank.is_hash_valid_for_age(&old_blockhash, crate::DEFAULT_ER_TRANSACTION_MAX_AGE,));

        let bank_forks = BankForks::new_rw_arc(bank);
        let bank = bank_forks.read().unwrap().root_bank();
        let er_history_store = Arc::new(ErHistoryStore::default());
        let client = create_client_with_history(bank_forks, vec![], er_history_store.clone());

        <EphemeralTransactionClient as TransactionClient>::send_transactions_in_batch(
            &client,
            vec![bincode::serialize(&tx).unwrap()],
            &SendTransactionServiceStats::default(),
        );

        assert_eq!(bank.get_balance(&recipient), 0);
        er_history_store.finalize_slot(&bank);
        let status = er_history_store
            .get_signature_status(&signature)
            .expect("expired tx should be recorded in ER history");
        assert_eq!(
            status.err,
            Some(solana_transaction::TransactionError::BlockhashNotFound),
        );
    }

    #[test]
    fn test_send_transactions_in_batch_records_expired_blockhash_and_executes_valid_siblings() {
        let expired_fee_payer = Keypair::new();
        let valid_fee_payer = Keypair::new();
        let expired_recipient = Pubkey::new_unique();
        let valid_recipient = Pubkey::new_unique();
        let mut bank = create_test_bank();
        fund_account(&bank, &expired_fee_payer.pubkey(), 10_000_000);
        fund_account(&bank, &valid_fee_payer.pubkey(), 10_000_000);

        let old_blockhash = bank.last_blockhash();
        let expired_tx = create_transfer_tx(
            &expired_fee_payer,
            expired_fee_payer.pubkey(),
            expired_recipient,
            old_blockhash,
        );
        let expired_signature = expired_tx.signatures[0];

        let recent_blockhash_max_age =
            crate::er_recent_blockhash_max_age_for_slot_duration(crate::DEFAULT_ER_SLOT_DURATION);
        bank.configure_er(
            &crate::EphemeralRollupSettings::zero_fee_structure(),
            recent_blockhash_max_age,
        );
        for _ in 0..=crate::DEFAULT_ER_TRANSACTION_MAX_AGE {
            bank.register_unique_recent_blockhash_for_test();
        }
        assert!(!bank.is_hash_valid_for_age(&old_blockhash, crate::DEFAULT_ER_TRANSACTION_MAX_AGE,));

        let valid_tx = create_transfer_tx(
            &valid_fee_payer,
            valid_fee_payer.pubkey(),
            valid_recipient,
            bank.last_blockhash(),
        );
        let valid_signature = valid_tx.signatures[0];

        let bank_forks = BankForks::new_rw_arc(bank);
        let bank = bank_forks.read().unwrap().root_bank();
        let er_history_store = Arc::new(ErHistoryStore::default());
        let client = create_client_with_history(bank_forks, vec![], er_history_store.clone());

        <EphemeralTransactionClient as TransactionClient>::send_transactions_in_batch(
            &client,
            vec![
                bincode::serialize(&expired_tx).unwrap(),
                bincode::serialize(&valid_tx).unwrap(),
            ],
            &SendTransactionServiceStats::default(),
        );

        assert_eq!(bank.get_balance(&expired_recipient), 0);
        assert_eq!(bank.get_balance(&valid_recipient), 1_000_000);
        er_history_store.finalize_slot(&bank);

        let expired_status = er_history_store
            .get_signature_status(&expired_signature)
            .expect("expired tx should be recorded in ER history");
        assert_eq!(
            expired_status.err,
            Some(solana_transaction::TransactionError::BlockhashNotFound),
        );
        let valid_status = er_history_store
            .get_signature_status(&valid_signature)
            .expect("valid sibling tx should be recorded in ER history");
        assert_eq!(valid_status.err, None);
    }

    #[test]
    fn test_record_transaction_history_preserves_logs_cpi_and_return_data() {
        let fee_payer = Keypair::new();
        let recipient = Pubkey::new_unique();
        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[
            (fee_payer.pubkey(), 10_000_000),
            (recipient, 1_000_000),
        ]);
        let er_history_store = Arc::new(ErHistoryStore::default());
        let client = create_client_with_history(bank_forks, vec![], er_history_store.clone());

        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[solana_system_interface::instruction::transfer(
                &fee_payer.pubkey(),
                &recipient,
                1,
            )],
            Some(&fee_payer.pubkey()),
            &[&fee_payer],
            blockhash,
        );
        let tx = VersionedTransaction::from(tx);
        let signature = tx.signatures[0];

        let expected_logs = vec![
            "Program 11111111111111111111111111111111 invoke [1]".to_string(),
            "Program 11111111111111111111111111111111 success".to_string(),
        ];
        let expected_inner = vec![vec![]];
        let expected_return_data = TransactionReturnData {
            program_id: Pubkey::new_unique(),
            data: vec![0xAA, 0xBB, 0xCC],
        };

        client.record_transaction_history_for_batch(
            &bank,
            std::slice::from_ref(&tx),
            &[Ok(
                solana_svm::transaction_commit_result::CommittedTransaction {
                    status: Ok(()),
                    log_messages: Some(expected_logs.clone()),
                    inner_instructions: Some(expected_inner.clone()),
                    return_data: Some(expected_return_data.clone()),
                    executed_units: 321,
                    fee_details: FeeDetails::new(5_000, 0),
                    loaded_account_stats: TransactionLoadedAccountsStats::default(),
                    fee_payer_post_balance: 9_995_000,
                },
            )],
            None,
        );

        er_history_store.finalize_slot(&bank);
        let recorded = er_history_store
            .get_transaction(
                &signature,
                solana_rpc_client_types::config::CommitmentConfig::confirmed(),
            )
            .expect("ER history should contain transaction");
        let meta = recorded
            .tx_with_meta
            .get_status_meta()
            .expect("transaction status meta should exist");

        let expected_inner = expected_inner
            .into_iter()
            .enumerate()
            .map(
                |(index, instructions)| solana_transaction_status::InnerInstructions {
                    index: index as u8,
                    instructions: instructions
                        .into_iter()
                        .map(|info| solana_transaction_status::InnerInstruction {
                            stack_height: Some(u32::from(info.stack_height)),
                            instruction: info.instruction,
                        })
                        .collect(),
                },
            )
            .filter(|inner| !inner.instructions.is_empty())
            .collect::<Vec<_>>();

        assert_eq!(meta.log_messages.as_ref(), Some(&expected_logs));
        assert_eq!(meta.inner_instructions.as_ref(), Some(&expected_inner));
        assert_eq!(meta.return_data.as_ref(), Some(&expected_return_data));
        assert_eq!(meta.compute_units_consumed, Some(321));
    }

    #[test]
    fn test_record_transaction_history_preserves_loaded_addresses() {
        let fee_payer = Keypair::new();
        let loaded_address = Pubkey::new_unique();

        use solana_genesis_config::GenesisConfig;
        let genesis_config = GenesisConfig::new(&[], &[]);
        let root_bank = Bank::new_for_tests(&genesis_config);
        let fee_payer_account = AccountSharedData::new(10_000_000, 0, &system_program::id());
        root_bank.store_account(&fee_payer.pubkey(), &fee_payer_account);

        let address_table_key = Pubkey::new_unique();
        let address_table_state = AddressLookupTable {
            meta: LookupTableMeta {
                last_extended_slot_start_index: 1,
                ..LookupTableMeta::default()
            },
            addresses: Cow::Owned(vec![loaded_address]),
        };
        let address_table_data = Arc::new(address_table_state.serialize_for_tests().unwrap());
        let address_table_account = AccountSharedData::create_from_existing_shared_data(
            root_bank.get_minimum_balance_for_rent_exemption(address_table_data.len()),
            address_table_data,
            address_lookup_table::program::id(),
            false,
            0,
        );
        root_bank.store_account(&address_table_key, &address_table_account);
        root_bank.freeze();
        let bank = Bank::new_from_parent(Arc::new(root_bank), SlotLeader::new_unique(), 1);
        let bank_forks = BankForks::new_rw_arc(bank);
        let bank = bank_forks.read().unwrap().root_bank();

        let er_history_store = Arc::new(ErHistoryStore::default());
        let client = create_client_with_history(bank_forks, vec![], er_history_store.clone());

        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(v0::Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 0,
                },
                recent_blockhash: bank.last_blockhash(),
                account_keys: vec![fee_payer.pubkey()],
                instructions: vec![],
                address_table_lookups: vec![MessageAddressTableLookup {
                    account_key: address_table_key,
                    writable_indexes: vec![0],
                    readonly_indexes: vec![],
                }],
            }),
            &[&fee_payer],
        )
        .unwrap();
        let signature = tx.signatures[0];

        client.record_transaction_history_for_batch(
            &bank,
            std::slice::from_ref(&tx),
            &[Ok(
                solana_svm::transaction_commit_result::CommittedTransaction {
                    status: Ok(()),
                    log_messages: None,
                    inner_instructions: None,
                    return_data: None,
                    executed_units: 1,
                    fee_details: FeeDetails::new(5_000, 0),
                    loaded_account_stats: TransactionLoadedAccountsStats::default(),
                    fee_payer_post_balance: 0,
                },
            )],
            None,
        );
        er_history_store.finalize_slot(&bank);

        let recorded = er_history_store
            .get_transaction(
                &signature,
                solana_rpc_client_types::config::CommitmentConfig::confirmed(),
            )
            .expect("ER history should contain transaction");
        let meta = recorded.tx_with_meta.get_status_meta().unwrap();
        assert_eq!(meta.loaded_addresses.writable, vec![loaded_address]);
        assert!(meta.loaded_addresses.readonly.is_empty());
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

        assert!(EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
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

        assert!(!EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
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
        assert!(EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
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
        assert!(EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
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
        assert!(EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
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
        assert!(EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
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
        assert!(EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
    }

    #[test]
    fn test_unresolvable_alt_rejected() {
        let fee_payer = Keypair::new();
        let delegated_a = Pubkey::new_unique();
        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[
            (fee_payer.pubkey(), 10_000_000),
            (delegated_a, 10_000_000),
        ]);
        let client = create_client_with_delegated(bank_forks, vec![delegated_a]);

        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(v0::Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 0,
                },
                recent_blockhash: bank.last_blockhash(),
                account_keys: vec![fee_payer.pubkey()],
                instructions: vec![],
                address_table_lookups: vec![MessageAddressTableLookup {
                    account_key: Pubkey::new_unique(),
                    writable_indexes: vec![0],
                    readonly_indexes: vec![],
                }],
            }),
            &[&fee_payer],
        )
        .unwrap();

        assert!(!EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
    }

    #[test]
    fn test_writable_alt_non_delegated_existing_account_rejected() {
        let fee_payer = Keypair::new();
        let delegated_a = Pubkey::new_unique();
        let existing_non_delegated = Pubkey::new_unique();

        use solana_genesis_config::GenesisConfig;
        let genesis_config = GenesisConfig::new(&[], &[]);
        let root_bank = Bank::new_for_tests(&genesis_config);
        fund_account(&root_bank, &fee_payer.pubkey(), 10_000_000);
        fund_account(&root_bank, &delegated_a, 10_000_000);
        fund_account(&root_bank, &existing_non_delegated, 10_000_000);

        let address_table_key = Pubkey::new_unique();
        let address_table_state = AddressLookupTable {
            meta: LookupTableMeta {
                last_extended_slot_start_index: 1,
                ..LookupTableMeta::default()
            },
            addresses: Cow::Owned(vec![existing_non_delegated]),
        };
        let address_table_data = Arc::new(address_table_state.serialize_for_tests().unwrap());
        let address_table_account = AccountSharedData::create_from_existing_shared_data(
            root_bank.get_minimum_balance_for_rent_exemption(address_table_data.len()),
            address_table_data,
            address_lookup_table::program::id(),
            false,
            0,
        );
        root_bank.store_account(&address_table_key, &address_table_account);
        root_bank.freeze();
        let bank = Bank::new_from_parent(Arc::new(root_bank), SlotLeader::new_unique(), 1);
        let bank_forks = BankForks::new_rw_arc(bank);
        let bank = bank_forks.read().unwrap().root_bank();
        let client = create_client_with_delegated(bank_forks, vec![delegated_a]);

        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(v0::Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 0,
                },
                recent_blockhash: bank.last_blockhash(),
                account_keys: vec![fee_payer.pubkey()],
                instructions: vec![],
                address_table_lookups: vec![MessageAddressTableLookup {
                    account_key: address_table_key,
                    writable_indexes: vec![0],
                    readonly_indexes: vec![],
                }],
            }),
            &[&fee_payer],
        )
        .unwrap();

        assert!(!EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
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
        assert!(!EphemeralTransactionClient::is_transaction_allowed_on_bank(
            &bank,
            &tx,
            &client.delegated_accounts.read().unwrap(),
            &HashSet::new()
        ));
    }

    #[test]
    fn test_failed_transaction_recorded_in_er_history() {
        // Regression: a transaction that fails execution (e.g. expired blockhash)
        // must still be recorded in ER history so that getSignatureStatuses
        // and getTransaction return the error instead of null.
        let fee_payer = Keypair::new();
        let recipient = Pubkey::new_unique();
        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[
            (fee_payer.pubkey(), 10_000_000),
            (recipient, 1_000_000),
        ]);
        let er_history_store = Arc::new(ErHistoryStore::default());
        let client = create_client_with_history(bank_forks, vec![], er_history_store.clone());

        // Build a tx with a fabricated (expired) blockhash
        let fake_blockhash = solana_hash::Hash::new_unique();
        let tx = Transaction::new_signed_with_payer(
            &[solana_system_interface::instruction::transfer(
                &fee_payer.pubkey(),
                &recipient,
                1,
            )],
            Some(&fee_payer.pubkey()),
            &[&fee_payer],
            fake_blockhash,
        );
        let tx = VersionedTransaction::from(tx);
        let signature = tx.signatures[0];

        // Simulate a pre-execution failure (e.g. BlockhashNotFound) via
        // record_failed_transaction, which is the path taken when
        // prepare_entry_batch returns an error.
        client.record_failed_transaction(
            &bank,
            tx,
            solana_transaction::TransactionError::BlockhashNotFound,
        );

        // The signature must be findable in ER history
        er_history_store.finalize_slot(&bank);
        let status = er_history_store
            .get_signature_status(&signature)
            .expect("failed tx should be recorded in ER history");
        assert_eq!(
            status.err,
            Some(solana_transaction::TransactionError::BlockhashNotFound),
            "status should carry BlockhashNotFound error"
        );

        // Also verify via getTransaction
        let recorded = er_history_store
            .get_transaction(
                &signature,
                solana_rpc_client_types::config::CommitmentConfig::confirmed(),
            )
            .expect("failed tx should be retrievable via getTransaction");
        let meta = recorded
            .tx_with_meta
            .get_status_meta()
            .expect("failed tx should have status meta");
        assert_eq!(
            meta.status,
            Err(solana_transaction::TransactionError::BlockhashNotFound),
            "failed tx meta status should be BlockhashNotFound"
        );
    }

    #[test]
    fn test_commit_error_transaction_recorded_in_er_history() {
        // Regression: a transaction that fails during commit (not just pre-execution)
        // must also be recorded in ER history with its error.
        let fee_payer = Keypair::new();
        let recipient = Pubkey::new_unique();
        let (bank, bank_forks) = create_test_bank_forks_with_accounts(&[
            (fee_payer.pubkey(), 10_000_000),
            (recipient, 1_000_000),
        ]);
        let er_history_store = Arc::new(ErHistoryStore::default());
        let client = create_client_with_history(bank_forks, vec![], er_history_store.clone());

        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[solana_system_interface::instruction::transfer(
                &fee_payer.pubkey(),
                &recipient,
                1,
            )],
            Some(&fee_payer.pubkey()),
            &[&fee_payer],
            blockhash,
        );
        let tx = VersionedTransaction::from(tx);
        let signature = tx.signatures[0];

        // Simulate a commit-result error (e.g. insufficient funds) directly
        client.record_transaction_history_for_batch(
            &bank,
            std::slice::from_ref(&tx),
            &[Err(
                solana_transaction::TransactionError::InsufficientFundsForRent { account_index: 0 },
            )],
            None,
        );

        er_history_store.finalize_slot(&bank);
        let status = er_history_store
            .get_signature_status(&signature)
            .expect("commit-failed tx should be recorded in ER history");
        assert!(
            status.err.is_some(),
            "commit-failed tx status should carry an error"
        );
    }
}
