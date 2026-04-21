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
        let finalized = inner
            .slots
            .get(&transaction.slot)
            .map(|slot| slot.finalized)
            .unwrap_or(false);

        if commitment.is_finalized() && !finalized {
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
        if commitment.is_finalized() && !slot_history.finalized {
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
            .filter(|(_, slot_history)| !commitment.is_finalized() || slot_history.finalized)
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
            .filter(|(_, slot_history)| !commitment.is_finalized() || slot_history.finalized)
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
        if commitment.is_finalized() && !slot_history.finalized {
            return None;
        }
        slot_history.block_time
    }

    pub fn get_first_available_block(&self) -> Option<Slot> {
        self.inner.read().unwrap().slots.keys().next().copied()
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
            .filter(|(_, slot_history)| !commitment.is_finalized() || slot_history.finalized)
            .flat_map(|(_, slot_history)| slot_history.signatures.iter().rev())
            .filter_map(|signature| {
                let transaction = inner.transactions.get(signature)?;
                let versioned_transaction = match &transaction.tx_with_meta {
                    TransactionWithStatusMeta::Complete(transaction) => transaction,
                    TransactionWithStatusMeta::MissingMetadata(_) => return None,
                };
                if !versioned_transaction
                    .transaction
                    .message
                    .static_account_keys()
                    .contains(address)
                {
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
        assert!(
            store
                .get_block(bank0.slot(), CommitmentConfig::confirmed())
                .is_some()
        );
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
}
