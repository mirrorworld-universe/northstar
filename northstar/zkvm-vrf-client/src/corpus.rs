use {
    crate::{digest_text, public_values, read_bounded, sha256, INPUT_LIMIT},
    anyhow::{ensure, Context, Result},
    serde::{Deserialize, Serialize},
    std::{
        collections::{BTreeMap, HashSet},
        path::{Component, Path},
    },
};

const GENERATOR: &str = "f71c0eb302b6874d0e750e4222d0df8f78967dc8575dcb7d158bea69fdebc3c1";
const SOURCES: [(&str, &str); 4] = [
    (
        "northstar/transaction-proof/src/vrf_candidate.rs",
        "f72341d3c089a6f1a7978d8cf8ecf02b9a8e02e0559870ea077b69760f7f1835",
    ),
    (
        "northstar/transaction-proof/src/vrf_candidate/binding.rs",
        "06e91bdcb8a20653735b00f1fd00c1830c1793132126988b07f6a0018331e14b",
    ),
    (
        "northstar/transaction-proof/src/vrf_candidate/wire.rs",
        "4cc4e5c30a252e4e1cef3987ad9b7ca789ff1afbf7c6242170429154893adb4a",
    ),
    (
        "northstar/transaction-proof/src/vrf_candidate/ids.rs",
        "a0846928ee7a1f17ad4f2e5875e60be56a7d903ccc5943fc5c110d5465357b95",
    ),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    schema: String,
    construction_only: bool,
    engine_replayed: bool,
    guest_execution_observed: bool,
    proof_generated: bool,
    proof_kind: u8,
    proof_version: u8,
    domain_hex: String,
    policy_sha256: String,
    checkpoint_steps: u64,
    transaction_max_age: u64,
    generator_source_sha256: String,
    binding_sources_sha256: BTreeMap<String, String>,
    positive_cases: Vec<Positive>,
    controls: Vec<Control>,
    unsupported_signed_transaction: SignedSource,
    skipped_controls: Vec<Skipped>,
    acceptance_rule: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Positive {
    id: String,
    file: String,
    sha256: String,
    public_inputs_file: String,
    public_inputs_sha256: String,
    expected_guest_success: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    id: String,
    file: String,
    sha256: String,
    bytes: usize,
    basis: String,
    basis_sha256: String,
    category: String,
    changed_field: String,
    canonical_borsh: bool,
    derived_checkpoint_rebound: bool,
    expected_guard: String,
    missing_invariant: String,
    expected_guest_exit: String,
    expected_public_output_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedSource {
    file: String,
    sha256: String,
    message_signatures_checked: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Skipped {
    id: String,
    reason: String,
}

#[derive(Clone, Serialize)]
pub struct CorpusCase {
    pub id: String,
    pub file: String,
    pub sha256: String,
    pub basis: String,
    pub public_inputs_file: String,
    pub public_inputs_sha256: String,
    pub expected_rejection: bool,
    pub input_bytes: Option<usize>,
}

fn id(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid corpus id"
    );
    Ok(())
}

fn bounded_text(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 1024,
        "invalid corpus annotation"
    );
    Ok(())
}

fn relative(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 256
            && Path::new(value)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "corpus artifact path must be safe and relative"
    );
    Ok(())
}

fn artifact(root: &Path, file: &str, hash: &str, limit: usize) -> Result<Vec<u8>> {
    relative(file)?;
    digest_text(hash)?;
    let root = root.canonicalize()?;
    let path = root.join(file).canonicalize()?;
    ensure!(path.starts_with(&root), "corpus artifact escapes root");
    let bytes = read_bounded(&path, limit)?;
    ensure!(
        sha256(&bytes) == hash,
        "corpus artifact SHA-256 mismatch: {file}"
    );
    Ok(bytes)
}

