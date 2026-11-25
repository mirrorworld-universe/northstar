// Sonic: Native Bank execution adapter for the Northstar replay harness.
use {
    crate::{
        bank::{Bank, BankFieldsToDeserialize, BankRc},
        epoch_stakes::VersionedEpochStakes,
        stake_history::StakeHistory,
        stakes::{DeserializableDelegationStakes, SerdeStakesToStakeFormat, Stakes},
    },
    agave_feature_set::FeatureSet,
    agave_transaction_view::{
        resolved_transaction_view::ResolvedTransactionView,
        transaction_view::UnsanitizedTransactionView,
    },
    bytes::Bytes,
    solana_account::AccountSharedData,
    solana_accounts_db::{
        accounts::Accounts, accounts_db::AccountsDb, ancestors::Ancestors,
        blockhash_queue::BlockhashQueue,
    },
    solana_clock::{Clock, Epoch, MAX_PROCESSING_AGE},
    solana_epoch_schedule::EpochSchedule,
    solana_fee_calculator::FeeRateGovernor,
    solana_pubkey::Pubkey,
    solana_runtime_transaction::runtime_transaction::RuntimeTransaction,
    solana_sdk_ids::sysvar,
    solana_stake_interface::state::Stake,
    solana_svm::{
        conformance::setup::sysvar_from_accounts,
        transaction_error_metrics::TransactionErrorMetrics,
        transaction_processing_result::TransactionProcessingResult,
        transaction_processor::{ExecutionRecordingConfig, TransactionProcessingConfig},
    },
    solana_svm_timings::ExecuteTimings,
    solana_transaction::{TransactionVerificationMode, versioned::VersionedTransaction},
    solana_transaction_error::TransactionError,
    solana_vote::vote_account::VoteAccounts,
    std::{collections::HashMap, sync::Arc},
};
/// Result of executing a single transaction through the [`Bank`].
pub enum BankTxnProcessingResult {
    /// The transaction failed verification before processing.
    FailedVerification(TransactionError),
    /// The transaction was processed (executed, fees-only, or no-op). Carries the
    /// processing result and transaction for effect extraction.
    Processed {
        result: TransactionProcessingResult,
        runtime_transaction: Box<RuntimeTransaction<ResolvedTransactionView<Bytes>>>,
    },
}

// Sonic: opt-in real-Bank execution path for deterministic proof traces.
#[allow(clippy::too_many_arguments)]
pub fn execute_txn_with_trace(
    accounts: &[(Pubkey, AccountSharedData)],
    feature_set: FeatureSet,
    blockhash_queue: BlockhashQueue,
    fee_rate_governor: FeeRateGovernor,
    total_epoch_stake: u64,
    transaction: VersionedTransaction,
    verify_signatures: bool,
) -> BankTxnProcessingResult {
    execute_txn_inner(
        accounts,
        feature_set,
        blockhash_queue,
        fee_rate_governor,
        total_epoch_stake,
        transaction,
        true,
        if verify_signatures {
            TransactionVerificationMode::FullVerification
        } else {
            TransactionVerificationMode::HashAndVerifyPrecompiles
        },
        None,
    )
}

// Sonic: Reconstruct ER fee policy instead of silently using L1 defaults.
#[allow(clippy::too_many_arguments)]
pub fn execute_er_txn_with_trace(
    accounts: &[(Pubkey, AccountSharedData)],
    feature_set: FeatureSet,
    blockhash_queue: BlockhashQueue,
    fee_rate_governor: FeeRateGovernor,
    total_epoch_stake: u64,
    transaction: VersionedTransaction,
    fee_structure: &solana_fee_structure::FeeStructure,
    max_processing_age: usize,
) -> BankTxnProcessingResult {
    execute_txn_inner(
        accounts,
        feature_set,
        blockhash_queue,
        fee_rate_governor,
        total_epoch_stake,
        transaction,
        true,
        TransactionVerificationMode::FullVerification,
        Some((fee_structure, max_processing_age)),
    )
}

