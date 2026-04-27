use {
    solana_clock::{Slot, UnixTimestamp},
    solana_commitment_config::CommitmentConfig,
    solana_pubkey::Pubkey,
    solana_runtime::bank::Bank,
    solana_signature::Signature,
    solana_transaction_status::{
        ConfirmedBlock, ConfirmedTransactionStatusWithSignature,
        ConfirmedTransactionWithStatusMeta, TransactionConfirmationStatus, TransactionStatus,
        TransactionWithStatusMeta, VersionedTransactionWithStatusMeta,
    },
    std::{
        collections::{BTreeMap, HashMap},
        sync::RwLock,
    },
};

fn slot_visible_at_commitment(slot_history: &ErSlotHistory, commitment: CommitmentConfig) -> bool {
    // Sonic: ER has a single local fork, so recorded slots are visible at
    // `confirmed`; only `finalized` waits for the slot advancer mark.
    if commitment.is_finalized() {
        slot_history.finalized
    } else {
        true
    }
}

#[derive(Default, Clone)]
struct ErSlotHistory {
    blockhash: Option<String>,
    previous_blockhash: Option<String>,
    parent_slot: Option<Slot>,
    block_height: Option<u64>,
    block_time: Option<UnixTimestamp>,
    finalized: bool,
    signatures: Vec<Signature>,
}

/// Slim performance sample for ER, mapped 1:1 onto `RpcPerfSample`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErPerfSample {
    pub slot: Slot,
    pub num_transactions: u64,
    pub num_non_vote_transactions: u64,
    pub num_slots: u64,
    pub sample_period_secs: u16,
}

#[derive(Default)]
struct ErHistoryInner {
    slots: BTreeMap<Slot, ErSlotHistory>,
    transactions: HashMap<Signature, ConfirmedTransactionWithStatusMeta>,
}

#[derive(Default)]
pub struct ErHistoryStore {
    inner: RwLock<ErHistoryInner>,
}

impl ErHistoryStore {
    fn populate_slot_from_bank(slot_history: &mut ErSlotHistory, bank: &Bank) {
        slot_history.blockhash = Some(bank.last_blockhash().to_string());
        slot_history.previous_blockhash = Some(bank.parent_hash().to_string());
        slot_history.parent_slot = Some(bank.parent_slot());
        slot_history.block_height = Some(bank.block_height());
        slot_history.block_time = slot_history
            .block_time
            .or(Some(bank.clock().unix_timestamp));
    }

    pub fn record_transaction(
        &self,
        bank: &Bank,
        transaction: VersionedTransactionWithStatusMeta,
    ) -> Option<u32> {
        let signature = transaction.transaction.signatures.first().copied()?;
        let mut inner = self.inner.write().unwrap();

        if let Some(existing) = inner.transactions.get(&signature) {
            return Some(existing.index);
        }

        let slot = bank.slot();
        let slot_history = inner.slots.entry(slot).or_default();
        Self::populate_slot_from_bank(slot_history, bank);
        let index = slot_history.signatures.len() as u32;
        let block_time = slot_history.block_time;
        slot_history.signatures.push(signature);

        inner.transactions.insert(
            signature,
            ConfirmedTransactionWithStatusMeta {
                slot,
                tx_with_meta: TransactionWithStatusMeta::Complete(transaction),
                block_time,
                index,
            },
        );

        Some(index)
    }

    pub fn finalize_slot(&self, bank: &Bank) {
        let slot = bank.slot();
        let mut inner = self.inner.write().unwrap();
        let slot_history = inner.slots.entry(slot).or_default();
        Self::populate_slot_from_bank(slot_history, bank);
        slot_history.finalized = true;

        let block_time = slot_history.block_time;
        let signatures = slot_history.signatures.clone();
        for signature in signatures {
            if let Some(transaction) = inner.transactions.get_mut(&signature) {
                transaction.block_time = block_time;
            }
        }
    }

    pub fn get_transaction(
        &self,
        signature: &Signature,
        commitment: CommitmentConfig,
    ) -> Option<ConfirmedTransactionWithStatusMeta> {
        let inner = self.inner.read().unwrap();
        let transaction = inner.transactions.get(signature)?.clone();
        let slot_history = inner.slots.get(&transaction.slot)?;

        if !slot_visible_at_commitment(slot_history, commitment) {
            return None;
        }

        Some(transaction)
    }

