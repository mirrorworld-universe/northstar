//! Development-only native-RPC oracle and pinned VRF interoperability checks.
#![forbid(unsafe_code)]
/// Available only when local VRF key import and proof generation are compiled out.
///
/// ```compile_fail
/// use northstar_vrf_relay::crypto::OracleKey;
/// ```
/// ```compile_fail
/// use northstar_vrf_relay::service::Worker;
/// let local_constructor = Worker::open;
/// ```
#[cfg(not(feature = "local-prover"))]
pub const LOCAL_PROVING_DISABLED: bool = true;
pub mod accounts;
mod credentials;
pub mod crypto;
pub mod discovery;
pub mod instructions;
pub mod journal;
pub mod keys;
pub mod portal;
pub mod protocol;
pub mod remote_prover;
pub mod request_signer;
pub mod rpc;
pub mod serde_keys;
pub use northstar_vrf_protocol as commitment_protocol;
pub mod service;
pub mod settings;
