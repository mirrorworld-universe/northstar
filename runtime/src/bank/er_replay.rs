// Sonic: Bounded inputs for offline re-execution of the supported ER SBF relation.
use {
    super::Bank,
    agave_feature_set::FeatureSet,
    serde::{Deserialize, Serialize},
    solana_account::{AccountSharedData, ReadableAccount},
    solana_accounts_db::blockhash_queue::BlockhashQueue,
    solana_clock::Slot,
    solana_fee_calculator::FeeRateGovernor,
    solana_loader_v3_interface::state::UpgradeableLoaderState,
    solana_pubkey::Pubkey,
    solana_sdk_ids::{bpf_loader_upgradeable, sysvar},
    solana_transaction::versioned::VersionedTransaction,
};

pub const MAX_ER_REPLAY_SNAPSHOT_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErReplaySnapshot {
    pub version: u8,
    pub accounts: Vec<(Pubkey, AccountSharedData)>,
    pub active_features: Vec<(Pubkey, Slot)>,
    pub inactive_features: Vec<Pubkey>,
    pub blockhash_queue: BlockhashQueue,
    pub fee_rate_governor: FeeRateGovernor,
    pub lamports_per_signature: u64,
    pub max_processing_age: usize,
    pub total_epoch_stake: u64,
}

impl Bank {
    pub fn er_replay_snapshot(&self, transaction: &VersionedTransaction) -> Option<Vec<u8>> {
        let solana_message::VersionedMessage::Legacy(message) = &transaction.message else {
            return None;
        };
        if message.account_keys.len() != 3
            || message.instructions.len() != 1
            || message.header.num_required_signatures != 1
            || message.header.num_readonly_signed_accounts != 0
            || message.header.num_readonly_unsigned_accounts != 1
            || message.instructions[0].program_id_index != 2
            || message.instructions[0].accounts != [1]
            || self.fee_structure.lamports_per_write_lock != 0
            || !self.fee_structure.compute_fee_bins.is_empty()
        {
            return None;
        }
        let program = self.get_account(&message.account_keys[2])?;
        if program.owner() != &bpf_loader_upgradeable::id() || !program.executable() {
            return None;
        }
        let UpgradeableLoaderState::Program {
            programdata_address,
        } = bincode::deserialize(program.data()).ok()?
        else {
            return None;
        };
        let mut keys = message.account_keys.clone();
        keys.extend([
            programdata_address,
            sysvar::clock::id(),
            sysvar::epoch_schedule::id(),
            sysvar::rent::id(),
            sysvar::slot_hashes::id(),
        ]);
        keys.sort_unstable();
        keys.dedup();
        let mut size = 0usize;
        let accounts = keys
            .into_iter()
            .map(|key| {
                let account = self.get_account(&key)?;
                size = size.checked_add(account.data().len())?;
                (size <= MAX_ER_REPLAY_SNAPSHOT_BYTES).then_some((key, account))
            })
            .collect::<Option<Vec<_>>>()?;
        let mut active_features = self
            .feature_set
            .active()
            .iter()
            .map(|(key, slot)| (*key, *slot))
            .collect::<Vec<_>>();
        active_features.sort_unstable();
        let mut inactive_features = self
            .feature_set
            .inactive()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        inactive_features.sort_unstable();
        let snapshot = ErReplaySnapshot {
            version: 1,
            accounts,
            active_features,
            inactive_features,
            blockhash_queue: self.blockhash_queue.read().unwrap().clone(),
            fee_rate_governor: self.fee_rate_governor.clone(),
            lamports_per_signature: self.fee_structure.lamports_per_signature,
            max_processing_age: self.max_processing_age(),
            total_epoch_stake: self.get_current_epoch_total_stake(),
        };
        let bytes = bincode::serialize(&snapshot).ok()?;
        (bytes.len() <= MAX_ER_REPLAY_SNAPSHOT_BYTES).then_some(bytes)
    }
}

impl ErReplaySnapshot {
    pub fn feature_set(&self) -> FeatureSet {
        FeatureSet::new(
            self.active_features.iter().copied().collect(),
            self.inactive_features.iter().copied().collect(),
        )
    }

    #[cfg(feature = "conformance")]
    pub fn reexecute(
        &self,
        transaction: VersionedTransaction,
    ) -> crate::conformance::txn::BankTxnProcessingResult {
        use {
            crate::conformance::txn::BankTxnProcessingResult, solana_transaction::TransactionError,
        };
        let account_data = |key: Pubkey| {
            self.accounts
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, account)| account.data())
        };
        let clock = account_data(sysvar::clock::id())
            .and_then(|data| bincode::deserialize::<solana_clock::Clock>(data).ok());
        let schedule = account_data(sysvar::epoch_schedule::id()).and_then(|data| {
            bincode::deserialize::<solana_epoch_schedule::EpochSchedule>(data).ok()
        });
        if self.version != 1
            || clock.is_none()
            || schedule.is_none_or(|schedule| schedule.slots_per_epoch == 0)
        {
            return BankTxnProcessingResult::FailedVerification(
                TransactionError::InvalidAccountIndex,
            );
        }
        if !self.blockhash_queue.is_hash_valid_for_age(
            transaction.message.recent_blockhash(),
            self.max_processing_age,
        ) {
            return BankTxnProcessingResult::FailedVerification(
                TransactionError::BlockhashNotFound,
            );
        }
        // BlockhashQueue serialization omits its derived durable nonce cache.
        let mut blockhash_queue = self.blockhash_queue.clone();
        blockhash_queue.refresh_durable_nonce();
        crate::conformance::txn::execute_er_txn_with_trace(
            &self.accounts,
            self.feature_set(),
            blockhash_queue,
            self.fee_rate_governor.clone(),
            self.total_epoch_stake,
            transaction,
            &solana_fee_structure::FeeStructure {
                lamports_per_signature: self.lamports_per_signature,
                lamports_per_write_lock: 0,
                compute_fee_bins: vec![],
            },
            self.max_processing_age,
        )
    }
}
