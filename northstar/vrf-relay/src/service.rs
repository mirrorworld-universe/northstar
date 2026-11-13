use {
    crate::{
        accounts::{self, ProgramConfig, Request},
        crypto::{self, VrfResult},
        discovery::L1Discovery,
        instructions,
        journal::{Attempt, Journal, Record, Status},
        keys,
        remote_prover::{CommitmentSource, RelaySettings, RemoteProver, SigningReceipt},
        rpc::{Account, Confirmation, Rpc, decode_account, number, string},
        settings::Settings,
    },
    anyhow::{Context, Result, ensure},
    base64::{Engine, engine::general_purpose::STANDARD},
    bincode::Options,
    serde_json::{Value, json},
    sha2::{Digest, Sha256},
    solana_hash::Hash,
    solana_instruction::Instruction,
    solana_keypair::Keypair,
    solana_message::Message,
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_transaction::Transaction,
};
pub(crate) struct ProvedRequest {
    pub result: VrfResult,
    pub receipt: Option<SigningReceipt>,
}
pub(crate) trait RequestProver: Send {
    fn public_key(&self) -> [u8; 32];
    fn binding_suffix(&self) -> Option<String>;
    fn candidates(&self) -> Option<Result<Vec<Pubkey>>> {
        None
    }
    fn prove(
        &mut self,
        address: &Pubkey,
        account: &Account,
        request: &Request,
    ) -> Result<Option<ProvedRequest>>;
    fn validate_receipt(&self, record: &Record) -> Result<()>;
}
pub struct Worker {
    pub settings: Settings,
    pub rpc: Rpc,
    signer: Keypair,
    prover: Box<dyn RequestProver>,
    binding: String,
    journal: Journal,
    discovery: Option<L1Discovery>,
}
pub fn check_programs(settings: &Settings, rpc: &Rpc) -> Result<()> {
    checked_programs(settings, rpc).map(|_| ())
}
fn checked_programs(
    settings: &Settings,
    rpc: &Rpc,
) -> Result<Option<(crate::portal::Session, u64)>> {
    let session = settings.checked_network_session(rpc)?;
    ensure!(
        rpc.program_hash(&settings.coordinator)?
            .eq_ignore_ascii_case(&settings.coordinator_programdata_sha256),
        "Coordinator deployment changed"
    );
    ensure!(
        rpc.program_hash(&settings.verifier)?
            .eq_ignore_ascii_case(&settings.verifier_programdata_sha256),
        "verifier deployment changed"
    );
    Ok(session)
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ErSyncStatus {
    is_syncing: bool,
    latest_synced_slot: u64,
    latest_l1_slot: u64,
    checkpoint_error: Option<String>,
}
pub fn check_er_readiness(rpc: &Rpc) -> Result<()> {
    let status: ErSyncStatus =
        serde_json::from_value(rpc.call("northstarSysGetSyncStatus", json!([]))?)
            .context("invalid ER sync status")?;
    ensure!(
        !status.is_syncing
            && status.latest_synced_slot >= status.latest_l1_slot
            && status.checkpoint_error.is_none(),
        "ER is syncing or checkpoint recovery is quarantined"
    );
    Ok(())
}
pub fn check_deployment(settings: &Settings, rpc: &Rpc) -> Result<ProgramConfig> {
    if let Some((session, slot)) = checked_programs(settings, rpc)? {
        session.check_l1_slot(slot)?;
        check_er_readiness(rpc)?;
    }
    let address = accounts::config_address(&settings.coordinator).0;
    let account = rpc
        .account(&address, 0)?
        .context("Coordinator is not initialized")?;
    ensure!(!account.executable, "config must not be executable");
    let config = ProgramConfig::decode(
        &settings.coordinator,
        &address,
        &account.owner,
        &account.data,
    )?;
    ensure!(config.status != 2, "Coordinator fulfillment is halted");
    ensure!(
        config.fulfillers.contains(&settings.fulfiller),
        "signer is not an active fulfiller"
    );
    Ok(config)
}
pub fn signed_attempt(
    rpc: &Rpc,
    signer: &Keypair,
    instructions: &[Instruction],
) -> Result<Attempt> {
    attempt_in_window(rpc, signer, instructions, None, 0)?.context("missing signed attempt")
}
fn attempt_in_window(
    rpc: &Rpc,
    signer: &Keypair,
    instructions: &[Instruction],
    window: Option<(u64, u64)>,
    min_context_slot: u64,
) -> Result<Option<Attempt>> {
    let block = rpc.call(
        "getLatestBlockhash",
        json!(
            [{ "commitment" : rpc.commitment, "minContextSlot" : min_context_slot }]
        ),
    )?;
    let context_slot = number(&block["context"]["slot"])?;
    ensure!(
        context_slot >= min_context_slot,
        "stale RPC blockhash context"
    );
    if window.is_some_and(|(created, deadline)| context_slot <= created || context_slot > deadline)
    {
        return Ok(None);
    }
    let hash: Hash = string(&block["value"]["blockhash"])?.parse()?;
    let height = number(&block["value"]["lastValidBlockHeight"])?;
    let message = Message::new_with_blockhash(instructions, Some(&signer.pubkey()), &hash);
    let mut transaction = Transaction::new_unsigned(message);
    transaction.try_sign(&[signer], hash)?;
    let wire = bincode::serialize(&transaction)?;
    ensure!(wire.len() <= 1232, "transaction exceeds packet limit");
    Ok(Some(Attempt {
        signature: transaction.signatures[0].to_string(),
        wire_base64: STANDARD.encode(wire),
        last_valid_block_height: height,
    }))
}
pub fn request_id(binding: &str, address: &solana_pubkey::Pubkey, request: &Request) -> String {
    hex::encode(
        Sha256::new()
            .chain_update(b"northstar/oracle/request/v1\0")
            .chain_update(binding)
            .chain_update(address.as_ref())
            .chain_update(request.alpha)
            .chain_update(request.vrf_key)
            .finalize(),
    )
}
fn result(record: &Record) -> Result<VrfResult> {
    Ok(VrfResult {
        proof: hex::decode(&record.proof_hex)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid journal proof length"))?,
        output: hex::decode(&record.output_hex)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid journal output length"))?,
    })
}
fn fulfillment(
    settings: &Settings,
    record: &Record,
    request: &Request,
) -> Result<Vec<Instruction>> {
    Ok(vec![
        instructions::compute_limit(1_400_000)?,
        instructions::fulfill(
            settings.coordinator,
            settings.verifier,
            settings.fulfiller,
            record.address,
            request,
            &result(record)?.proof,
        )?,
    ])
}
pub fn validate_record(
    settings: &Settings,
    binding: &str,
    id: &str,
    record: &Record,
) -> Result<Request> {
    let bytes = STANDARD.decode(&record.request_base64)?;
    let request = Request::decode(
        &settings.coordinator,
        &record.address,
        &settings.coordinator,
        &bytes,
    )?;
    ensure!(
        request.status == 0 && request.vrf_key == settings.vrf_public_key.to_bytes(),
        "journal request/key mismatch"
    );
    ensure!(
        request_id(binding, &record.address, &request) == id,
        "journal request identity mismatch"
    );
    ensure!(
        (1..=settings.max_attempts).contains(&record.attempts),
        "journal attempt count out of bounds"
    );
    ensure!(
        (record.status == Status::Confirmed) == record.confirmed_slot.is_some(),
        "journal receipt state mismatch"
    );
    crypto::verify_output(&request.vrf_key, &request.alpha, &result(record)?)?;
    let wire = STANDARD.decode(&record.attempt.wire_base64)?;
    ensure!(wire.len() <= 1232, "journal transaction too large");
    let transaction: Transaction = bincode::options()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .with_limit(1232)
        .deserialize(&wire)?;
    transaction
        .verify()
        .map_err(|_| anyhow::anyhow!("invalid journal transaction signature"))?;
    ensure!(
        transaction.signatures.len() == 1
            && transaction.signatures[0].to_string() == record.attempt.signature,
        "journal signature mismatch"
    );
    let expected = Message::new_with_blockhash(
        &fulfillment(settings, record, &request)?,
        Some(&settings.fulfiller),
        &transaction.message.recent_blockhash,
    );
    ensure!(
        transaction.message == expected,
        "journal transaction differs from pinned fulfillment"
    );
    Ok(request)
}
impl Worker {
    pub fn open_relay(
        settings: RelaySettings,
        source: std::sync::Arc<dyn CommitmentSource>,
    ) -> Result<Self> {
        settings.validate()?;
        let (settings, remote) = settings.into_parts();
        let rpc = Self::checked_rpc(&settings)?;
        let prover = RemoteProver::open(&settings, remote, source)?;
        Self::open_inner(settings, rpc, Box::new(prover))
    }
    fn checked_rpc(settings: &Settings) -> Result<Rpc> {
        if settings.ws_url.is_some() {
            settings.validate()?;
        }
        let rpc = Rpc::new(&settings.rpc_url, &settings.commitment)?;
        check_deployment(settings, &rpc)?;
        Ok(rpc)
    }
    fn open_inner(settings: Settings, rpc: Rpc, prover: Box<dyn RequestProver>) -> Result<Self> {
        let signer = keys::load(&settings.signer_file)?;
        ensure!(
            signer.pubkey() == settings.fulfiller,
            "signer identity mismatch"
        );
        ensure!(
            prover.public_key() == settings.vrf_public_key.to_bytes(),
            "proof provider key differs"
        );
        let base = settings.binding()?;
        let binding = prover.binding_suffix().map_or_else(
            || base.clone(),
            |suffix| {
                hex::encode(Sha256::digest(format!(
                    "northstar-vrf-relay-journal-v1:{base}:{suffix}"
                )))
            },
        );
        let mut journal = Journal::open(&settings.state_dir, &binding, settings.max_records)?;
        for (id, record) in &journal.snapshot().records {
            validate_record(&settings, &binding, id, record)?;
            prover.validate_receipt(record)?;
        }
        ensure!(
            !journal.snapshot().l1_discovery_established || settings.ws_url.is_some(),
            "established L1 subscription discovery cannot be disabled"
        );
        let discovery = if settings.ws_url.is_some() {
            Some(L1Discovery::open(&settings, &rpc, &mut journal)?)
        } else {
            None
        };
        Ok(Self {
            settings,
            rpc,
            signer,
            prover,
            binding,
            journal,
            discovery,
        })
    }
    fn mark(&mut self, id: &str, status: Status, slot: Option<u64>) -> Result<()> {
        self.journal.change(|snapshot| {
            let record = snapshot.records.get_mut(id).context("record missing")?;
            record.status = status.clone();
            record.confirmed_slot = slot;
            Ok(())
        })?;
        println!(
            "{}",
            json!({ "event" : "state", "request_id" : id, "status" : status, "slot"
            : slot })
        );
        Ok(())
    }
    fn dispatch(&self, record: &Record) -> Result<()> {
        if self
            .rpc
            .send(
                &STANDARD.decode(&record.attempt.wire_base64)?,
                &record.attempt.signature,
            )
            .is_err()
        {
            eprintln!(
                "{}",
                json!({ "event" : "submission_unknown", "signature" : record
                .attempt.signature })
            );
        } else {
            println!(
                "{}",
                json!({ "event" : "submitted", "signature" : record.attempt
                .signature, "request" : record.address.to_string() })
            );
        }
        Ok(())
    }
    fn discover_requests(&mut self) -> Result<(u64, Vec<(Pubkey, Account)>)> {
        if let Some(candidates) = self.prover.candidates() {
            let candidates = candidates?;
            ensure!(
                candidates.len() <= northstar_vrf_protocol::MAX_STATE_ACCOUNTS
                    && candidates.iter().all(|key| *key != Pubkey::default())
                    && candidates
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == candidates.len(),
                "native request candidate list exceeds bounds or contains aliases"
            );
            let mut context = self.rpc.slot()?;
            let mut present = Vec::new();
            for batch in candidates.chunks(crate::rpc::MAX_ACCOUNT_BATCH) {
                let (slot, accounts) = self.rpc.accounts_at(batch, context)?;
                context = slot;
                present.extend(
                    batch
                        .iter()
                        .copied()
                        .zip(accounts)
                        .filter_map(|(address, account)| account.map(|account| (address, account))),
                );
            }
            return Ok((context, present));
        }
        if let Some(discovery) = &mut self.discovery {
            return discovery.discover(&self.settings, &self.rpc);
        }
        if !self.settings.request_addresses.is_empty() {
            let (slot, accounts) = self
                .rpc
                .accounts_at(&self.settings.request_addresses, self.rpc.slot()?)?;
            let present = self
                .settings
                .request_addresses
                .iter()
                .copied()
                .zip(accounts)
                .filter_map(|(key, account)| account.map(|account| (key, account)))
                .collect();
            return Ok((slot, present));
        }
        let discovery = self.rpc.call(
            "getProgramAccounts",
            json!(
                [self.settings.coordinator.to_string(), { "encoding" : "base64",
                "commitment" : self.settings.commitment, "withContext" : true,
                "filters" : [{ "memcmp" : { "offset" : 0, "bytes" :
                bs58::encode(accounts::discriminator("account", "RequestAccount"))
                .into_string() } }, { "memcmp" : { "offset" : 9, "bytes" : "1" } }]
                }]
            ),
        )?;
        let slot = number(&discovery["context"]["slot"])?;
        let entries = discovery["value"]
            .as_array()
            .context("invalid discovery result")?;
        let accounts = entries
            .iter()
            .map(|entry| {
                Ok((
                    string(&entry["pubkey"])?.parse()?,
                    decode_account(&entry["account"])?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((slot, accounts))
    }
    pub fn tick(&mut self) -> Result<()> {
        self.tick_until(|| false)
    }
    pub fn tick_until(&mut self, stop: impl Fn() -> bool) -> Result<()> {
        if stop() {
            return Ok(());
        }
        check_deployment(&self.settings, &self.rpc)?;
        let height = self.rpc.height()?;
        let records = self.journal.snapshot().records.clone();
        for (id, record) in records {
            if stop() {
                return Ok(());
            }
            if record.status == Status::Pending {
                if let Some(receipt) = self.rpc.status(&record.attempt.signature)? {
                    if receipt.error.is_none()
                        && receipt.confirmation.satisfies(&self.settings.commitment)
                    {
                        self.mark(&id, Status::Confirmed, Some(receipt.slot))?;
                    } else if receipt.error.is_some()
                        && receipt.confirmation == Confirmation::Finalized
                    {
                        self.mark(
                            &id,
                            if record.attempts >= self.settings.max_attempts {
                                Status::Exhausted
                            } else {
                                Status::Retryable
                            },
                            None,
                        )?;
                    }
                    continue;
                }
                if height > record.attempt.last_valid_block_height {
                    self.mark(&id, Status::Unresolved, None)?;
                    continue;
                }
                if let Some(current) = self.rpc.account(&record.address, 0)?
                    && !current.executable
                    && current.owner == self.settings.coordinator
                    && STANDARD.encode(&current.data) == record.request_base64
                    && !stop()
                {
                    self.dispatch(&record)?;
                }
            } else if record.status == Status::Retryable {
                let request = validate_record(&self.settings, &self.binding, &id, &record)?;
                self.prover.validate_receipt(&record)?;
                let Some(receipt) = self.rpc.status(&record.attempt.signature)? else {
                    continue;
                };
                ensure!(
                    receipt.error.is_some() && receipt.confirmation == Confirmation::Finalized,
                    "retry receipt no longer records a finalized failure"
                );
                let minimum = receipt.slot.max(request.request_slot.saturating_add(1));
                let (slot, current) = self.rpc.account_at(&record.address, minimum)?;
                if !current.is_some_and(|a| {
                    !a.executable
                        && a.owner == self.settings.coordinator
                        && STANDARD.encode(a.data) == record.request_base64
                }) {
                    self.mark(&id, Status::Unresolved, None)?;
                    continue;
                }
                if slot > request.deadline_slot {
                    self.mark(&id, Status::Expired, None)?;
                    continue;
                }
                if slot <= request.request_slot {
                    continue;
                }
                if stop() {
                    return Ok(());
                }
                let Some(attempt) = attempt_in_window(
                    &self.rpc,
                    &self.signer,
                    &fulfillment(&self.settings, &record, &request)?,
                    Some((request.request_slot, request.deadline_slot)),
                    slot,
                )?
                else {
                    self.mark(&id, Status::Expired, None)?;
                    continue;
                };
                if attempt.signature == record.attempt.signature {
                    continue;
                }
                let mut next = record;
                next.attempts += 1;
                next.attempt = attempt;
                next.status = Status::Pending;
                self.journal.change(|s| {
                    s.records.insert(id, next.clone());
                    Ok(())
                })?;
                if stop() {
                    return Ok(());
                }
                self.dispatch(&next)?;
            }
        }
        if stop() {
            return Ok(());
        }
        let (context_slot, discovered) = self.discover_requests()?;
        for (address, observed) in discovered {
            if stop() {
                return Ok(());
            }
            ensure!(!observed.executable, "request is executable");
            let request = Request::decode(
                &self.settings.coordinator,
                &address,
                &observed.owner,
                &observed.data,
            )?;
            if request.status != 0
                || request.vrf_key != self.prover.public_key()
                || context_slot <= request.request_slot
                || context_slot > request.deadline_slot
                || request
                    .callback_accounts
                    .iter()
                    .any(|account| account.key == self.settings.fulfiller)
            {
                continue;
            }
            if (request.mode == 1 && !self.settings.allow_store)
                || (request.mode == 0
                    && !self.settings.allowed_consumers.contains(&request.requester))
            {
                continue;
            }
            let id = request_id(&self.binding, &address, &request);
            if self.journal.snapshot().records.contains_key(&id) {
                continue;
            }
            ensure!(
                self.journal.snapshot().records.len() < self.settings.max_records,
                "journal full; no records were evicted"
            );
            let (account_slot, current) = self.rpc.account_at(&address, context_slot)?;
            if !current.is_some_and(|a| {
                a.owner == observed.owner && a.data == observed.data && !a.executable
            }) {
                continue;
            }
            if account_slot <= request.request_slot || account_slot > request.deadline_slot {
                continue;
            }
            if stop() {
                return Ok(());
            }
            let Some(proved) = self.prover.prove(&address, &observed, &request)? else {
                continue;
            };
            if stop() {
                return Ok(());
            }
            let proof = proved.result;
            let mut record = Record {
                address,
                request_base64: STANDARD.encode(&observed.data),
                proof_hex: hex::encode(proof.proof),
                output_hex: hex::encode(proof.output),
                signing_receipt: proved.receipt,
                attempts: 1,
                status: Status::Pending,
                confirmed_slot: None,
                attempt: Attempt {
                    signature: String::new(),
                    wire_base64: String::new(),
                    last_valid_block_height: 0,
                },
            };
            let Some(attempt) = attempt_in_window(
                &self.rpc,
                &self.signer,
                &fulfillment(&self.settings, &record, &request)?,
                Some((request.request_slot, request.deadline_slot)),
                account_slot,
            )?
            else {
                continue;
            };
            record.attempt = attempt;
            self.journal.change(|s| {
                s.records.insert(id, record.clone());
                Ok(())
            })?;
            if stop() {
                return Ok(());
            }
            self.dispatch(&record)?;
        }
        Ok(())
    }
    pub fn summary(&self) -> Value {
        json!(
            { "records" : self.journal.snapshot().records.iter().map(| (id, r) | json!({
            "id" : id, "request" : r.address.to_string(), "status" : r.status, "attempts"
            : r.attempts, "signature" : r.attempt.signature, "output_hex" : r.output_hex
            })).collect::< Vec < _ >> () }
        )
    }
}
