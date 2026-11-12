//! Relay-only proof acquisition. This configuration cannot name a local VRF credential.
use {
    crate::{
        accounts::Request,
        credentials, crypto,
        journal::Record,
        portal,
        request_signer::{SignResponse, loopback_url},
        rpc::Account,
        service::{ProvedRequest, RequestProver},
        settings::{Network, Settings},
    },
    anyhow::{Context, Result, ensure},
    base64::{Engine, engine::general_purpose::STANDARD},
    northstar_vrf_protocol::RequestWitness,
    reqwest::{
        blocking::Client,
        header::{AUTHORIZATION, HeaderMap, HeaderValue},
    },
    serde::{Deserialize, Deserializer, Serialize, Serializer},
    solana_pubkey::Pubkey,
    std::{
        io::Read,
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, Instant},
    },
    zeroize::Zeroizing,
};
pub trait CommitmentSource: Send + Sync {
    fn candidates(&self) -> Result<Vec<Pubkey>> {
        Ok(Vec::new())
    }
    fn witness(&self, address: &Pubkey) -> Result<Option<RequestWitness>>;
}
#[derive(Clone)]
pub struct RelayWorkerSettings(Settings);
impl<'de> Deserialize<'de> for RelayWorkerSettings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let mut fields = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        if fields.contains_key("vrf_key_file") {
            return Err(serde::de::Error::custom(
                "relay configuration cannot contain a VRF credential",
            ));
        }
        fields.insert(
            "vrf_key_file".into(),
            serde_json::Value::String(String::new()),
        );
        let settings = serde_json::from_value(serde_json::Value::Object(fields))
            .map_err(serde::de::Error::custom)?;
        Ok(Self(settings))
    }
}
impl Serialize for RelayWorkerSettings {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut fields = serde_json::to_value(&self.0).map_err(serde::ser::Error::custom)?;
        fields
            .as_object_mut()
            .ok_or_else(|| serde::ser::Error::custom("invalid worker schema"))?
            .remove("vrf_key_file");
        fields.serialize(serializer)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteSignerConfig {
    pub url: String,
    pub bearer_token_file: PathBuf,
    pub policy_sha256: String,
}
impl RemoteSignerConfig {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.url)
            .map_err(|_| anyhow::anyhow!("invalid remote signer URL"))?;
        ensure!(
            (url.scheme() == "https" || (url.scheme() == "http" && loopback_url(&url)))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "remote signer requires HTTPS or literal loopback HTTP without URL credentials"
        );
        ensure!(
            self.policy_sha256.len() == 64 && hex::decode(& self.policy_sha256)
            .is_ok_and(| bytes | bytes != [0; 32] && hex::encode(bytes) == self
            .policy_sha256), "remote signer policy digest is invalid"
        );
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelaySettings {
    pub version: u8,
    pub worker: RelayWorkerSettings,
    pub signer: RemoteSignerConfig,
}
impl RelaySettings {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = credentials::read_regular(path, 64 * 1024, false)?;
        let mut settings: Self =
            serde_json::from_slice(&bytes).context("invalid relay configuration")?;
        let parent = path.parent().unwrap_or(Path::new("."));
        for value in [
            &mut settings.worker.0.signer_file,
            &mut settings.worker.0.state_dir,
            &mut settings.signer.bearer_token_file,
        ] {
            if value.is_relative() {
                *value = parent.join(&*value);
            }
        }
        settings.validate()?;
        Ok(settings)
    }
    pub fn worker_settings(&self) -> &Settings {
        &self.worker.0
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported relay configuration version");
        ensure!(
            !matches!(self.worker.0.network, Network::DevnetL1) && self.worker.0.ws_url.is_none(),
            "native ER relay cannot adopt the standalone L1 discovery profile"
        );
        ensure!(
            self.worker.0.er_session.is_some() && self.worker.0.er_session_context.is_some(),
            "native ER relay requires a pinned continuity session"
        );
        ensure!(
            self.worker.0.request_addresses.is_empty(),
            "native ER relay discovers only from its accepted lineage"
        );
        ensure!(
            self.worker.0.fulfiller != self.worker.0.vrf_public_key,
            "relay transaction wallet must be distinct from the VRF key"
        );
        self.worker.0.validate()?;
        self.signer.validate()
    }
    pub(crate) fn into_parts(self) -> (Settings, RemoteSignerConfig) {
        (self.worker.0, self.signer)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigningReceipt {
    pub witness: RequestWitness,
    pub response: SignResponse,
}
pub(crate) struct RemoteProver {
    client: Client,
    config: RemoteSignerConfig,
    public_key: [u8; 32],
    session_context: [u8; 32],
    coordinator: Pubkey,
    source: Arc<dyn CommitmentSource>,
    last_request: Option<Instant>,
}
impl RemoteProver {
    pub fn open(
        settings: &Settings,
        config: RemoteSignerConfig,
        source: Arc<dyn CommitmentSource>,
    ) -> Result<Self> {
        config.validate()?;
        let token = credentials::bearer(&config.bearer_token_file)?;
        let mut authorization = Zeroizing::new(b"Bearer ".to_vec());
        authorization.extend_from_slice(&token);
        let mut value = HeaderValue::from_bytes(&authorization)?;
        value.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value);
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(25))
            .build()?;
        Ok(Self {
            client,
            config,
            public_key: settings.vrf_public_key.to_bytes(),
            session_context: portal::parse_context(
                settings
                    .er_session_context
                    .as_deref()
                    .context("relay session context missing")?,
            )?,
            coordinator: settings.coordinator,
            source,
            last_request: None,
        })
    }
    fn validate(
        &self,
        receipt: &SigningReceipt,
        address: &Pubkey,
        data: &[u8],
        request: &Request,
    ) -> Result<crypto::VrfResult> {
        let witness = &receipt.witness;
        let response = &receipt.response;
        let root = witness
            .opening
            .commitment()
            .context("invalid remote state opening")?;
        ensure!(
            witness.account.address == address.to_bytes()
                && witness.account.owner == self.coordinator.to_bytes()
                && witness.data == data
                && witness.verify(&self.session_context, &root),
            "remote proof witness differs from the observed request"
        );
        ensure!(
            response.version == 1
                && response.policy_sha256 == self.config.policy_sha256
                && response.public_key == Pubkey::new_from_array(self.public_key).to_string()
                && response.alpha_hex == hex::encode(request.alpha)
                && response.checkpoint_er_slot == witness.er_slot
                && response.state_root_hex == hex::encode(root)
                && response.finalized_l1_slot > 0
                && response
                    .checkpoint
                    .parse::<Pubkey>()
                    .is_ok_and(|key| key != Pubkey::default())
                && response.request_id.len() == 64
                && hex::decode(&response.request_id)
                    .is_ok_and(|bytes| hex::encode(bytes) == response.request_id)
                && witness.er_slot >= request.request_slot
                && witness.er_slot < request.deadline_slot,
            "remote signing receipt binding differs"
        );
        let result = response.result()?;
        crypto::verify_output(&self.public_key, &request.alpha, &result)?;
        Ok(result)
    }
}
impl RequestProver for RemoteProver {
    fn public_key(&self) -> [u8; 32] {
        self.public_key
    }
    fn binding_suffix(&self) -> Option<String> {
        Some(format!("{}:{}", self.config.policy_sha256, self.config.url))
    }
    fn candidates(&self) -> Option<Result<Vec<Pubkey>>> {
        Some(self.source.candidates())
    }
    fn validate_receipt(&self, record: &Record) -> Result<()> {
        let receipt = record
            .signing_receipt
            .as_ref()
            .context("remote journal is missing its commitment receipt")?;
        let data = STANDARD.decode(&record.request_base64)?;
        let request =
            Request::decode(&self.coordinator, &record.address, &self.coordinator, &data)?;
        let result = self.validate(receipt, &record.address, &data, &request)?;
        ensure!(
            hex::encode(result.proof) == record.proof_hex
                && hex::encode(result.output) == record.output_hex,
            "remote journal proof differs from its signing receipt"
        );
        Ok(())
    }
    fn prove(
        &mut self,
        address: &Pubkey,
        account: &Account,
        request: &Request,
    ) -> Result<Option<ProvedRequest>> {
        if self
            .last_request
            .is_some_and(|instant| instant.elapsed() < Duration::from_secs(2))
        {
            return Ok(None);
        }
        let Some(witness) = self.source.witness(address)? else {
            return Ok(None);
        };
        ensure!(
            witness.account.address == address.to_bytes()
                && witness.account.owner == account.owner.to_bytes()
                && witness.data == account.data
                && !account.executable,
            "checkpoint witness differs from current request"
        );
        self.last_request = Some(Instant::now());
        let response = match self
            .client
            .post(format!("{}/v1/sign", self.config.url.trim_end_matches('/')))
            .json(&witness)
            .send()
        {
            Ok(response) => response,
            Err(_) => return Ok(None),
        };
        if matches!(response.status().as_u16(), 409 | 429 | 503) {
            return Ok(None);
        }
        ensure!(
            response.status().is_success(),
            "remote signing request was not qualified"
        );
        let mut bytes = Vec::new();
        response.take(32 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 32 * 1024,
            "remote signing response exceeds bounds"
        );
        let response: SignResponse =
            serde_json::from_slice(&bytes).context("invalid remote signing response")?;
        let receipt = SigningReceipt { witness, response };
        let result = self.validate(&receipt, address, &account.data, request)?;
        Ok(Some(ProvedRequest {
            result,
            receipt: Some(receipt),
        }))
    }
}