    pub fn get_signature_status(&self, signature: &Signature) -> Option<TransactionStatus> {
        let inner = self.inner.read().unwrap();
        let transaction = inner.transactions.get(signature)?;
        let meta = transaction.tx_with_meta.get_status_meta()?;
        let finalized = inner
            .slots
            .get(&transaction.slot)
            .map(|slot| slot.finalized)
            .unwrap_or(false);
        let (confirmations, confirmation_status) = if finalized {
            (None, Some(TransactionConfirmationStatus::Finalized))
        } else {
            (Some(0), Some(TransactionConfirmationStatus::Confirmed))
        };
        let status = meta.status.clone();
        let err = status.clone().err();

        Some(TransactionStatus {
            slot: transaction.slot,
            confirmations,
            status,
            err,
            confirmation_status,
        })
    }

    pub fn get_block(&self, slot: Slot, commitment: CommitmentConfig) -> Option<ConfirmedBlock> {
        let inner = self.inner.read().unwrap();
        let slot_history = inner.slots.get(&slot)?;
        if !slot_visible_at_commitment(slot_history, commitment) {
            return None;
        }

        Some(ConfirmedBlock {
            previous_blockhash: slot_history.previous_blockhash.clone().unwrap_or_default(),
            blockhash: slot_history.blockhash.clone().unwrap_or_default(),
            parent_slot: slot_history.parent_slot.unwrap_or_default(),
            transactions: slot_history
                .signatures
                .iter()
                .filter_map(|signature| inner.transactions.get(signature))
                .map(|transaction| transaction.tx_with_meta.clone())
                .collect(),
            rewards: vec![],
            num_partitions: None,
            block_time: slot_history.block_time,
            block_height: slot_history.block_height,
        })
    }

    pub fn get_blocks(
        &self,
        start_slot: Slot,
        end_slot: Slot,
        commitment: CommitmentConfig,
    ) -> Vec<Slot> {
        let inner = self.inner.read().unwrap();
        inner
            .slots
            .range(start_slot..=end_slot)
            .filter(|(_, slot_history)| slot_visible_at_commitment(slot_history, commitment))
            .map(|(slot, _)| *slot)
            .collect()
    }

    pub fn get_blocks_with_limit(
        &self,
        start_slot: Slot,
        limit: usize,
        commitment: CommitmentConfig,
    ) -> Vec<Slot> {
        let inner = self.inner.read().unwrap();
        inner
            .slots
            .range(start_slot..)
            .filter(|(_, slot_history)| slot_visible_at_commitment(slot_history, commitment))
            .map(|(slot, _)| *slot)
            .take(limit)
            .collect()
    }

    pub fn get_block_time(
        &self,
        slot: Slot,
        commitment: CommitmentConfig,
    ) -> Option<UnixTimestamp> {
        let inner = self.inner.read().unwrap();
        let slot_history = inner.slots.get(&slot)?;
        if !slot_visible_at_commitment(slot_history, commitment) {
            return None;
        }
        slot_history.block_time
    }

    pub fn get_first_available_block(&self) -> Option<Slot> {
        self.inner.read().unwrap().slots.keys().next().copied()
    }

