// Sonic: Native Bank execution adapter for the Northstar replay harness.
use {
    crate::{
        bank::{Bank, BankFieldsToDeserialize, BankRc},
        epoch_stakes::VersionedEpochStakes,
        stake_history::StakeHistory,
        stakes::{DeserializableDelegationStakes, SerdeStakesToStakeFormat, Stakes},
    },
    agave_feature_set::FeatureSet,
    solana_account::AccountSharedData,
    solana_accounts_db::{
        accounts::Accounts, accounts_db::AccountsDb, ancestors::Ancestors,
        blockhash_queue::BlockhashQueue,
    },
    solana_clock::{BankId, Clock, Epoch, MAX_PROCESSING_AGE},
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
    solana_transaction::{
        TransactionVerificationMode, sanitized::SanitizedTransaction,
        versioned::VersionedTransaction,
    },
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
        runtime_transaction: Box<RuntimeTransaction<SanitizedTransaction>>,
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
    const TICKS_PER_SLOT: u64 = 64;

    // Slot and parent slot come from the clock sysvar.
    let clock: Clock = sysvar_from_accounts(accounts, &sysvar::clock::id());
    let slot = clock.slot;
    let parent_slot = slot.saturating_sub(1);

    let epoch_schedule: EpochSchedule =
        sysvar_from_accounts(accounts, &sysvar::epoch_schedule::id());
    let epoch = epoch_schedule.get_epoch(slot);

    // Populate the accounts DB with the input accounts at the parent slot.
    let bank_accounts = Accounts::new(Arc::new(AccountsDb::default_for_tests()));
    let ancestors = Ancestors::from(vec![parent_slot]);
    bank_accounts.store_accounts_seq((parent_slot, accounts), BankId::default(), None, &ancestors);
    bank_accounts.accounts_db.add_root(parent_slot);
    let bank_rc = BankRc::new(bank_accounts);

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

    // The bank must be wrapped in `BankForks` so the program cache has a fork graph;
    // `_bank_forks` is kept alive for the duration of execution.
    let mut bank = Bank::new_for_txn_tests(bank_rc, bank_fields, feature_set, epoch_stakes);
    // Sonic: Apply the captured ER policy before verification and execution.
    if let Some((fee_structure, max_processing_age)) = er_config {
        bank.configure_er(fee_structure, max_processing_age);
    }
    if enable_trace {
        bank.enable_transaction_tracing();
    }
    let (bank, _bank_forks) = bank.wrap_with_bank_forks_for_tests();

    let runtime_transaction = match bank.verify_transaction(transaction, verification_mode) {
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
