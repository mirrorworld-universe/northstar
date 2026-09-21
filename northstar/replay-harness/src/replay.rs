use {
    solana_account::ReadableAccount, solana_pubkey::Pubkey,
    solana_runtime::bank::er_replay::ErReplaySnapshot, solana_sdk_ids::sysvar,
    solana_transaction::versioned::VersionedTransaction,
};

pub fn reexecute(
    snapshot: &ErReplaySnapshot,
    transaction: VersionedTransaction,
) -> crate::txn::BankTxnProcessingResult {
    use {crate::txn::BankTxnProcessingResult, solana_transaction::TransactionError};
    let account_data = |key: Pubkey| {
        snapshot
            .accounts
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, account)| account.data())
    };
    let clock = account_data(sysvar::clock::id())
        .and_then(|data| bincode::deserialize::<solana_clock::Clock>(data).ok());
    let schedule = account_data(sysvar::epoch_schedule::id())
        .and_then(|data| bincode::deserialize::<solana_epoch_schedule::EpochSchedule>(data).ok());
    if snapshot.version != 1
        || clock.is_none()
        || schedule.is_none_or(|schedule| schedule.slots_per_epoch == 0)
    {
        return BankTxnProcessingResult::FailedVerification(TransactionError::InvalidAccountIndex);
    }
    if !snapshot.blockhash_queue.is_hash_valid_for_age(
        transaction.message.recent_blockhash(),
        snapshot.max_processing_age,
    ) {
        return BankTxnProcessingResult::FailedVerification(TransactionError::BlockhashNotFound);
    }
    // BlockhashQueue serialization omits its derived durable nonce cache.
    let mut blockhash_queue = snapshot.blockhash_queue.clone();
    blockhash_queue.refresh_durable_nonce();
    crate::txn::execute_er_txn_with_trace(
        &snapshot.accounts,
        snapshot.feature_set(),
        blockhash_queue,
        snapshot.fee_rate_governor.clone(),
        snapshot.total_epoch_stake,
        transaction,
        &solana_fee_structure::FeeStructure {
            lamports_per_signature: snapshot.lamports_per_signature,
            lamports_per_write_lock: 0,
            compute_fee_bins: vec![],
        },
        snapshot.max_processing_age,
    )
}