impl Corpus {
    pub fn parse(bytes: &[u8], policy: &str) -> Result<Self> {
        ensure!(bytes.len() <= 1024 * 1024, "corpus manifest exceeds bound");
        let corpus: Self = serde_json::from_slice(bytes)?;
        ensure!(
            corpus.schema == "northstar-vrf-candidate-controls-v1"
                && corpus.construction_only
                && !corpus.engine_replayed
                && !corpus.guest_execution_observed
                && !corpus.proof_generated,
            "expected construction-only control corpus v1"
        );
        ensure!(
            corpus.proof_kind == 3
                && corpus.proof_version == 1
                && corpus.domain_hex
                    == hex::encode(northstar_zk_types::FrBytes::er_step_domain_v1(3, 1).as_bytes())
                && corpus.checkpoint_steps == 1
                && corpus.transaction_max_age == 900,
            "unsupported corpus domain/policy bounds"
        );
        digest_text(&corpus.policy_sha256)?;
        ensure!(
            corpus.policy_sha256 == policy,
            "corpus/deployment policy mismatch"
        );
        ensure!(
            corpus.generator_source_sha256 == GENERATOR
                && corpus.binding_sources_sha256.len() == SOURCES.len(),
            "unreviewed corpus source identity"
        );
        for (path, hash) in SOURCES {
            ensure!(
                corpus.binding_sources_sha256.get(path).map(String::as_str) == Some(hash),
                "unreviewed binding source identity: {path}"
            );
        }
        ensure!(
            corpus.positive_cases.len() == 2
                && corpus.controls.len() == 15
                && corpus.skipped_controls.len() <= 16,
            "corpus requires two positives and fifteen controls"
        );
        bounded_text(&corpus.acceptance_rule)?;
        let mut ids = HashSet::new();
        let mut files = HashSet::new();
        let mut inputs = HashSet::new();
        let mut publics = HashSet::new();
        for positive in &corpus.positive_cases {
            ensure!(
                positive.expected_guest_success,
                "positive must require guest success"
            );
            relative(&positive.public_inputs_file)?;
            digest_text(&positive.public_inputs_sha256)?;
            ensure!(
                files.insert(positive.public_inputs_file.clone())
                    && publics.insert(positive.public_inputs_sha256.clone()),
                "duplicate baseline public artifact"
            );
        }
        for control in &corpus.controls {
            ensure!(
                control.expected_guest_exit == "nonzero"
                    && control.expected_public_output_bytes == 0,
                "negative requires nonzero guest exit and empty output"
            );
            ensure!(
                (1..=INPUT_LIMIT).contains(&control.bytes),
                "control byte bound"
            );
            ensure!(
                matches!(
                    control.category.as_str(),
                    "incorrect_witness"
                        | "unsupported_lifecycle"
                        | "malformed_encoding"
                        | "unsupported_transaction_shape"
                ),
                "unknown control category"
            );
            ensure!(
                control.canonical_borsh == (control.category != "malformed_encoding"),
                "encoding annotation mismatch"
            );
            ensure!(
                !control.derived_checkpoint_rebound || control.canonical_borsh,
                "invalid rebound annotation"
            );
            for text in [
                &control.changed_field,
                &control.expected_guard,
                &control.missing_invariant,
            ] {
                bounded_text(text)?;
            }
            let basis = corpus
                .positive_cases
                .iter()
                .find(|p| p.id == control.basis)
                .context("unknown control basis")?;
            ensure!(
                control.basis_sha256 == basis.sha256 && control.sha256 != basis.sha256,
                "control basis/source mismatch"
            );
        }
        for case in corpus.cases() {
            id(&case.id)?;
            relative(&case.file)?;
            digest_text(&case.sha256)?;
            ensure!(
                ids.insert(case.id) && files.insert(case.file) && inputs.insert(case.sha256),
                "duplicate corpus id/file/input"
            );
        }
        for skipped in &corpus.skipped_controls {
            id(&skipped.id)?;
            bounded_text(&skipped.reason)?;
            ensure!(ids.insert(skipped.id.clone()), "duplicate skipped id");
        }
        relative(&corpus.unsupported_signed_transaction.file)?;
        digest_text(&corpus.unsupported_signed_transaction.sha256)?;
        ensure!(
            corpus
                .unsupported_signed_transaction
                .message_signatures_checked
                && files.insert(corpus.unsupported_signed_transaction.file.clone()),
            "invalid signed source metadata"
        );
        Ok(corpus)
    }

    pub fn cases(&self) -> Vec<CorpusCase> {
        self.positive_cases
            .iter()
            .map(|p| CorpusCase {
                id: p.id.clone(),
                file: p.file.clone(),
                sha256: p.sha256.clone(),
                basis: p.id.clone(),
                public_inputs_file: p.public_inputs_file.clone(),
                public_inputs_sha256: p.public_inputs_sha256.clone(),
                expected_rejection: false,
                input_bytes: None,
            })
            .chain(self.controls.iter().map(|c| {
                let basis = self
                    .positive_cases
                    .iter()
                    .find(|p| p.id == c.basis)
                    .expect("validated basis");
                CorpusCase {
                    id: c.id.clone(),
                    file: c.file.clone(),
                    sha256: c.sha256.clone(),
                    basis: c.basis.clone(),
                    public_inputs_file: basis.public_inputs_file.clone(),
                    public_inputs_sha256: basis.public_inputs_sha256.clone(),
                    expected_rejection: true,
                    input_bytes: Some(c.bytes),
                }
            }))
            .collect()
    }

    pub fn selection(&self, selected: &[String]) -> Result<Vec<bool>> {
        ensure!(selected.len() <= 17, "selection exceeds corpus");
        let cases = self.cases();
        let mut seen = HashSet::new();
        for name in selected {
            ensure!(
                seen.insert(name) && cases.iter().any(|c| &c.id == name),
                "duplicate or unknown selected id"
            );
        }
        Ok(cases
            .iter()
            .map(|c| selected.is_empty() || seen.contains(&c.id))
            .collect())
    }

    pub fn preflight(&self, root: &Path) -> Result<()> {
        artifact(
            root,
            &self.unsupported_signed_transaction.file,
            &self.unsupported_signed_transaction.sha256,
            1232,
        )?;
        for case in self.cases() {
            case.load(root)?;
        }
        Ok(())
    }
}

impl CorpusCase {
    pub fn load(&self, root: &Path) -> Result<(Vec<u8>, [u8; 256])> {
        let input = artifact(root, &self.file, &self.sha256, INPUT_LIMIT)?;
        if let Some(expected) = self.input_bytes {
            ensure!(input.len() == expected, "control size mismatch");
        }
        let public = artifact(
            root,
            &self.public_inputs_file,
            &self.public_inputs_sha256,
            256,
        )?;
        Ok((input, public_values(&public)?))
    }
}

pub enum Observation<'a> {
    Returned { exit_code: u64, public: &'a [u8] },
    HostError,
    Panic,
    Timeout,
    Missing,
}

pub fn observe(observation: Observation<'_>, expected: &[u8; 256], rejection: bool) -> Result<()> {
    match observation {
        Observation::Returned { exit_code, public } => {
            crate::execution_result(exit_code, public, expected, rejection)
        }
        _ => anyhow::bail!("missing returned guest report; host failure is not a guest rejection"),
    }
}
