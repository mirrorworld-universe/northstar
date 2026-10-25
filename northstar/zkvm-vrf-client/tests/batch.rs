//! Synthetic prepared records/envelope prefixes only; no execution proof.
//! The public session-source golden vector comes from an actual setup, not the final window.
use {
    clap::Parser,
    northstar_zk_types::FrBytes,
    northstar_zkvm_vrf_client::{
        session::SessionSource, sha256, Args, Batch, BatchCase, BatchIdentities,
    },
    serde_json::json,
};

#[path = "batch/session.rs"]
mod session;

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "synthetic-vrf-batch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn manifest(&self) -> serde_json::Value {
        let mut cases = Vec::new();
        for index in 0..3u8 {
            let mut public = [0u8; 256];
            public[..32].copy_from_slice(FrBytes::er_step_domain_v1(3, 1).as_bytes());
            for field in 1..8 {
                public[field * 32 + 31] = index + 1;
            }
            let mut session: serde_json::Value =
                serde_json::from_str(include_str!("../fixtures/session-source-v1.json")).unwrap();
            let mut context =
                hex::decode(session["session_context_preimage_hex"].as_str().unwrap()).unwrap();
            let grid = u64::from(index) + 301;
            context[92..100].copy_from_slice(&grid.to_le_bytes());
            session["grid_id"] = json!(grid.to_string());
            session["genesis_hash"] = json!(bs58::encode([index + 7; 32]).into_string());
            session["session_context_preimage_hex"] = json!(hex::encode(&context));
            let mut input = b"NSVRF001".to_vec();
            input.extend_from_slice(&1u16.to_le_bytes());
            input.extend_from_slice(&[3, 1]);
            input.extend_from_slice(&149u32.to_le_bytes());
            input.extend_from_slice(&context);
            let genesis = session["genesis_hash"].as_str().unwrap().as_bytes();
            let mut identity = b"northstar-local-portal-session-identity-v1".to_vec();
            identity.extend_from_slice(&(genesis.len() as u32).to_le_bytes());
            identity.extend_from_slice(genesis);
            identity.extend_from_slice(&context);
            let source: SessionSource = serde_json::from_value(session.clone()).unwrap();
            let prepared = json!({"schema":"northstar.vrf-candidate-portal-prepared.v1","kind":3,"version":1,
                "public_inputs_sha256":sha256(&public),"session":session,
                "session_identity_sha256":sha256(&identity),"session_source_sha256":sha256(&source.source_bytes().unwrap()),
                "portal_program":{"schema":"northstar.vrf-candidate-portal-program.v1","program_id":source.program_id,
                    "loader":"BPFLoaderUpgradeab1e11111111111111111111111","elf_sha256":sha256(b"synthetic deployment")},
                "binding_sha256":sha256(b"synthetic binding"),"er_slot":"10","step_index":0,"prepared_slot":5,
                "addresses":{"checkpoint":"BH5yRd1S8hW9AejSTFA9o8SuxVGHQ4iSztSXg3xHHqqY","challenge":"Dran6bEBC2CP9894rdfYhae3Ps8QoTPqQD8Pmy5DcJRt"},"deadline_slot":"750"});
            let prepared = serde_json::to_vec(&prepared).unwrap();
            let input_name = format!("input-{index}");
            let public_name = format!("public-{index}");
            let prepare_name = format!("prepare-{index}.json");
            std::fs::write(self.0.join(&input_name), &input).unwrap();
            std::fs::write(self.0.join(&public_name), public).unwrap();
            std::fs::write(self.0.join(&prepare_name), &prepared).unwrap();
            cases.push(json!({"case_id":format!("case-{index}"),"input":input_name,"input_sha256":sha256(&input),
                "expected_public":public_name,"public_inputs_sha256":sha256(&public),"prepare_record":prepare_name,"prepare_record_sha256":sha256(&prepared)}));
        }
        json!({"schema":"northstar-vrf-sp1-batch-v1","warmup":null,"cases":cases})
    }
    fn prepare(&self, case: &BatchCase) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.0.join(&case.prepare_record)).unwrap()).unwrap()
    }
    fn rewrite(&self, case: &BatchCase, prepared: &serde_json::Value) -> BatchCase {
        let bytes = serde_json::to_vec(prepared).unwrap();
        std::fs::write(self.0.join(&case.prepare_record), &bytes).unwrap();
        let mut case = case.clone();
        case.prepare_record_sha256 = sha256(&bytes);
        case
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn three_distinct_case_bindings_and_explicit_separate_warmup() {
    let fixture = Fixture::new();
    let mut manifest = fixture.manifest();
    let batch = Batch::parse(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut identities = BatchIdentities::default();
    for case in &batch.cases {
        let loaded = case.load(&fixture.0).unwrap();
        assert_eq!(loaded.deadline_slot, 750);
        identities.observe(&loaded).unwrap();
    }
    manifest["warmup"] = manifest["cases"][0].clone();
    manifest["warmup"]["case_id"] = json!("retained-separate-warmup");
    let batch = Batch::parse(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert_eq!(batch.cases.len(), 3);
    assert!(batch.warmup.is_some());
    let args = Args::try_parse_from([
        "client",
        "groth16",
        "--elf",
        "e",
        "--profile",
        "p",
        "--out",
        "new",
        "--batch",
        "cases.json",
        "--setup-receipt",
        "r",
    ])
    .unwrap();
    assert!(args.validate(Some("cuda")).is_ok());
}

#[test]
fn repeated_witnesses_or_unknown_or_escaping_cases_reject() {
    let fixture = Fixture::new();
    let original = fixture.manifest();
    for field in [
        "case_id",
        "input_sha256",
        "public_inputs_sha256",
        "prepare_record_sha256",
    ] {
        let mut changed = original.clone();
        changed["cases"][1][field] = changed["cases"][0][field].clone();
        assert!(
            Batch::parse(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "{field}"
        );
    }
    let mut changed = original.clone();
    changed["cases"][0]["input"] = json!("../outside");
    assert!(Batch::parse(&serde_json::to_vec(&changed).unwrap()).is_err());
    let mut changed = original.clone();
    changed["cases"][0]["extra"] = json!(true);
    assert!(Batch::parse(&serde_json::to_vec(&changed).unwrap()).is_err());
    let mut changed = original;
    changed["cases"].as_array_mut().unwrap().pop();
    assert!(Batch::parse(&serde_json::to_vec(&changed).unwrap()).is_err());
}

#[test]
fn hashes_and_prepare_public_binding_cannot_drift() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest();
    let batch = Batch::parse(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(fixture.0.join("input-0"), b"changed synthetic input").unwrap();
    assert!(batch.cases[0].load(&fixture.0).is_err());
    let mut prepare: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.0.join("prepare-1.json")).unwrap()).unwrap();
    prepare["public_inputs_sha256"] = json!("0".repeat(64));
    let bytes = serde_json::to_vec(&prepare).unwrap();
    std::fs::write(fixture.0.join("prepare-1.json"), &bytes).unwrap();
    let mut case = batch.cases[1].clone();
    case.prepare_record_sha256 = sha256(&bytes);
    assert!(case.load(&fixture.0).is_err());
}