    /// Synthesize `getRecentPerformanceSamples` data from finalized ER slots.
    /// Buckets adjacent slots into ~`sample_period_secs`-second windows using
    /// `slot_duration_ms`. Returns at most `limit` samples, newest first (the
    /// same order Solana RPC returns).
    pub fn recent_performance_samples(
        &self,
        limit: usize,
        slot_duration_ms: u64,
    ) -> Vec<ErPerfSample> {
        if limit == 0 {
            return vec![];
        }
        let sample_period_secs: u16 = 60;
        let slot_duration_ms = slot_duration_ms.max(1);
        let slots_per_sample = ((sample_period_secs as u64 * 1000) / slot_duration_ms).max(1);

        let inner = self.inner.read().unwrap();
        // Walk finalized slots oldest -> newest, bucketed; then reverse for
        // newest-first output.
        let mut samples: Vec<ErPerfSample> = Vec::new();
        let mut bucket_start: Option<Slot> = None;
        let mut bucket_slot_count: u64 = 0;
        let mut bucket_tx_count: u64 = 0;
        let mut bucket_last_slot: Slot = 0;
        for (slot, slot_history) in inner.slots.iter() {
            if !slot_history.finalized {
                continue;
            }
            let start = *bucket_start.get_or_insert(*slot);
            if slot.saturating_sub(start) >= slots_per_sample {
                samples.push(ErPerfSample {
                    slot: bucket_last_slot,
                    num_transactions: bucket_tx_count,
                    num_non_vote_transactions: bucket_tx_count,
                    num_slots: bucket_slot_count,
                    sample_period_secs,
                });
                bucket_start = Some(*slot);
                bucket_slot_count = 0;
                bucket_tx_count = 0;
            }
            bucket_slot_count += 1;
            bucket_tx_count += slot_history.signatures.len() as u64;
            bucket_last_slot = *slot;
        }
        if bucket_slot_count > 0 {
            samples.push(ErPerfSample {
                slot: bucket_last_slot,
                num_transactions: bucket_tx_count,
                num_non_vote_transactions: bucket_tx_count,
                num_slots: bucket_slot_count,
                sample_period_secs,
            });
        }
        samples.reverse();
        samples.truncate(limit);
        samples
    }

    pub fn get_signatures_for_address(
        &self,
        address: &Pubkey,
        before: Option<Signature>,
        until: Option<Signature>,
        limit: usize,
        commitment: CommitmentConfig,
    ) -> std::result::Result<Vec<ConfirmedTransactionStatusWithSignature>, Signature> {
        let inner = self.inner.read().unwrap();
        let mut matches = inner
            .slots
            .iter()
            .rev()
            .filter(|(_, slot_history)| slot_visible_at_commitment(slot_history, commitment))
            .flat_map(|(_, slot_history)| slot_history.signatures.iter().rev())
            .filter_map(|signature| {
                let transaction = inner.transactions.get(signature)?;
                let versioned_transaction = match &transaction.tx_with_meta {
                    TransactionWithStatusMeta::Complete(transaction) => transaction,
                    TransactionWithStatusMeta::MissingMetadata(_) => return None,
                };
                let account_keys = versioned_transaction.account_keys();
                if !account_keys.iter().any(|key| key == address) {
                    return None;
                }
                let err = versioned_transaction.meta.status.clone().err();
                Some(ConfirmedTransactionStatusWithSignature {
                    signature: *signature,
                    slot: transaction.slot,
                    err,
                    memo: None,
                    block_time: transaction.block_time,
                    index: transaction.index,
                })
            })
            .collect::<Vec<_>>();

        let start_index = if let Some(before_signature) = before {
            matches
                .iter()
                .position(|entry| entry.signature == before_signature)
                .map(|index| index + 1)
                .ok_or(before_signature)?
        } else {
            0
        };

        let end_index = if let Some(until_signature) = until {
            matches
                .iter()
                .position(|entry| entry.signature == until_signature)
                .ok_or(until_signature)?
        } else {
            matches.len()
        };

        if start_index >= end_index {
            return Ok(vec![]);
        }

        matches.drain(..start_index);
        matches.truncate(end_index.saturating_sub(start_index));
        matches.truncate(limit);
        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_keypair::{Keypair, Signer},
        solana_message::{MessageHeader, VersionedMessage, v0},
        solana_runtime::genesis_utils::create_genesis_config,
        solana_system_interface::instruction as system_instruction,
        solana_transaction::{Transaction, versioned::VersionedTransaction},
        solana_transaction_status::TransactionStatusMeta,
        std::sync::Arc,
    };

    fn create_test_bank() -> Bank {
        let genesis_config = create_genesis_config(1_000_000).genesis_config;
        Bank::new_for_tests(&genesis_config)
    }

