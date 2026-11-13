//! Strict signing of independently committed ER requests; no arbitrary-alpha endpoint.
use {
    crate::crypto::VrfResult,
    anyhow::{Context, Result, ensure},
    northstar_vrf_protocol::{Hash32, hash},
    serde::{Deserialize, Serialize},
};
pub fn request_identity(
    genesis: &Hash32,
    context: &Hash32,
    coordinator: &Hash32,
    request: &Hash32,
) -> String {
    hex::encode(hash(&[
        b"northstar-vrf-signing-admission-v1",
        genesis,
        context,
        coordinator,
        request,
    ]))
}
pub(crate) fn loopback_url(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignResponse {
    pub version: u8,
    pub policy_sha256: String,
    pub request_id: String,
    pub public_key: String,
    pub alpha_hex: String,
    pub proof_hex: String,
    pub output_hex: String,
    pub checkpoint: String,
    pub checkpoint_er_slot: u64,
    pub state_root_hex: String,
    pub finalized_l1_slot: u64,
}
impl SignResponse {
    pub fn result(&self) -> Result<VrfResult> {
        let proof = hex::decode(&self.proof_hex)?
            .try_into()
            .ok()
            .context("invalid remote proof length")?;
        let output = hex::decode(&self.output_hex)?
            .try_into()
            .ok()
            .context("invalid remote output length")?;
        ensure!(
            hex::encode(proof) == self.proof_hex && hex::encode(output) == self.output_hex,
            "noncanonical remote proof encoding"
        );
        Ok(VrfResult { proof, output })
    }
}
