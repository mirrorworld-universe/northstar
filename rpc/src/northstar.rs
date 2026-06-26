/// Sonic: NorthStar ephemeral rollup RPC methods
use {
    crate::rpc::JsonRpcRequestProcessor,
    jsonrpc_core::Result,
    jsonrpc_derive::rpc,
    log::debug,
    serde::{Deserialize, Serialize},
    solana_clock::Slot,
    std::sync::atomic::{AtomicU64, Ordering},
};

/// Sonic: Shared NorthStar L1 sync cursor exposed by `northstarSysGetSyncStatus`.
#[derive(Debug)]
pub struct NorthStarSyncStatus {
    latest_synced_slot: AtomicU64,
    latest_l1_slot: AtomicU64,
}

impl NorthStarSyncStatus {
    pub fn new(initial_slot: Slot) -> Self {
        Self::new_with_slots(initial_slot, initial_slot)
    }

    pub fn new_with_slots(latest_synced_slot: Slot, latest_l1_slot: Slot) -> Self {
        Self {
            latest_synced_slot: AtomicU64::new(latest_synced_slot),
            latest_l1_slot: AtomicU64::new(latest_l1_slot),
        }
    }

    pub fn update_latest_l1_slot(&self, slot: Slot) {
        self.latest_l1_slot.fetch_max(slot, Ordering::Relaxed);
    }

    pub fn mark_synced_through(&self, slot: Slot) {
        self.latest_synced_slot.fetch_max(slot, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> RpcNorthStarSyncStatus {
        self.snapshot_with_cluster_behind(false)
    }

    pub fn snapshot_with_cluster_behind(&self, cluster_behind: bool) -> RpcNorthStarSyncStatus {
        let latest_synced_slot = self.latest_synced_slot.load(Ordering::Relaxed);
        let latest_l1_slot = self.latest_l1_slot.load(Ordering::Relaxed);
        RpcNorthStarSyncStatus {
            is_syncing: cluster_behind || latest_synced_slot < latest_l1_slot,
            latest_synced_slot,
            latest_l1_slot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcNorthStarSyncStatus {
    pub is_syncing: bool,
    pub latest_synced_slot: Slot,
    pub latest_l1_slot: Slot,
}

#[rpc]
pub trait NorthStar {
    type Metadata;

    #[rpc(meta, name = "getDelegatedAccounts")]
    fn get_delegated_accounts(&self, meta: Self::Metadata) -> Result<Vec<String>>;

    #[rpc(meta, name = "getSessionPda")]
    fn get_session_pda(&self, meta: Self::Metadata) -> Result<Option<String>>;

    #[rpc(meta, name = "northstarSysGetSyncStatus")]
    fn get_sync_status(&self, meta: Self::Metadata) -> Result<RpcNorthStarSyncStatus>;
}

pub struct NorthStarImpl;
impl NorthStar for NorthStarImpl {
    type Metadata = JsonRpcRequestProcessor;

    fn get_delegated_accounts(&self, meta: Self::Metadata) -> Result<Vec<String>> {
        debug!("get_delegated_accounts rpc request received");
        match &meta.delegated_accounts {
            Some(accounts) => {
                let set = accounts.read().unwrap();
                Ok(set.iter().map(|p| p.to_string()).collect())
            }
            None => Err(jsonrpc_core::Error {
                code: jsonrpc_core::ErrorCode::InvalidRequest,
                message: "getDelegatedAccounts is not available on this node".to_string(),
                data: None,
            }),
        }
    }

    fn get_session_pda(&self, meta: Self::Metadata) -> Result<Option<String>> {
        debug!("get_session_pda rpc request received");
        match &meta.session_pda {
            Some(pda_lock) => {
                let pda = pda_lock.read().unwrap();
                Ok(pda.map(|p| p.to_string()))
            }
            None => Err(jsonrpc_core::Error {
                code: jsonrpc_core::ErrorCode::InvalidRequest,
                message: "getSessionPda is not available on this node".to_string(),
                data: None,
            }),
        }
    }

    fn get_sync_status(&self, meta: Self::Metadata) -> Result<RpcNorthStarSyncStatus> {
        debug!("northstar_sys_get_sync_status rpc request received");
        meta.northstar_sync_status
            .as_ref()
            .map(|status| status.snapshot_with_cluster_behind(meta.northstar_is_behind_cluster()))
            .ok_or_else(|| jsonrpc_core::Error {
                code: jsonrpc_core::ErrorCode::InvalidRequest,
                message: "northstarSysGetSyncStatus is not available on this node".to_string(),
                data: None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_status_reports_syncing_when_cluster_is_behind_without_inventing_l1_slot() {
        let status = NorthStarSyncStatus::new_with_slots(100, 100);

        assert_eq!(
            status.snapshot_with_cluster_behind(true),
            RpcNorthStarSyncStatus {
                is_syncing: true,
                latest_synced_slot: 100,
                latest_l1_slot: 100,
            }
        );
    }

    #[test]
    fn sync_status_uses_local_l1_gap_when_cluster_is_not_behind() {
        let status = NorthStarSyncStatus::new_with_slots(100, 120);

        assert_eq!(
            status.snapshot_with_cluster_behind(false),
            RpcNorthStarSyncStatus {
                is_syncing: true,
                latest_synced_slot: 100,
                latest_l1_slot: 120,
            }
        );
    }
}