    fn create_test_tx(
        bank: &Bank,
        from: &Keypair,
        to: &Pubkey,
    ) -> VersionedTransactionWithStatusMeta {
        let tx = Transaction::new_signed_with_payer(
            &[system_instruction::transfer(&from.pubkey(), to, 1)],
            Some(&from.pubkey()),
            &[from],
            bank.last_blockhash(),
        );

        VersionedTransactionWithStatusMeta {
            transaction: VersionedTransaction::from(tx),
            meta: TransactionStatusMeta {
                status: Ok(()),
                fee: 5000,
                pre_balances: vec![],
                post_balances: vec![],
                inner_instructions: None,
                log_messages: None,
                pre_token_balances: None,
                post_token_balances: None,
                rewards: Some(vec![]),
                loaded_addresses: Default::default(),
                return_data: None,
                compute_units_consumed: Some(0),
                cost_units: None,
            },
        }
    }

    #[test]
    fn test_block_queries_and_first_available_block() {
        let store = ErHistoryStore::default();
        let payer = Keypair::new();
        let recipient = Pubkey::new_unique();

        let bank0 = create_test_bank();
        let tx0 = create_test_tx(&bank0, &payer, &recipient);
        let sig0 = tx0.transaction.signatures[0];
        assert_eq!(store.record_transaction(&bank0, tx0), Some(0));
        let confirmed_block0 = store
            .get_block(bank0.slot(), CommitmentConfig::confirmed())
            .unwrap();
        assert_eq!(confirmed_block0.transactions.len(), 1);
        assert!(
            store
                .get_block(bank0.slot(), CommitmentConfig::finalized())
                .is_none()
        );
        store.finalize_slot(&bank0);

        let block0 = store
            .get_block(bank0.slot(), CommitmentConfig::finalized())
            .unwrap();
        assert_eq!(block0.blockhash, bank0.last_blockhash().to_string());
        assert_eq!(block0.transactions.len(), 1);
        assert_eq!(store.get_first_available_block(), Some(bank0.slot()));
        assert!(
            store
                .get_signature_status(&sig0)
                .unwrap()
                .confirmations
                .is_none()
        );

        let bank1 = Bank::new_from_parent(Arc::new(bank0), &Pubkey::new_unique(), 1);
        store.finalize_slot(&bank1);

        assert_eq!(
            store.get_blocks(0, 10, CommitmentConfig::finalized()),
            vec![0, 1]
        );
        assert_eq!(
            store.get_blocks_with_limit(0, 1, CommitmentConfig::finalized()),
            vec![0]
        );
        assert_eq!(
            store.get_block_time(1, CommitmentConfig::finalized()),
            Some(bank1.clock().unix_timestamp)
        );
    }

    #[test]
    fn test_recent_performance_samples_buckets_finalized_slots() {
        let store = ErHistoryStore::default();
        let payer = Keypair::new();
        let recipient = Pubkey::new_unique();

        // 6 finalized slots; with slot_duration=10s and 60s window we expect
        // 1 sample of 6 slots. Each slot has one tx.
        let bank0 = Arc::new(create_test_bank());
        let mut prev = bank0.clone();
        let tx = create_test_tx(&prev, &payer, &recipient);
        store.record_transaction(&prev, tx);
        store.finalize_slot(&prev);
        for slot in 1u64..6 {
            let bank = Arc::new(Bank::new_from_parent(
                prev.clone(),
                &Pubkey::new_unique(),
                slot,
            ));
            let tx = VersionedTransactionWithStatusMeta {
                transaction: VersionedTransaction {
                    signatures: vec![Signature::from([slot as u8 + 1; 64])],
                    ..create_test_tx(&bank, &payer, &recipient).transaction
                },
                meta: TransactionStatusMeta {
                    status: Ok(()),
                    fee: 5000,
                    pre_balances: vec![],
                    post_balances: vec![],
                    inner_instructions: None,
                    log_messages: None,
                    pre_token_balances: None,
                    post_token_balances: None,
                    rewards: Some(vec![]),
                    loaded_addresses: Default::default(),
                    return_data: None,
                    compute_units_consumed: Some(0),
                    cost_units: None,
                },
            };
            store.record_transaction(&bank, tx);
            store.finalize_slot(&bank);
            prev = bank;
        }

        // 10s/slot -> 60s window -> 6 slots per bucket -> 1 sample.
        let samples = store.recent_performance_samples(10, 10_000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].num_slots, 6);
        assert_eq!(samples[0].num_transactions, 6);
        assert_eq!(samples[0].sample_period_secs, 60);

