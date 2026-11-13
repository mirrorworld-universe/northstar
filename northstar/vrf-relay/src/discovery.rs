use {
    crate::{
        accounts::{Request, discriminator},
        journal::Journal,
        rpc::{Account, MAX_ACCOUNT_BATCH, Rpc, number, string},
        settings::Settings,
    },
    anyhow::{Context, Result, bail, ensure},
    serde::{Deserialize, Serialize},
    serde_json::{Value, json},
    solana_pubkey::Pubkey,
    std::{
        collections::BTreeSet,
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        path::Path,
        time::{Duration, Instant},
    },
    tungstenite::{Message, WebSocket, client::client_with_config, protocol::WebSocketConfig},
};
pub const STATE_FILE: &str = "l1-discovery-v1.json";
const STATE_BYTES: u64 = 1024 * 1024;
const MAX_CANDIDATES: usize = 4096;
const PAGE_SIZE: usize = 128;
const MAX_PAGES: usize = 16;
const REPLAY_INTERVAL: Duration = Duration::from_secs(5);
const BOOTSTRAP_HORIZON: u64 = u16::MAX as u64;
type Discovered = (u64, Vec<(Pubkey, Account)>);
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryState {
    pub version: u32,
    pub binding: String,
    pub covered_through: Option<u64>,
    pub pending: BTreeSet<String>,
}
impl DiscoveryState {
    fn validate(&self, binding: &str, capacity: usize) -> Result<()> {
        ensure!(
            self.version == 1 && self.binding == binding && self.pending.len() <= capacity,
            "L1 discovery state binding or capacity differs"
        );
        for address in &self.pending {
            let key: Pubkey = address.parse().context("invalid L1 discovery address")?;
            ensure!(
                key != Pubkey::default() && key.to_string() == *address,
                "invalid L1 discovery address"
            );
        }
        Ok(())
    }
    fn load(directory: &Path) -> Result<Option<Self>> {
        let path = directory.join(STATE_FILE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file()
                        && metadata.nlink() == 1
                        && metadata.permissions().mode() & 0o077 == 0,
                    "L1 discovery state must be a private regular file"
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "L1 discovery input is not regular"
        );
        let mut bytes = Vec::new();
        file.take(STATE_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= STATE_BYTES,
            "L1 discovery state exceeds bounds"
        );
        Ok(Some(
            serde_json::from_slice(&bytes).context("invalid established L1 discovery state")?,
        ))
    }
    fn save(&self, directory: &Path) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        ensure!(
            bytes.len() as u64 <= STATE_BYTES,
            "L1 discovery state exceeds bounds"
        );
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(directory.join(STATE_FILE))?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }
    pub fn inspect(directory: &Path, binding: &str, capacity: usize) -> Result<Self> {
        let state = Self::load(directory)?
            .context("established L1 discovery state missing; refusing reset")?;
        state.validate(binding, capacity)?;
        ensure!(
            state.covered_through.is_some(),
            "L1 discovery coverage is not established"
        );
        Ok(state)
    }
}
pub struct ProgramSubscription {
    socket: WebSocket<TcpStream>,
    subscription: u64,
}
impl ProgramSubscription {
    pub fn connect(settings: &Settings) -> Result<Self> {
        let endpoint = settings
            .ws_url
            .as_deref()
            .context("L1 subscription endpoint missing")?;
        let uri = url::Url::parse(endpoint)?;
        let host = match uri.host() {
            Some(url::Host::Ipv4(address)) if address.is_loopback() => IpAddr::V4(address),
            Some(url::Host::Ipv6(address)) if address.is_loopback() => IpAddr::V6(address),
            Some(url::Host::Domain("localhost")) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            _ => bail!("L1 subscription must use loopback"),
        };
        ensure!(uri.scheme() == "ws", "L1 subscription requires local WS");
        let address = SocketAddr::new(host, uri.port().unwrap_or(80));
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
            .context("L1 subscription connection failed")?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let configuration = WebSocketConfig::default()
            .read_buffer_size(4096)
            .write_buffer_size(4096)
            .max_write_buffer_size(65536)
            .max_message_size(Some(1024 * 1024))
            .max_frame_size(Some(1024 * 1024));
        let (mut socket, _) = client_with_config(endpoint, stream, Some(configuration))
            .map_err(|_| anyhow::anyhow!("L1 subscription handshake failed"))?;
        socket.send(Message::Text(
            json!(
                { "jsonrpc" : "2.0", "id" : 1, "method" : "programSubscribe",
                "params" : [settings.coordinator.to_string(), { "encoding" :
                "base64", "commitment" : settings.commitment, "filters" : [{
                "memcmp" : { "offset" : 0, "bytes" :
                bs58::encode(discriminator("account", "RequestAccount"))
                .into_string() } }, { "memcmp" : { "offset" : 9, "bytes" :
                bs58::encode([0u8]).into_string() } }] }] }
            )
            .to_string()
            .into(),
        ))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let text = loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("L1 subscription acknowledgement timed out")?;
            socket.get_mut().set_read_timeout(Some(remaining))?;
            match socket
                .read()
                .context("L1 program subscription acknowledgement missing")?
            {
                Message::Text(text) => break text,
                Message::Ping(_) => socket.flush()?,
                Message::Pong(_) => {}
                _ => bail!("invalid L1 subscription acknowledgement"),
            }
        };
        let value: Value = serde_json::from_str(text.as_str())?;
        ensure!(
            value["jsonrpc"] == "2.0" && value["id"] == 1 && value.get("error").is_none(),
            "L1 program subscription rejected"
        );
        let subscription = number(&value["result"])?;
        socket
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(20)))?;
        Ok(Self {
            socket,
            subscription,
        })
    }
    fn drain(&mut self) -> Result<Vec<(Pubkey, u64)>> {
        let mut updates = Vec::new();
        for _ in 0..MAX_CANDIDATES {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let value: Value = serde_json::from_str(text.as_str())
                        .context("invalid L1 program notification")?;
                    ensure!(
                        value["jsonrpc"] == "2.0"
                            && value["method"] == "programNotification"
                            && value["params"]["subscription"].as_u64() == Some(self.subscription),
                        "unexpected L1 subscription response"
                    );
                    let result = &value["params"]["result"];
                    let slot = number(&result["context"]["slot"])?;
                    let key: Pubkey = string(&result["value"]["pubkey"])?.parse()?;
                    ensure!(key != Pubkey::default(), "invalid L1 subscription address");
                    updates.push((key, slot));
                }
                Ok(Message::Ping(_)) => self.socket.flush()?,
                Ok(Message::Pong(_)) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(updates);
                }
                _ => bail!("L1 subscription disconnected; history replay required"),
            }
        }
        bail!("L1 subscription exceeds bounded intake; resync required")
    }
}
impl Drop for ProgramSubscription {
    fn drop(&mut self) {
        let _ = self.socket.close(None);
    }
}
pub struct L1Discovery {
    subscription: ProgramSubscription,
    state: DiscoveryState,
    last_replay: Option<Instant>,
}
impl L1Discovery {
    pub fn open(settings: &Settings, rpc: &Rpc, journal: &mut Journal) -> Result<Self> {
        let binding = settings.binding()?;
        let state = match DiscoveryState::load(&settings.state_dir)? {
            Some(state) => {
                state.validate(&binding, settings.max_records)?;
                ensure!(
                    state.covered_through.is_some(),
                    "L1 discovery coverage is not established"
                );
                state
            }
            None => {
                ensure!(
                    !journal.snapshot().l1_discovery_established,
                    "established L1 discovery state missing; refusing reset"
                );
                DiscoveryState {
                    version: 1,
                    binding,
                    covered_through: None,
                    pending: BTreeSet::new(),
                }
            }
        };
        let subscription = ProgramSubscription::connect(settings)?;
        let mut result = Self {
            subscription,
            state,
            last_replay: None,
        };
        result.discover(settings, rpc)?;
        if !journal.snapshot().l1_discovery_established {
            journal.change(|snapshot| {
                snapshot.l1_discovery_established = true;
                Ok(())
            })?;
        }
        Ok(result)
    }
    fn history_candidates(
        &self,
        settings: &Settings,
        rpc: &Rpc,
        current_slot: u64,
    ) -> Result<(u64, u64, BTreeSet<Pubkey>)> {
        let finalized = number(&rpc.call("getSlot", json!([{ "commitment" : "finalized" }]))?)?;
        ensure!(
            self.state
                .covered_through
                .is_none_or(|slot| finalized >= slot),
            "L1 finalized replay cursor moved backwards"
        );
        let current_slot = current_slot.max(finalized);
        let horizon = current_slot.saturating_sub(BOOTSTRAP_HORIZON);
        let cutoff = self
            .state
            .covered_through
            .map_or(horizon, |slot| slot.min(horizon));
        let first = number(&rpc.call("getFirstAvailableBlock", json!([]))?)?;
        ensure!(first <= cutoff, "L1 history replay gap; resync required");
        let mut before: Option<String> = None;
        let mut candidates = BTreeSet::new();
        let mut observed = BTreeSet::new();
        let mut previous_slot = u64::MAX;
        let mut maximum_slot = current_slot;
        for _ in 0..MAX_PAGES {
            let mut options = json!(
                { "commitment" : settings.commitment, "limit" : PAGE_SIZE }
            );
            if let Some(before) = &before {
                options["before"] = json!(before);
            }
            let response = rpc.call(
                "getSignaturesForAddress",
                json!([settings.coordinator.to_string(), options]),
            )?;
            let rows = response.as_array().context("invalid L1 history response")?;
            ensure!(rows.len() <= PAGE_SIZE, "L1 history page exceeds bounds");
            for row in rows {
                let signature = string(&row["signature"])?;
                ensure!(
                    bs58::decode(&signature).into_vec()?.len() == 64
                        && observed.insert(signature.clone()),
                    "invalid or repeated L1 history signature"
                );
                let slot = number(&row["slot"])?;
                ensure!(slot <= previous_slot, "L1 history ordering differs");
                previous_slot = slot;
                if slot < cutoff {
                    return Ok((finalized, maximum_slot, candidates));
                }
                maximum_slot = maximum_slot.max(slot);
                ensure!(
                    matches!(
                        row["confirmationStatus"].as_str(),
                        Some("confirmed" | "finalized")
                    ),
                    "L1 history commitment unavailable"
                );
                let error = row.get("err").context("L1 history error field missing")?;
                if !error.is_null() {
                    continue;
                }
                let transaction = rpc.call(
                    "getTransaction",
                    json!(
                        [signature, { "encoding" : "json", "commitment" : settings
                        .commitment, "maxSupportedTransactionVersion" : 0 }]
                    ),
                )?;
                ensure!(
                    number(&transaction["slot"])? == slot
                        && transaction["meta"].get("err") == Some(&Value::Null)
                        && transaction["transaction"]["signatures"][0] == row["signature"],
                    "L1 replay transaction unavailable or inconsistent"
                );
                let version = transaction
                    .get("version")
                    .context("L1 replay transaction version missing")?;
                ensure!(
                    version == "legacy" || version.as_u64() == Some(0),
                    "unsupported L1 replay transaction version"
                );
                let keys = transaction["transaction"]["message"]["accountKeys"]
                    .as_array()
                    .context("L1 replay account keys missing")?;
                ensure!(keys.len() <= 256, "L1 replay transaction account limit");
                let mut transaction_keys = BTreeSet::new();
                for key in keys {
                    transaction_keys.insert(string(key)?.parse::<Pubkey>()?);
                }
                let loaded = transaction["meta"]
                    .get("loadedAddresses")
                    .filter(|value| !value.is_null());
                ensure!(
                    version == "legacy" || loaded.is_some(),
                    "L1 replay lookup addresses missing"
                );
                if let Some(loaded) = loaded {
                    for class in ["writable", "readonly"] {
                        let keys = loaded[class]
                            .as_array()
                            .context("L1 replay loaded addresses missing")?;
                        ensure!(keys.len() <= 256, "L1 replay loaded address limit");
                        for key in keys {
                            transaction_keys.insert(string(key)?.parse::<Pubkey>()?);
                        }
                    }
                }
                ensure!(
                    transaction_keys.len() <= 256
                        && transaction_keys.contains(&settings.coordinator),
                    "L1 replay coordinator or account bound differs"
                );
                candidates.extend(transaction_keys);
                ensure!(
                    candidates.len() <= MAX_CANDIDATES,
                    "L1 history candidate capacity exceeded; resync required"
                );
            }
            if rows.len() < PAGE_SIZE {
                return Ok((finalized, maximum_slot, candidates));
            }
            before = Some(string(
                &rows.last().context("empty L1 history page")?["signature"],
            )?);
        }
        bail!("L1 history replay bound exceeded; resync required")
    }
    pub fn discover(&mut self, settings: &Settings, rpc: &Rpc) -> Result<Discovered> {
        let mut minimum_slot = rpc
            .slot()?
            .max(self.state.covered_through.unwrap_or_default());
        let mut candidates = self
            .state
            .pending
            .iter()
            .map(|key| key.parse::<Pubkey>())
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;
        for (key, slot) in self.subscription.drain()? {
            minimum_slot = minimum_slot.max(slot);
            candidates.insert(key);
        }
        let replay = self
            .last_replay
            .is_none_or(|time| time.elapsed() >= REPLAY_INTERVAL);
        let mut next = self.state.clone();
        if replay {
            let (finalized, slot, history) =
                self.history_candidates(settings, rpc, minimum_slot)?;
            minimum_slot = minimum_slot.max(slot);
            candidates.extend(history);
            next.covered_through = Some(finalized);
        }
        ensure!(
            candidates.len() <= MAX_CANDIDATES,
            "L1 discovery candidate capacity exceeded; resync required"
        );
        let keys: Vec<_> = candidates.into_iter().collect();
        let mut pending = BTreeSet::new();
        let mut accounts = Vec::new();
        let mut context = None;
        for batch in keys.chunks(MAX_ACCOUNT_BATCH) {
            let (slot, values) = rpc.accounts_at(batch, minimum_slot)?;
            context = Some(context.map_or(slot, |previous: u64| previous.min(slot)));
            for (key, account) in batch.iter().zip(values) {
                let Some(account) = account else { continue };
                if account.owner != settings.coordinator
                    || !account
                        .data
                        .starts_with(&discriminator("account", "RequestAccount"))
                {
                    continue;
                }
                let request =
                    Request::decode(&settings.coordinator, key, &account.owner, &account.data)?;
                let interested = (request.mode == 1 && settings.allow_store)
                    || (request.mode == 0
                        && settings.allowed_consumers.contains(&request.requester));
                if request.status == 0
                    && request.vrf_key == settings.vrf_public_key.to_bytes()
                    && request.deadline_slot >= slot
                    && interested
                {
                    ensure!(
                        request.request_slot <= slot,
                        "L1 request context precedes creation"
                    );
                    pending.insert(key.to_string());
                    accounts.push((*key, account));
                }
            }
        }
        next.pending = pending;
        next.validate(&settings.binding()?, settings.max_records)?;
        if next != self.state {
            next.save(&settings.state_dir)?;
            self.state = next;
        }
        if replay {
            self.last_replay = Some(Instant::now());
        }
        Ok((context.unwrap_or(minimum_slot), accounts))
    }
}
