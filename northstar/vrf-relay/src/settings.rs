use {
    crate::rpc::Rpc,
    anyhow::{Context, Result, ensure},
    serde::{Deserialize, Serialize},
    sha2::{Digest, Sha256},
    solana_pubkey::Pubkey,
    std::{
        fs,
        io::Read,
        os::unix::fs::OpenOptionsExt,
        path::{Path, PathBuf},
    },
};
pub const DEVNET_GENESIS: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
pub const MAINNET_GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
fn read_configuration(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_CONFIG_BYTES,
        "configuration too large"
    );
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_configuration_read_stops_at_the_limit() {
        struct Input {
            remaining: usize,
            consumed: usize,
        }
        impl Read for Input {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let count = output.len().min(self.remaining);
                output[..count].fill(0);
                self.remaining -= count;
                self.consumed += count;
                Ok(count)
            }
        }
        let mut input = Input {
            remaining: MAX_CONFIG_BYTES as usize + 4096,
            consumed: 0,
        };
        assert!(read_configuration(&mut input).is_err());
        assert!(input.consumed <= MAX_CONFIG_BYTES as usize + 1);
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    Local,
    DevnetL1,
    DevnetEr,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub network: Network,
    pub rpc_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_url: Option<String>,
    pub expected_genesis: String,
    pub l1_rpc_url: Option<String>,
    #[serde(with = "crate::serde_keys::optional")]
    pub er_session: Option<Pubkey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub er_session_context: Option<String>,
    #[serde(with = "crate::serde_keys::single")]
    pub coordinator: Pubkey,
    #[serde(with = "crate::serde_keys::single")]
    pub verifier: Pubkey,
    pub coordinator_programdata_sha256: String,
    pub verifier_programdata_sha256: String,
    #[serde(with = "crate::serde_keys::single")]
    pub vrf_public_key: Pubkey,
    #[serde(with = "crate::serde_keys::single")]
    pub fulfiller: Pubkey,
    pub signer_file: PathBuf,
    pub vrf_key_file: PathBuf,
    pub state_dir: PathBuf,
    pub commitment: String,
    #[serde(with = "crate::serde_keys::list")]
    pub allowed_consumers: Vec<Pubkey>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        with = "crate::serde_keys::list"
    )]
    pub request_addresses: Vec<Pubkey>,
    pub allow_store: bool,
    pub poll_ms: u64,
    pub max_attempts: u32,
    pub max_records: usize,
}
fn loopback_url(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}
impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_checked(path, Self::validate)
    }
    pub fn load_for_inspection(path: &Path) -> Result<Self> {
        Self::load_checked(path, |settings| settings.validate_inner(false))
    }
    fn load_checked(path: &Path, check: fn(&Self) -> Result<()>) -> Result<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "configuration must be a regular file"
        );
        let bytes = read_configuration(file)?;
        let mut result: Self = serde_json::from_slice(&bytes).context("invalid configuration")?;
        let parent = path.parent().unwrap_or(Path::new("."));
        for item in [
            &mut result.signer_file,
            &mut result.vrf_key_file,
            &mut result.state_dir,
        ] {
            if item.is_relative() {
                *item = parent.join(&*item);
            }
        }
        check(&result)?;
        Ok(result)
    }
    pub fn validate(&self) -> Result<()> {
        self.validate_inner(true)
    }
    fn validate_inner(&self, require_context: bool) -> Result<()> {
        ensure!(
            matches!(self.commitment.as_str(), "confirmed" | "finalized"),
            "commitment must be confirmed or finalized"
        );
        ensure!(
            (100..=60_000).contains(&self.poll_ms),
            "poll interval out of bounds"
        );
        ensure!(
            (1..=8).contains(&self.max_attempts) && (1..=1024).contains(&self.max_records),
            "journal policy out of bounds"
        );
        ensure!(
            self.request_addresses.len() <= crate::rpc::MAX_ACCOUNT_BATCH
                && self
                    .request_addresses
                    .iter()
                    .all(|key| *key != Pubkey::default())
                && self
                    .request_addresses
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == self.request_addresses.len(),
            "request watchlist must contain at most 64 distinct nonzero addresses"
        );
        ensure!(
            !matches!(self.network, Network::DevnetL1)
                || (self.er_session.is_none()
                    && self.er_session_context.is_none()
                    && self.l1_rpc_url.is_none()),
            "devnet L1 must not include ER configuration"
        );
        ensure!(
            (!require_context && self.er_session_context.is_none())
                || self.er_session.is_some() == self.er_session_context.is_some(),
            "ER requires an explicit continuity session context"
        );
        if let Some(context) = &self.er_session_context {
            let l1 = reqwest::Url::parse(
                self.l1_rpc_url
                    .as_deref()
                    .context("ER continuity requires an L1 RPC")?,
            )
            .map_err(|_| anyhow::anyhow!("invalid L1 RPC URL"))?;
            ensure!(
                matches!(l1.scheme(), "http" | "https")
                    && l1.username().is_empty()
                    && l1.password().is_none(),
                "L1 RPC requires HTTP(S) without userinfo"
            );
            match self.network {
                Network::Local => {
                    ensure!(loopback_url(&l1), "local mode requires a loopback L1 RPC")
                }
                Network::DevnetL1 | Network::DevnetEr => {
                    ensure!(
                        l1.scheme() == "https" || loopback_url(&l1),
                        "hosted L1 RPC requires HTTPS or a loopback tunnel"
                    )
                }
            }
            crate::portal::parse_context(context)?;
        }
        ensure!(
            !self.expected_genesis.is_empty() && self.expected_genesis != MAINNET_GENESIS,
            "mainnet or absent genesis is not permitted"
        );
        for digest in [
            &self.coordinator_programdata_sha256,
            &self.verifier_programdata_sha256,
        ] {
            ensure!(
                hex::decode(digest)?.len() == 32,
                "program-data hash must be SHA-256 hex"
            );
        }
        let url =
            reqwest::Url::parse(&self.rpc_url).map_err(|_| anyhow::anyhow!("invalid RPC URL"))?;
        let loopback = loopback_url(&url);
        if let Some(endpoint) = &self.ws_url {
            let ws = reqwest::Url::parse(endpoint)
                .map_err(|_| anyhow::anyhow!("invalid L1 subscription URL"))?;
            ensure!(
                matches!(self.network, Network::Local | Network::DevnetL1)
                    && self.er_session.is_none()
                    && self.er_session_context.is_none()
                    && self.l1_rpc_url.is_none()
                    && self.request_addresses.is_empty()
                    && loopback
                    && url.scheme() == "http"
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && matches!(url.path(), "" | "/")
                    && loopback_url(&ws)
                    && ws.scheme() == "ws"
                    && ws.username().is_empty()
                    && ws.password().is_none()
                    && ws.query().is_none()
                    && ws.fragment().is_none()
                    && matches!(ws.path(), "" | "/"),
                "L1 subscriptions require own-node loopback HTTP/WS without a request watchlist"
            );
        }
        ensure!(
            url.username().is_empty() && url.password().is_none(),
            "RPC userinfo is not supported"
        );
        match self.network {
            Network::Local => ensure!(loopback, "local mode requires a loopback RPC"),
            Network::DevnetL1 => {
                ensure!(
                    self.expected_genesis == DEVNET_GENESIS,
                    "L1 mode requires the complete Solana devnet genesis"
                );
                ensure!(
                    url.scheme() == "https" || (url.scheme() == "http" && loopback),
                    "hosted RPC requires HTTPS or an HTTP loopback tunnel"
                );
            }
            Network::DevnetEr => {
                ensure!(
                    self.er_session.is_some() && self.l1_rpc_url.is_some(),
                    "devnet ER requires L1 RPC and explicit session identity"
                );
                ensure!(
                    url.scheme() == "https" || loopback,
                    "hosted RPC requires HTTPS or a loopback tunnel"
                );
            }
        }
        Ok(())
    }
    pub fn check_network(&self, rpc: &Rpc) -> Result<()> {
        self.checked_network_session(rpc).map(|_| ())
    }
    pub(crate) fn checked_network_session(
        &self,
        rpc: &Rpc,
    ) -> Result<Option<(crate::portal::Session, u64)>> {
        self.validate()?;
        ensure!(
            rpc.genesis()? == self.expected_genesis,
            "RPC genesis does not match configured deployment"
        );
        if let Some(expected) = self.er_session {
            let l1 = Rpc::new(
                self.l1_rpc_url.as_deref().context("missing L1 RPC")?,
                "finalized",
            )?;
            let genesis = l1.genesis()?;
            ensure!(genesis != MAINNET_GENESIS, "mainnet L1 is not permitted");
            if let Network::DevnetEr = self.network {
                ensure!(genesis == DEVNET_GENESIS, "L1 is not Solana devnet");
            }
            let pinned = self
                .er_session_context
                .as_deref()
                .context("missing continuity session context")?;
            return Ok(Some(crate::portal::check_rpc_binding(
                &l1,
                rpc,
                expected,
                crate::portal::parse_context(pinned)?,
                &genesis,
            )?));
        }
        Ok(None)
    }
    pub fn binding(&self) -> Result<String> {
        let mut value = serde_json::json!(
            { "format" : 1, "network" : self.network, "genesis" : self.expected_genesis,
            "session" : self.er_session, "coordinator" : self.coordinator, "verifier" :
            self.verifier, "coordinator_hash" : self.coordinator_programdata_sha256
            .to_ascii_lowercase(), "verifier_hash" : self.verifier_programdata_sha256
            .to_ascii_lowercase(), "vrf_key" : self.vrf_public_key, "fulfiller" : self
            .fulfiller, "commitment" : self.commitment, "max_attempts" : self
            .max_attempts, "max_records" : self.max_records, "consumers" : self
            .allowed_consumers, "allow_store" : self.allow_store, }
        );
        if let Some(context) = &self.er_session_context {
            value["session_context"] = context.to_ascii_lowercase().into();
        }
        if !self.request_addresses.is_empty() {
            let mut addresses = self.request_addresses.clone();
            addresses.sort_unstable();
            value["request_addresses"] = serde_json::json!(
                addresses
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            );
        }
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&value)?)))
    }
}