        // 30s/slot -> 60s window -> 2 slots per bucket -> 3 samples (newest
        // first).
        let samples = store.recent_performance_samples(10, 30_000);
        assert_eq!(samples.len(), 3);
        assert!(samples[0].slot > samples[1].slot);
        assert_eq!(samples.iter().map(|s| s.num_slots).sum::<u64>(), 6);

        // limit
        let samples = store.recent_performance_samples(2, 30_000);
        assert_eq!(samples.len(), 2);
    }

    #[test]
    fn test_get_signatures_for_address_filters() {
        let store = ErHistoryStore::default();
        let payer = Keypair::new();
        let recipient = Pubkey::new_unique();

        let bank0 = create_test_bank();
        let tx0 = create_test_tx(&bank0, &payer, &recipient);
        let sig0 = tx0.transaction.signatures[0];
        store.record_transaction(&bank0, tx0);
        store.finalize_slot(&bank0);

        let bank1 = Bank::new_from_parent(Arc::new(bank0), &Pubkey::new_unique(), 1);
        let tx1 = VersionedTransactionWithStatusMeta {
            transaction: VersionedTransaction {
                signatures: vec![Signature::from([0u8; 64])],
                ..create_test_tx(&bank1, &payer, &recipient).transaction
            },
            meta: TransactionStatusMeta {
                status: Ok(()),
                fee: 5000,
                pre_balances: vec![],
                post_balances: vec![],
                inner_instructions: None,
                log_messages: None,
                pre_token_balances: None,
                post_token_balances: None,
                rewards: Some(vec![]),
                loaded_addresses: Default::default(),
                return_data: None,
                compute_units_consumed: Some(0),
                cost_units: None,
            },
        };
        let sig1 = tx1.transaction.signatures[0];
        store.record_transaction(&bank1, tx1);
        store.finalize_slot(&bank1);

        let all = store
            .get_signatures_for_address(&recipient, None, None, 10, CommitmentConfig::finalized())
            .unwrap();
        assert_eq!(
            all.iter().map(|entry| entry.signature).collect::<Vec<_>>(),
            vec![sig1, sig0]
        );

        let before = store
            .get_signatures_for_address(
                &recipient,
                Some(sig1),
                None,
                10,
                CommitmentConfig::finalized(),
            )
            .unwrap();
        assert_eq!(
            before
                .iter()
                .map(|entry| entry.signature)
                .collect::<Vec<_>>(),
            vec![sig0]
        );
    }

    #[test]
    fn test_get_signatures_for_address_matches_loaded_addresses() {
        let store = ErHistoryStore::default();
        let payer = Keypair::new();
        let loaded_address = Pubkey::new_unique();

        let bank = create_test_bank();
        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(v0::Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 0,
                },
                recent_blockhash: bank.last_blockhash(),
                account_keys: vec![payer.pubkey()],
                instructions: vec![],
                address_table_lookups: vec![],
            }),
            &[&payer],
        )
        .unwrap();
        let signature = tx.signatures[0];

        store.record_transaction(
            &bank,
            VersionedTransactionWithStatusMeta {
                transaction: tx,
                meta: TransactionStatusMeta {
                    status: Ok(()),
                    fee: 5000,
                    pre_balances: vec![],
                    post_balances: vec![],
                    inner_instructions: None,
                    log_messages: None,
                    pre_token_balances: None,
                    post_token_balances: None,
                    rewards: Some(vec![]),
                    loaded_addresses: v0::LoadedAddresses {
                        writable: vec![loaded_address],
                        readonly: vec![],
                    },
                    return_data: None,
                    compute_units_consumed: Some(0),
                    cost_units: None,
                },
            },
        );
        store.finalize_slot(&bank);

        let matches = store
            .get_signatures_for_address(
                &loaded_address,
                None,
                None,
                10,
                CommitmentConfig::confirmed(),
            )
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].signature, signature);
    }
}
