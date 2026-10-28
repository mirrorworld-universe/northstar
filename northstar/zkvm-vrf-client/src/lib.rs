use {
    anyhow::{bail, ensure, Context, Result},
    clap::{Parser, ValueEnum},
    northstar_zk_types::{ErStepPublicInputsV1, FrBytes, Sp1Groth16ProofV1},
    serde::{Deserialize, Serialize},
    sha2::{Digest, Sha256},
    std::{fs, path::PathBuf},
};

pub const FIELDS: [&str; 8] = [
    "domain",
    "session_context",
    "slot_step",
    "pre_state_root",
    "post_state_root",
    "tx_effect_root",
    "readonly_l1_root",
    "settlement_effect_root",
];
pub const LEGACY_KEY: &str = "0x0063a10e31e90625b76d06bfa22cbf0a4a8fa0850ccf71e063d243bd1a7aa31b";
pub const INPUT_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Key,
    Execute,
    ExecuteCorpus,
    Groth16,
    Verify,
}

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Development-only SP1 6.8 runtime-ELF bounded VRF client"
)]
pub struct Args {
    #[arg(value_enum)]
    pub command: Command,
    #[arg(long)]
    pub elf: PathBuf,
    #[arg(long)]
    pub profile: PathBuf,
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long)]
    pub input: Option<PathBuf>,
    #[arg(long)]
    pub batch: Option<PathBuf>,
    #[arg(long)]
    pub corpus: Option<PathBuf>,
    #[arg(long = "case-id")]
    pub case_ids: Vec<String>,
    #[arg(long = "public")]
    pub public_inputs: Option<PathBuf>,
    #[arg(long)]
    pub setup_receipt: Option<PathBuf>,
    #[arg(long)]
    pub proof: Option<PathBuf>,
    #[arg(long, default_value_t = 1)]
    pub samples: usize,
    #[arg(long)]
    pub expect_rejection: bool,
    #[arg(long, default_value_t = 1_000_000_000)]
    pub cycle_limit: u64,
    #[arg(long, default_value = "not-inspected")]
    pub cache_state: String,
}

