mod broker;
mod config;
mod event;
mod extract;
mod server;

use {
    agave_geyser_plugin_interface::geyser_plugin_interface::{
        GeyserPlugin, GeyserPluginError, ReplicaBlockInfoVersions, ReplicaTransactionInfoVersions,
        Result, SlotStatus,
    },
    broker::Broker,
    config::Config,
    server::Server,
    solana_clock::BankId,
    std::{fmt, fs, sync::Arc},
};

#[derive(Default)]
pub struct EventPlugin {
    broker: Option<Arc<Broker>>,
    server: Option<Server>,
}

impl fmt::Debug for EventPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventPlugin")
            .field(
                "listen_addr",
                &self.server.as_ref().map(|server| server.addr),
            )
            .finish()
    }
}

impl GeyserPlugin for EventPlugin {
    fn name(&self) -> &'static str {
        "northstar-geyser-events"
    }

    fn setup_logger(&self, logger: &'static dyn log::Log, level: log::LevelFilter) -> Result<()> {
        log::set_max_level(level);
        let _ = log::set_logger(logger);
        Ok(())
    }

    fn on_load(&mut self, config_file: &str, _is_reload: bool) -> Result<()> {
        let config: Config = serde_json::from_slice(&fs::read(config_file)?).map_err(|error| {
            GeyserPluginError::ConfigFileReadError {
                msg: error.to_string(),
            }
        })?;
        config
            .validate()
            .map_err(|error| GeyserPluginError::ConfigFileReadError {
                msg: error.to_string(),
            })?;
        self.on_unload();
        let broker = Broker::new(config);
        let server = Server::start(broker.clone())?;
        self.broker = Some(broker);
        self.server = Some(server);
        Ok(())
    }

    fn on_unload(&mut self) {
        self.server.take();
        self.broker.take();
    }

    fn account_data_notifications_enabled(&self) -> bool {
        false
    }
    fn account_data_snapshot_notifications_enabled(&self) -> bool {
        false
    }
    fn transaction_notifications_enabled(&self) -> bool {
        true
    }

    fn notify_transaction_for_bank(
        &self,
        transaction: ReplicaTransactionInfoVersions<'_>,
        slot: u64,
        bank_id: BankId,
    ) -> Result<()> {
        if let Some(broker) = &self.broker {
            let ReplicaTransactionInfoVersions::V0_0_4(info) = transaction;
            let watched = broker.watched();
            let events = extract::extract(info, slot, &watched);
            broker.transaction_with_watch(slot, bank_id, info.index, events, &watched);
        }
        Ok(())
    }

    fn notify_block_metadata_for_bank(
        &self,
        block: ReplicaBlockInfoVersions<'_>,
        bank_id: BankId,
    ) -> Result<()> {
        if let Some(broker) = &self.broker {
            let ReplicaBlockInfoVersions::V0_0_5(block) = block;
            broker.metadata(
                block.slot,
                bank_id,
                block.parent_slot,
                block.executed_transaction_count,
                block.block_time,
            );
        }
        Ok(())
    }

    fn update_bank_status(
        &self,
        slot: u64,
        parent: Option<u64>,
        status: &SlotStatus,
        bank_id: BankId,
    ) -> Result<()> {
        if let Some(broker) = &self.broker {
            broker.status(slot, bank_id, parent, status);
        }
        Ok(())
    }

    fn update_slot_status(
        &self,
        slot: u64,
        parent: Option<u64>,
        status: &SlotStatus,
    ) -> Result<()> {
        if matches!(status, SlotStatus::Dead(_))
            && let Some(broker) = &self.broker
        {
            broker.status(slot, 0, parent, status);
        }
        Ok(())
    }
}

// Geyser's Rust trait-object ABI requires the same compiler and interface build as the validator.
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn _create_plugin() -> *mut dyn GeyserPlugin {
    Box::into_raw(Box::new(EventPlugin::default()))
}
