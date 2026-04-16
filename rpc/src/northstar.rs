/// Sonic: NorthStar ephemeral rollup RPC methods
use {crate::rpc::JsonRpcRequestProcessor, jsonrpc_core::Result, jsonrpc_derive::rpc, log::debug};

#[rpc]
pub trait NorthStar {
    type Metadata;

    #[rpc(meta, name = "getDelegatedAccounts")]
    fn get_delegated_accounts(&self, meta: Self::Metadata) -> Result<Vec<String>>;

    #[rpc(meta, name = "getSessionPda")]
    fn get_session_pda(&self, meta: Self::Metadata) -> Result<Option<String>>;
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
}