/// Build a [`Bank`] from the supplied native inputs and execute `transaction`.
///
/// The clock and epoch-schedule sysvars are read out of `accounts` to derive the
/// bank's slot/epoch.
pub fn execute_txn(
    accounts: &[(Pubkey, AccountSharedData)],
    feature_set: FeatureSet,
    blockhash_queue: BlockhashQueue,
    fee_rate_governor: FeeRateGovernor,
    total_epoch_stake: u64,
    transaction: VersionedTransaction,
) -> BankTxnProcessingResult {
    execute_txn_inner(
        accounts,
        feature_set,
        blockhash_queue,
        fee_rate_governor,
        total_epoch_stake,
        transaction,
        false,
        TransactionVerificationMode::HashAndVerifyPrecompiles,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_txn_inner(
    accounts: &[(Pubkey, AccountSharedData)],
    feature_set: FeatureSet,
    blockhash_queue: BlockhashQueue,
    fee_rate_governor: FeeRateGovernor,
    total_epoch_stake: u64,
    transaction: VersionedTransaction,
    enable_trace: bool,
    verification_mode: TransactionVerificationMode,
    er_config: Option<(&solana_fee_structure::FeeStructure, usize)>,
) -> BankTxnProcessingResult {
    let clock: Clock = sysvar_from_accounts(accounts, &sysvar::clock::id());
    let epoch_schedule = sysvar_from_accounts(accounts, &sysvar::epoch_schedule::id());
    let mut bank = disposable_bank(
        accounts,
        feature_set,
        blockhash_queue,
        fee_rate_governor,
        total_epoch_stake,
        clock.slot,
        epoch_schedule,
    );
    if let Some((fee_structure, max_processing_age)) = er_config {
        bank.configure_er(fee_structure, max_processing_age);
    }
    execute_in_bank(bank, transaction, enable_trace, verification_mode)
}

// Sonic: Both replay adapters must allocate private accounts and caches, not fork a live bank.
fn disposable_bank(
    accounts: &[(Pubkey, AccountSharedData)],
    feature_set: FeatureSet,
    blockhash_queue: BlockhashQueue,
    fee_rate_governor: FeeRateGovernor,
    total_epoch_stake: u64,
    slot: u64,
    epoch_schedule: EpochSchedule,
) -> Bank {
    const TICKS_PER_SLOT: u64 = 64;
    let parent_slot = slot.saturating_sub(1);
    let epoch = epoch_schedule.get_epoch(slot);

    // Sonic: Reserve the parent's ID. The working bank writes at a different
    // slot and must not reuse its parent's AccountsCache entry.
    let bank_rc = BankRc::new(Accounts::new(Arc::new(AccountsDb::default_for_tests())));
    let parent_id = bank_rc.next_bank_id();
    let ancestors = Ancestors::from(vec![(parent_slot, parent_id)]);
    bank_rc
        .accounts
        .store_accounts((parent_slot, accounts), parent_id, None, &ancestors);
    bank_rc.accounts.accounts_db.add_root(parent_id);

    // Dummy epoch stakes with the provided total stake at the current and next epoch.
    let mut epoch_stakes: HashMap<Epoch, VersionedEpochStakes> = HashMap::new();
    for key in [epoch, epoch.saturating_add(1)] {
        let mut entry = VersionedEpochStakes::new(
            SerdeStakesToStakeFormat::Stake(Stakes::<Stake>::default()),
            key,
        );
        entry.set_total_stake(total_epoch_stake);
        epoch_stakes.insert(key, entry);
    }

    // `new_for_txn_tests` ignores `stakes`/`versioned_epoch_stakes`, but the
    // struct still has to be constructed.
    let stakes = DeserializableDelegationStakes {
        vote_accounts: VoteAccounts::default(),
        stake_delegations: vec![],
        unused: 0,
        epoch,
        stake_history: StakeHistory::default(),
    };

    let bank_fields = BankFieldsToDeserialize {
        blockhash_queue,
        parent_slot,
        tick_height: TICKS_PER_SLOT.saturating_mul(slot),
        max_tick_height: TICKS_PER_SLOT.saturating_mul(slot.saturating_add(1)),
        ticks_per_slot: TICKS_PER_SLOT,
        slot,
        block_height: slot,
        fee_rate_governor,
        epoch_schedule,
        stakes,
        ..BankFieldsToDeserialize::default()
    };

    Bank::new_for_txn_tests(bank_rc, bank_fields, feature_set, epoch_stakes)
}

impl Bank {
    /// Sonic: Replace the inherited transaction history only on the first unused
    /// disposable child. Later slots must reproduce the queue by replaying ticks.
    pub fn install_initial_replay_queue(
        &mut self,
        epoch: &solana_accounts_db::blockhash_queue::certified_er::RecentHashEpoch,
    ) -> bool {
        if self.freeze_started()
            || self.slot() != epoch.first_working_slot()
            || self.signature_count() != 0
            || !self.parent().is_some_and(|p| {
                p.slot().checked_add(1) == Some(self.slot())
                    && p.is_complete()
                    && p.is_frozen()
                    && p.transaction_count() == self.transaction_count()
                    && p.max_tick_height() == self.tick_height()
            })
        {
            return false;
        }
        let Some(queue) = epoch.queue_for_working_slot(self.slot()) else {
            return false;
        };
        self.configure_er(
            &solana_fee_structure::FeeStructure {
                lamports_per_signature: 0,
                lamports_per_write_lock: 0,
                compute_fee_bins: vec![],
            },
            epoch.max_age(),
        );
        *self.blockhash_queue.write().unwrap() = queue;
        self.update_recent_blockhashes();
        self.transaction_processor
            .reset_and_fill_sysvar_cache_entries(self);
        true
    }

    /// Sonic: Check the entire transaction-validity window on the private bank.
    pub fn matches_certified_replay_queue(
        &self,
        epoch: &solana_accounts_db::blockhash_queue::certified_er::RecentHashEpoch,
        working_slot: u64,
    ) -> bool {
        self.max_processing_age() == epoch.max_age()
            && epoch.matches_queue(working_slot, &self.blockhash_queue.read().unwrap())
    }
}

/// Sonic: Materialize a fresh, gasless scratch bank for an opt-in epoch. The
/// accounts, features, stake and working slot must already be authenticated by
/// the replica; they are not included in a signed-packet batch. `SlotHashes`
/// contains parent *bank* hashes and is not synthesized from recent blockhashes.
pub fn certified_er_replay_bank(
    accounts: &[(Pubkey, AccountSharedData)],
    feature_set: FeatureSet,
    total_epoch_stake: u64,
    working_slot: u64,
    hashes: &solana_accounts_db::blockhash_queue::certified_er::RecentHashEpoch,
) -> Option<Bank> {
    use solana_account::ReadableAccount;
    if accounts.len() > 8192 || accounts.windows(2).any(|a| a[0].0 >= a[1].0) {
        return None;
    }
    let total_bytes = accounts.iter().try_fold(0usize, |n, (_, account)| {
        n.checked_add(account.data().len())
    })?;
    if total_bytes > 32 * 1024 * 1024 {
        return None;
    }
    let lookup = |key: &Pubkey| {
        accounts
            .binary_search_by_key(key, |(address, _)| *address)
            .ok()
    };
    #[allow(deprecated)]
    if lookup(&sysvar::recent_blockhashes::id()).is_some() {
        return None;
    }
    let sysvar_bytes = |key: &Pubkey| {
        let data = &accounts[lookup(key)?].1;
        (data.owner() == &sysvar::id() && data.lamports() != 0 && !data.executable())
            .then(|| data.data())
    };
    // The legacy conformance constructor expects rent to be present. Reject an
    // incomplete snapshot here rather than letting that expectation panic.
    let _: solana_rent::Rent = wincode::deserialize(sysvar_bytes(&sysvar::rent::id())?).ok()?;
    let history = sysvar_bytes(&sysvar::slot_hashes::id())?;
    let count = u64::from_le_bytes(*history.first_chunk::<8>()?);
    if history.len() > solana_slot_hashes::SIZE
        || !(1..=solana_slot_hashes::MAX_ENTRIES as u64).contains(&count)
    {
        return None;
    }
    let history: solana_slot_hashes::SlotHashes = wincode::deserialize(history).ok()?;
    if history.first()?.slot != working_slot.checked_sub(1)?
        || history.windows(2).any(|w| w[0].slot <= w[1].slot)
        || history
            .iter()
            .any(|entry| entry.hash == solana_hash::Hash::default())
    {
        return None;
    }
    let clock: Clock = wincode::deserialize(sysvar_bytes(&sysvar::clock::id())?).ok()?;
    let schedule: EpochSchedule =
        wincode::deserialize(sysvar_bytes(&sysvar::epoch_schedule::id())?).ok()?;
    if clock.slot != working_slot
        || working_slot.checked_add(1)?.checked_mul(64).is_none()
        || !(32..=u64::from(u32::MAX)).contains(&schedule.slots_per_epoch)
        || schedule
            != EpochSchedule::custom(
                schedule.slots_per_epoch,
                schedule.leader_schedule_slot_offset,
                schedule.warmup,
            )
        || clock.epoch != schedule.get_epoch(working_slot)
    {
        return None;
    }
    let mut bank = disposable_bank(
        accounts,
        feature_set,
        hashes.queue_for_working_slot(working_slot)?,
        FeeRateGovernor::new(0, 0),
        total_epoch_stake,
        working_slot,
        schedule,
    );
    bank.configure_er(
        &solana_fee_structure::FeeStructure {
            lamports_per_signature: 0,
            lamports_per_write_lock: 0,
            compute_fee_bins: vec![],
        },
        hashes.max_age(),
    );
    bank.update_recent_blockhashes();
    bank.transaction_processor
        .reset_and_fill_sysvar_cache_entries(&bank);
    Some(bank)
}

fn execute_in_bank(
    mut bank: Bank,
    transaction: VersionedTransaction,
    enable_trace: bool,
    verification_mode: TransactionVerificationMode,
) -> BankTxnProcessingResult {
    if enable_trace {
        bank.enable_transaction_tracing();
    }
    let (bank, _bank_forks) = bank.wrap_with_bank_forks_for_tests();

    let transaction_bytes = match wincode::serialize(&transaction) {
        Ok(bytes) => Bytes::from(bytes),
        Err(_) => {
            return BankTxnProcessingResult::FailedVerification(TransactionError::SanitizeFailure);
        }
    };
    let Ok(transaction_view) = UnsanitizedTransactionView::try_new_unsanitized(transaction_bytes)
    else {
        return BankTxnProcessingResult::FailedVerification(TransactionError::SanitizeFailure);
    };
    let runtime_transaction = match bank.verify_transaction(transaction_view, verification_mode) {
        Ok(tx) => tx,
        Err(err) => return BankTxnProcessingResult::FailedVerification(err),
    };

    let recording_config = ExecutionRecordingConfig {
        enable_cpi_recording: enable_trace,
        enable_log_recording: true,
        enable_return_data_recording: true,
        enable_transaction_balance_recording: false,
    };
    let processing_config = TransactionProcessingConfig {
        recording_config,
        limit_to_load_programs: true,
        ..Default::default()
    };

    let mut timings = ExecuteTimings::default();
    let mut metrics = TransactionErrorMetrics::default();
    let result = {
        let batch = bank.prepare_locked_batch_from_single_tx(&runtime_transaction);
        bank.load_and_execute_transactions(
            &batch,
            MAX_PROCESSING_AGE,
            &mut timings,
            &mut metrics,
            processing_config,
        )
        .processing_results
        .into_iter()
        .next()
        .expect("single transaction execution must return one result")
    };

    BankTxnProcessingResult::Processed {
        result,
        runtime_transaction: Box::new(runtime_transaction),
    }
}
