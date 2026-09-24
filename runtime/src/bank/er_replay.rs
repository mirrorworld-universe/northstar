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

// Sonic: A populated ER blockhash queue plus sysvars exceeds 128 KiB even for a small program.
pub const MAX_ER_REPLAY_SNAPSHOT_BYTES: usize = 256 * 1024;

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
            log::debug!("ER replay: unsupported transaction or fee structure");
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
        if bytes.len() > MAX_ER_REPLAY_SNAPSHOT_BYTES {
            log::warn!(
                "ER replay snapshot exceeds bound: bytes={} max_age={}",
                bytes.len(),
                snapshot.max_processing_age
            );
            return None;
        }
        Some(bytes)
    }
}

impl ErReplaySnapshot {
    pub fn feature_set(&self) -> FeatureSet {
        FeatureSet::new(
            self.active_features.iter().copied().collect(),
            self.inactive_features.iter().copied().collect(),
        )
    }
}

// Sonic: A mature ER queue must remain capturable, not only a fresh validator's queue.
#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_account::WritableAccount,
        solana_fee_structure::FeeStructure,
        solana_hash::Hash,
        solana_instruction::{AccountMeta, Instruction},
        solana_message::Message,
        solana_signature::Signature,
    };

    #[test]
    fn mature_er_history_remains_capturable() {
        let (genesis, _) = solana_genesis_config::create_genesis_config(1_000_000);
        let mut bank = Bank::new_for_tests(&genesis);
        bank.feature_set = std::sync::Arc::new(FeatureSet::all_enabled());
        let max_age = 1800;
        bank.configure_er(
            &FeeStructure {
                lamports_per_signature: 0,
                lamports_per_write_lock: 0,
                compute_fee_bins: vec![],
            },
            max_age,
        );
        let payer = Pubkey::new_unique();
        let target = Pubkey::new_unique();
        let program = Pubkey::new_unique();
        let programdata = Pubkey::new_unique();
        bank.store_account(
            &payer,
            &AccountSharedData::new(1_000_000, 0, &solana_sdk_ids::system_program::id()),
        );
        bank.store_account(&target, &AccountSharedData::new(1_000_000, 8, &program));
        let mut executable = AccountSharedData::new(1, 0, &bpf_loader_upgradeable::id());
        executable.set_data_from_slice(
            &bincode::serialize(&UpgradeableLoaderState::Program {
                programdata_address: programdata,
            })
            .unwrap(),
        );
        executable.set_executable(true);
        bank.store_account(&program, &executable);
        bank.store_account(
            &programdata,
            &AccountSharedData::new(
                1,
                12_272 + UpgradeableLoaderState::size_of_programdata_metadata(),
                &bpf_loader_upgradeable::id(),
            ),
        );
        bank.store_account(
            &sysvar::slot_hashes::id(),
            &AccountSharedData::new(1, 8 + 512 * 40, &sysvar::id()),
        );
        for _ in 0..max_age {
            bank.blockhash_queue
                .write()
                .unwrap()
                .register_hash(&Hash::new_unique(), 0);
        }
        let message = Message::new_with_blockhash(
            &[Instruction::new_with_bytes(
                program,
                &[1],
                vec![AccountMeta::new(target, false)],
            )],
            Some(&payer),
            &bank.last_blockhash(),
        );
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: solana_message::VersionedMessage::Legacy(message),
        };
        let bytes = bank
            .er_replay_snapshot(&transaction)
            .expect("mature ER history must fit the bounded snapshot");
        let snapshot: ErReplaySnapshot = bincode::deserialize(&bytes).unwrap();
        assert_eq!(
            snapshot
                .blockhash_queue
                .get_hash_age(transaction.message.recent_blockhash()),
            Some(0)
        );
        assert_eq!(snapshot.max_processing_age, max_age);
    }
}
