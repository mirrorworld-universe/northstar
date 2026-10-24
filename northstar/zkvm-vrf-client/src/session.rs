use {
    crate::{digest_text, sha256},
    anyhow::{ensure, Context, Result},
    borsh::BorshDeserialize,
    serde::{Deserialize, Serialize},
    serde_json::Value,
};

// Field order reproduces lifecycle v1's exact JSON.stringify(value, null, 2) + '\n'.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSource {
    pub schema: String,
    pub program_id: String,
    pub session: String,
    pub grid_id: String,
    pub nonce: String,
    pub validator: String,
    pub challenger: String,
    pub session_context_preimage_hex: String,
    pub session_account_sha256: String,
    pub created_at_slot: String,
    pub ttl_slots: String,
    pub genesis_hash: String,
}

pub fn key_bytes(text: &str) -> Result<[u8; 32]> {
    ensure!(
        (32..=44).contains(&text.len()),
        "invalid base58 identity length"
    );
    let decoded = bs58::decode(text).into_vec()?;
    let bytes: [u8; 32] = decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity is not 32 bytes"))?;
    ensure!(
        bs58::encode(bytes).into_string() == text,
        "noncanonical base58 identity"
    );
    Ok(bytes)
}

fn unsigned<T: std::str::FromStr>(text: &str) -> Result<T> {
    ensure!(
        text == "0"
            || (!text.starts_with('0')
                && !text.is_empty()
                && text.bytes().all(|b| b.is_ascii_digit())),
        "noncanonical unsigned session integer"
    );
    text.parse()
        .map_err(|_| anyhow::anyhow!("session integer overflow"))
}

impl SessionSource {
    pub fn context(&self) -> Result<[u8; 149]> {
        ensure!(
            self.schema == "northstar.vrf-candidate-portal-session.v1",
            "unknown session schema"
        );
        key_bytes(&self.challenger)?;
        key_bytes(&self.genesis_hash)?;
        digest_text(&self.session_account_sha256)?;
        unsigned::<u64>(&self.created_at_slot)?;
        unsigned::<u64>(&self.ttl_slots)?;
        let bytes = borsh::to_vec(&(
            *b"northstar-session-context-v1",
            key_bytes(&self.program_id)?,
            key_bytes(&self.session)?,
            unsigned::<u64>(&self.grid_id)?,
            unsigned::<u128>(&self.nonce)?,
            key_bytes(&self.validator)?,
            1u8,
        ))?;
        bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("session context width"))
    }

    pub fn source_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

// Only the canonical envelope's bounded session prefix is inspected here. The
// guest still owns complete decoding, execution and public-value recomputation.
#[derive(BorshDeserialize)]
struct SessionPrefix {
    magic: [u8; 8],
    version: u16,
    kind: u8,
    proof_version: u8,
    context_length: u32,
    context: [u8; 149],
}

pub struct QualifiedSession {
    pub identity_sha256: String,
    pub context_sha256: String,
    pub source_sha256: String,
    pub portal_elf_sha256: String,
    pub program_id: String,
}

pub fn qualify(record: &Value, input: &[u8]) -> Result<QualifiedSession> {
    let session: SessionSource = serde_json::from_value(record["session"].clone())?;
    let context = session.context()?;
    ensure!(
        hex::encode(context) == session.session_context_preimage_hex,
        "session fields/context mismatch"
    );
    let prefix = SessionPrefix::deserialize(&mut &input[..])?;
    ensure!(
        prefix.magic == *b"NSVRF001"
            && prefix.version == 1
            && prefix.kind == 3
            && prefix.proof_version == 1
            && prefix.context_length == 149
            && prefix.context == context,
        "prepared session does not match canonical envelope context"
    );
    let source_sha256 = sha256(&session.source_bytes()?);
    ensure!(
        record["session_source_sha256"] == source_sha256,
        "session source digest mismatch"
    );
    let genesis = session.genesis_hash.as_bytes();
    let mut identity = b"northstar-local-portal-session-identity-v1".to_vec();
    identity.extend_from_slice(&(genesis.len() as u32).to_le_bytes());
    identity.extend_from_slice(genesis);
    identity.extend_from_slice(&context);
    let identity_sha256 = sha256(&identity);
    ensure!(
        record["session_identity_sha256"] == identity_sha256,
        "qualified session digest mismatch"
    );
    let portal = &record["portal_program"];
    ensure!(
        portal["schema"] == "northstar.vrf-candidate-portal-program.v1"
            && portal["program_id"] == session.program_id,
        "prepared Portal deployment/program mismatch"
    );
    key_bytes(
        portal["loader"]
            .as_str()
            .context("missing actual Portal loader")?,
    )?;
    let portal_elf_sha256 = portal["elf_sha256"]
        .as_str()
        .context("missing actual Portal ELF identity")?
        .to_owned();
    digest_text(&portal_elf_sha256)?;
    Ok(QualifiedSession {
        identity_sha256,
        context_sha256: sha256(&context),
        source_sha256,
        portal_elf_sha256,
        program_id: session.program_id,
    })
}
