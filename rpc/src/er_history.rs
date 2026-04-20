use {
    solana_clock::{Slot, UnixTimestamp},
    solana_commitment_config::CommitmentConfig,
    solana_runtime::bank::Bank,
    solana_signature::Signature,
    solana_transaction_status::{
        ConfirmedTransactionWithStatusMeta, TransactionConfirmationStatus, TransactionStatus,
        TransactionWithStatusMeta, VersionedTransactionWithStatusMeta,
    },
    std::{
        collections::{BTreeMap, HashMap},
        sync::RwLock,
    },
};

#[derive(Default)]
struct ErSlotHistory {
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
    pub fn record_transaction(
        &self,
        slot: Slot,
        transaction: VersionedTransactionWithStatusMeta,
        block_time: Option<UnixTimestamp>,
    ) -> Option<u32> {
        let signature = transaction.transaction.signatures.first().copied()?;
        let mut inner = self.inner.write().unwrap();

        if let Some(existing) = inner.transactions.get(&signature) {
            return Some(existing.index);
        }

        let slot_history = inner.slots.entry(slot).or_default();
        let index = slot_history.signatures.len() as u32;
        let block_time = slot_history.block_time.or(block_time);
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
        let block_time = Some(bank.clock().unix_timestamp);
        let mut inner = self.inner.write().unwrap();
        let slot_history = inner.slots.entry(slot).or_default();
        slot_history.finalized = true;
        slot_history.block_time = block_time;

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
}
