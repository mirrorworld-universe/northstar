// Sonic: ER Geyser events use ER history, never the L1 transaction-status pipeline.
use {
    solana_geyser_plugin_manager::geyser_plugin_service::{
        GeyserPluginService, GeyserPluginServiceError,
    },
    solana_rpc::er_history::ErHistoryObserver,
    solana_runtime::bank::{Bank, KeyedRewardsAndNumPartitions},
    solana_transaction_status::VersionedTransactionWithStatusMeta,
    std::{path::PathBuf, sync::Arc},
};

pub(super) struct ErGeyserObserver {
    slot_sender: Option<
        crossbeam_channel::Sender<
            solana_rpc::optimistically_confirmed_bank_tracker::SlotNotification,
        >,
    >,
    service: Option<GeyserPluginService>,
}

impl ErGeyserObserver {
    pub(super) fn load(
        paths: &[PathBuf],
    ) -> Result<Option<Arc<dyn ErHistoryObserver>>, GeyserPluginServiceError> {
        if paths.is_empty() {
            return Ok(None);
        }
        // ER publishes its own slot statuses synchronously. No L1 observer feed.
        let (sender, receiver) = crossbeam_channel::unbounded();
        let service = GeyserPluginService::new(receiver, false, paths)?;
        Ok(Some(Arc::new(Self {
            slot_sender: Some(sender),
            service: Some(service),
        })))
    }
}

impl ErHistoryObserver for ErGeyserObserver {
    fn transaction_recorded(
        &self,
        bank: &Bank,
        record: &VersionedTransactionWithStatusMeta,
        index: u32,
    ) {
        let service = self.service.as_ref().unwrap();
        if let Some(notifier) = service.get_transaction_notifier() {
            let transaction = &record.transaction;
            let Some(signature) = transaction.signatures.first() else {
                return;
            };
            notifier.notify_transaction(
                bank.slot(),
                bank.bank_id(),
                index as usize,
                signature,
                &transaction.message.hash(),
                false,
                &record.meta,
                transaction,
            );
        }
    }

    fn slot_finalized(&self, bank: &Bank, transaction_count: u64) {
        let service = self.service.as_ref().unwrap();
        if let Some(notifier) = service.get_block_metadata_notifier() {
            notifier.notify_block_metadata(
                bank.parent_slot(),
                &bank.parent_hash().to_string(),
                bank.slot(),
                bank.bank_id(),
                &bank.last_blockhash().to_string(),
                &KeyedRewardsAndNumPartitions {
                    keyed_rewards: vec![],
                    num_partitions: None,
                },
                Some(bank.clock().unix_timestamp),
                Some(bank.block_height()),
                transaction_count,
                0,
                false,
            );
        }
        if let Some(notifier) = service.get_slot_status_notifier() {
            let notifier = notifier.read().unwrap();
            let parent = Some(bank.parent_slot());
            notifier.notify_slot_processed(bank.slot(), parent, bank.bank_id());
            notifier.notify_slot_confirmed(bank.slot(), parent, bank.bank_id());
            notifier.notify_slot_rooted(bank.slot(), parent, bank.bank_id());
        }
    }
}

#[cfg(test)]
mod tests;

impl Drop for ErGeyserObserver {
    fn drop(&mut self) {
        // Wake the service's blocking slot observer before joining it.
        self.slot_sender.take();
        if let Some(service) = self.service.take()
            && service.join().is_err()
        {
            log::error!("ER Geyser service shutdown failed");
        }
    }
}