impl Args {
    pub fn validate(&self, backend: Option<&str>) -> Result<()> {
        ensure!(matches!(self.samples, 1 | 3), "samples must be 1 or 3");
        ensure!(
            self.samples == 1 || self.command == Command::Groth16,
            "samples require groth16"
        );
        ensure!(
            (1..=10_000_000_000).contains(&self.cycle_limit),
            "cycle limit out of bounds"
        );
        ensure!(
            matches!(
                self.cache_state.as_str(),
                "not-inspected" | "verified-present" | "cold"
            ),
            "unknown cache state"
        );
        ensure!(
            !self.expect_rejection || self.command == Command::Execute,
            "rejection control requires execute"
        );
        let expected = if self.command == Command::Groth16 {
            "cuda"
        } else {
            "cpu"
        };
        ensure!(
            backend.unwrap_or("cpu") == expected,
            "this command requires SP1_PROVER={expected}; network/mock/hosted/light backends are \
             forbidden"
        );
        ensure!(
            self.batch.is_none() || self.command == Command::Groth16,
            "batch requires groth16"
        );
        ensure!(
            (self.corpus.is_none() && self.case_ids.is_empty())
                || self.command == Command::ExecuteCorpus,
            "corpus and case selection require execute-corpus"
        );
        match self.command {
            Command::ExecuteCorpus => ensure!(
                self.corpus.is_some()
                    && self.setup_receipt.is_some()
                    && self.input.is_none()
                    && self.public_inputs.is_none()
                    && self.proof.is_none()
                    && self.samples == 1,
                "execute-corpus requires only --corpus --setup-receipt and optional --case-id"
            ),
            Command::Groth16 if self.batch.is_some() => ensure!(
                self.samples == 1
                    && self.input.is_none()
                    && self.public_inputs.is_none()
                    && self.setup_receipt.is_some()
                    && self.proof.is_none(),
                "batch requires only --batch --setup-receipt, not single-input/sample flags"
            ),
            Command::Key => ensure!(
                self.input.is_none()
                    && self.public_inputs.is_none()
                    && self.setup_receipt.is_none()
                    && self.proof.is_none(),
                "key accepts no execution/proof inputs"
            ),
            Command::Execute | Command::Groth16 => ensure!(
                self.input.is_some()
                    && self.public_inputs.is_some()
                    && self.setup_receipt.is_some()
                    && self.proof.is_none(),
                "execution requires --input --public --setup-receipt"
            ),
            Command::Verify => ensure!(
                self.input.is_none()
                    && self.public_inputs.is_some()
                    && self.setup_receipt.is_some()
                    && self.proof.is_some(),
                "verify requires --proof --public --setup-receipt"
            ),
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct Profile {
    pub proof_kind: u8,
    pub proof_version: u8,
    pub public_input_fields: Vec<String>,
    pub production_acceptance: bool,
    pub elf_sha256: String,
    pub guest_status: String,
    pub policy_profile_sha256: String,
}

pub fn profile(bytes: &[u8], elf: &[u8]) -> Result<Profile> {
    let value: Profile = serde_json::from_slice(bytes).context("profile JSON")?;
    ensure!(
        value.proof_kind == 3 && value.proof_version == 1,
        "profile requires kind3/version1"
    );
    ensure!(
        value.public_input_fields == FIELDS,
        "public field order mismatch"
    );
    ensure!(
        !value.production_acceptance,
        "development-only profile required"
    );
    ensure!(
        value.guest_status == "authenticated-envelope",
        "qualification guest is not a candidate target"
    );
    elf_header(elf)?;
    digest_text(&value.policy_profile_sha256)?;
    ensure!(value.elf_sha256 == sha256(elf), "profile ELF hash mismatch");
    Ok(value)
}

pub fn elf_header(elf: &[u8]) -> Result<()> {
    ensure!(
        elf.len() >= 64 && elf.starts_with(b"\x7fELF"),
        "complete ELF64 header required"
    );
    ensure!(
        elf[4..7] == [2, 1, 1],
        "candidate requires ELF64 little-endian version1"
    );
    ensure!(
        elf[16..20] == [2, 0, 243, 0] && elf[20..24] == [1, 0, 0, 0],
        "candidate requires executable RISC-V ELF version1"
    );
    ensure!(elf[52..54] == [64, 0], "ELF64 header size mismatch");
    let flags = u32::from_le_bytes(elf[48..52].try_into()?);
    ensure!(
        flags == 0,
        "candidate requires RV64IM/soft-float ELF flags0; compressed/RVC or other flags are \
         unsupported (observed {flags:#x})"
    );
    Ok(())
}

pub fn public_values(bytes: &[u8]) -> Result<[u8; 256]> {
    let encoded: [u8; 256] = bytes
        .try_into()
        .context("public values must be 256 bytes")?;
    let mut fields = [[0; 32]; 8];
    for (field, input) in fields.iter_mut().zip(encoded.as_chunks::<32>().0) {
        field.copy_from_slice(input);
    }
    let value = ErStepPublicInputsV1::from_array(fields)
        .map_err(|e| anyhow::anyhow!("noncanonical public values: {e:?}"))?;
    ensure!(
        value.domain == FrBytes::er_step_domain_v1(3, 1),
        "public domain must be kind3/version1"
    );
    for field in [
        value.session_context,
        value.pre_state_root,
        value.post_state_root,
        value.tx_effect_root,
        value.readonly_l1_root,
        value.settlement_effect_root,
    ] {
        ensure!(field != FrBytes::ZERO, "missing public commitment");
    }
    Ok(encoded)
}

pub fn program_key(value: &str) -> Result<()> {
    ensure!(
        value.len() == 66
            && value.starts_with("0x")
            && value[2..]
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "noncanonical program key text"
    );
    let raw: [u8; 32] = hex::decode(&value[2..])?
        .try_into()
        .map_err(|_| anyhow::anyhow!("key length"))?;
    let field = FrBytes::new(raw).map_err(|e| anyhow::anyhow!("invalid program key: {e:?}"))?;
    ensure!(
        field != FrBytes::ZERO && value != LEGACY_KEY,
        "candidate must have a nonzero, nonlegacy key"
    );
    Ok(())
}

pub fn execution_result(
    exit_code: u64,
    public: &[u8],
    expected: &[u8; 256],
    rejection: bool,
) -> Result<()> {
    if rejection {
        ensure!(
            exit_code != 0 && public.is_empty(),
            "rejection requires observed nonzero exit and no public output"
        );
    } else {
        ensure!(
            exit_code == 0,
            "guest execution rejected with exit {exit_code}"
        );
        public_values(public)?;
        ensure!(
            public == expected,
            "guest public values differ from expected canonical bytes"
        );
    }
    Ok(())
}

pub fn proof_envelope(bytes: &[u8]) -> Result<()> {
    Sp1Groth16ProofV1::from_bytes(bytes)
        .map_err(|e| anyhow::anyhow!("Portal proof envelope: {e:?}"))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupReceipt {
    pub schema: String,
    pub sp1_version: String,
    pub elf_sha256: String,
    pub profile_sha256: String,
    pub policy_profile_sha256: String,
    pub program_vkey_hash: String,
    pub verifying_key_sha256: String,
    pub production_acceptance: bool,
}

impl SetupReceipt {
    pub fn check(&self, derived: &Self) -> Result<()> {
        program_key(&self.program_vkey_hash)?;
        ensure!(
            self.schema == "northstar-sp1-setup-v1"
                && self.sp1_version == "6.8.0"
                && !self.production_acceptance,
            "invalid development setup receipt"
        );
        ensure!(
            self.elf_sha256 == derived.elf_sha256
                && self.profile_sha256 == derived.profile_sha256
                && self.policy_profile_sha256 == derived.policy_profile_sha256
                && self.program_vkey_hash == derived.program_vkey_hash
                && self.verifying_key_sha256 == derived.verifying_key_sha256,
            "SDK-derived setup identity differs from receipt"
        );
        Ok(())
    }
}

pub mod corpus;
pub mod session;

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn read_bounded(path: &std::path::Path, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "input exceeds size bound");
    Ok(bytes)
}

pub fn new_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

pub fn digest_text(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            && value.bytes().any(|c| c != b'0'),
        "expected nonzero lowercase SHA-256"
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchCase {
    pub case_id: String,
    pub input: PathBuf,
    pub input_sha256: String,
    pub expected_public: PathBuf,
    pub public_inputs_sha256: String,
    pub prepare_record: PathBuf,
    pub prepare_record_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub schema: String,
    pub warmup: Option<BatchCase>,
    pub cases: Vec<BatchCase>,
}

impl Batch {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        use std::collections::HashSet;
        let batch: Self = serde_json::from_slice(bytes)?;
        ensure!(
            batch.schema == "northstar-vrf-sp1-batch-v1" && batch.cases.len() == 3,
            "batch requires exactly three declared cases"
        );
        let mut ids = HashSet::new();
        let mut inputs = HashSet::new();
        let mut publics = HashSet::new();
        let mut prepared = HashSet::new();
        for case in &batch.cases {
            case.check()?;
            ensure!(
                ids.insert(case.case_id.clone())
                    && inputs.insert(case.input_sha256.clone())
                    && publics.insert(case.public_inputs_sha256.clone())
                    && prepared.insert(case.prepare_record_sha256.clone()),
                "batch cases must have distinct ids, inputs, public values and prepare records"
            );
        }
        if let Some(warmup) = &batch.warmup {
            warmup.check()?;
            ensure!(
                !ids.contains(&warmup.case_id),
                "warmup id overlaps declared window"
            );
        }
        Ok(batch)
    }
}

impl BatchCase {
    fn check(&self) -> Result<()> {
        ensure!(
            !self.case_id.is_empty()
                && self.case_id.len() <= 64
                && self
                    .case_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
            "invalid bounded case id"
        );
        for hash in [
            &self.input_sha256,
            &self.public_inputs_sha256,
            &self.prepare_record_sha256,
        ] {
            digest_text(hash)?;
        }
        for path in [&self.input, &self.expected_public, &self.prepare_record] {
            ensure!(
                !path.as_os_str().is_empty() && !path.is_absolute(),
                "batch artifact paths must be relative"
            );
            ensure!(
                path.components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "batch artifact path traversal"
            );
        }
        Ok(())
    }

    pub fn load(&self, root: &std::path::Path) -> Result<CaseInput> {
        self.check()?;
        let root = root.canonicalize()?;
        let read = |name: &std::path::Path, limit| -> Result<Vec<u8>> {
            let resolved = root.join(name).canonicalize()?;
            ensure!(resolved.starts_with(&root), "batch artifact escapes root");
            read_bounded(&resolved, limit)
        };
        let input = read(&self.input, INPUT_LIMIT)?;
        let public_bytes = read(&self.expected_public, 256)?;
        let prepare = read(&self.prepare_record, 1024 * 1024)?;
        ensure!(
            sha256(&input) == self.input_sha256
                && sha256(&public_bytes) == self.public_inputs_sha256
                && sha256(&prepare) == self.prepare_record_sha256,
            "batch artifact digest mismatch"
        );
        let expected = public_values(&public_bytes)?;
        let record: serde_json::Value = serde_json::from_slice(&prepare)?;
        ensure!(
            record["schema"] == "northstar.vrf-candidate-portal-prepared.v1"
                && record["kind"] == 3
                && record["version"] == 1
                && record["public_inputs_sha256"] == self.public_inputs_sha256,
            "prepare record does not bind candidate public values"
        );
        let text = |value: &serde_json::Value| -> Result<String> {
            Ok(value
                .as_str()
                .filter(|s| !s.is_empty())
                .context("prepare record missing identity")?
                .to_owned())
        };
        let qualified_session = session::qualify(&record, &input)?;
        let session = text(&record["session"]["session"])?;
        let checkpoint = text(&record["addresses"]["checkpoint"])?;
        let challenge = text(&record["addresses"]["challenge"])?;
        session::key_bytes(&checkpoint)?;
        session::key_bytes(&challenge)?;
        let deadline_slot: u64 = text(&record["deadline_slot"])?.parse()?;
        ensure!(deadline_slot != 0, "missing actual challenge deadline");
        Ok(CaseInput {
            input,
            expected,
            prepare,
            session,
            qualified_session,
            checkpoint,
            challenge,
            deadline_slot,
        })
    }
}

pub struct CaseInput {
    pub input: Vec<u8>,
    pub expected: [u8; 256],
    pub prepare: Vec<u8>,
    pub session: String,
    pub qualified_session: session::QualifiedSession,
    pub checkpoint: String,
    pub challenge: String,
    pub deadline_slot: u64,
}

#[derive(Default)]
pub struct BatchIdentities {
    sessions: std::collections::HashSet<String>,
    contexts: std::collections::HashSet<String>,
    sources: std::collections::HashSet<String>,
    checkpoints: std::collections::HashSet<(String, String)>,
    challenges: std::collections::HashSet<(String, String)>,
    deployment: Option<(String, String)>,
}

impl BatchIdentities {
    pub fn observe(&mut self, case: &CaseInput) -> Result<()> {
        let identity = &case.qualified_session;
        let checkpoint = (identity.identity_sha256.clone(), case.checkpoint.clone());
        let challenge = (identity.identity_sha256.clone(), case.challenge.clone());
        let deployment = (
            identity.program_id.clone(),
            identity.portal_elf_sha256.clone(),
        );
        ensure!(
            self.deployment
                .as_ref()
                .is_none_or(|expected| *expected == deployment),
            "three-case window changed Portal deployment"
        );
        ensure!(
            !self.sessions.contains(&identity.identity_sha256)
                && !self.contexts.contains(&identity.context_sha256)
                && !self.sources.contains(&identity.source_sha256)
                && !self.checkpoints.contains(&checkpoint)
                && !self.challenges.contains(&challenge),
            "three-case window requires distinct actual session contexts and qualified \
             checkpoint/challenge identities"
        );
        self.sessions.insert(identity.identity_sha256.clone());
        self.contexts.insert(identity.context_sha256.clone());
        self.sources.insert(identity.source_sha256.clone());
        self.checkpoints.insert(checkpoint);
        self.challenges.insert(challenge);
        self.deployment = Some(deployment);
        Ok(())
    }
}

pub fn check_lock(bytes: &str) -> Result<()> {
    let mut name = "";
    let mut checked = 0;
    for line in bytes.lines() {
        if let Some(value) = line
            .strip_prefix("name = \"")
            .and_then(|s| s.strip_suffix('"'))
        {
            name = value;
        }
        if let Some(version) = line
            .strip_prefix("version = \"")
            .and_then(|s| s.strip_suffix('"'))
        {
            if name.starts_with("sp1-") || name.starts_with("slop-") {
                ensure!(
                    version == "6.8.0",
                    "unreviewed internal dependency {name} {version}"
                );
                checked += 1;
            }
        }
    }
    if checked == 0 {
        bail!("lock has no reviewed SP1 packages");
    }
    Ok(())
}
