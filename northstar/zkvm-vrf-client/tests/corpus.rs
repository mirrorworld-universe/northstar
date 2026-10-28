//! Synthetic payload/parser/observer tests, not guest execution or proof evidence.
use {
    clap::Parser,
    northstar_zk_types::FrBytes,
    northstar_zkvm_vrf_client::{
        corpus::{observe, Corpus, Observation},
        sha256, Args,
    },
    serde_json::{json, Value},
    std::{fs, path::PathBuf},
};

fn golden() -> Value {
    serde_json::from_str(include_str!("../fixtures/controls-manifest-v1.json")).unwrap()
}
fn parse(value: &Value) -> anyhow::Result<Corpus> {
    Corpus::parse(
        &serde_json::to_vec(value).unwrap(),
        golden()["policy_sha256"].as_str().unwrap(),
    )
}

struct Fixture {
    root: PathBuf,
    manifest: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "synthetic-corpus-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let mut manifest = golden();
        for (index, positive) in manifest["positive_cases"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            let input = vec![1, index as u8];
            let path = root.join(positive["file"].as_str().unwrap());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, &input).unwrap();
            positive["sha256"] = json!(sha256(&input));
            let mut public = [0u8; 256];
            public[..32].copy_from_slice(FrBytes::er_step_domain_v1(3, 1).as_bytes());
            for i in 1..8 {
                public[i * 32 + 31] = (index + 1) as u8;
            }
            fs::write(
                root.join(positive["public_inputs_file"].as_str().unwrap()),
                public,
            )
            .unwrap();
            positive["public_inputs_sha256"] = json!(sha256(&public));
        }
        let positives = manifest["positive_cases"].as_array().unwrap().clone();
        for (index, control) in manifest["controls"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            let input = vec![2, index as u8];
            let path = root.join(control["file"].as_str().unwrap());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, &input).unwrap();
            control["sha256"] = json!(sha256(&input));
            control["bytes"] = json!(input.len());
            control["basis_sha256"] = positives
                .iter()
                .find(|p| p["id"] == control["basis"])
                .unwrap()["sha256"]
                .clone();
        }
        let wire = b"synthetic wire, not signed";
        let source = &mut manifest["unsupported_signed_transaction"];
        let path = root.join(source["file"].as_str().unwrap());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, wire).unwrap();
        source["sha256"] = json!(sha256(wire));
        Self { root, manifest }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn actual_manifest_schema_and_order_with_explicit_selection() {
    let corpus = parse(&golden()).unwrap();
    let cases = corpus.cases();
    assert_eq!(cases.len(), 17);
    assert_eq!(cases[0].id, "request");
    assert_eq!(cases[1].id, "fulfill-zero");
    assert_eq!(cases[16].id, "actual-signed-setup");
    assert!(cases[..2].iter().all(|c| !c.expected_rejection));
    assert!(cases[2..].iter().all(|c| c.expected_rejection));
    assert!(corpus.selection(&[]).unwrap().iter().all(|v| *v));
    let selection = corpus
        .selection(&["actual-signed-setup".into(), "fulfill-zero".into()])
        .unwrap();
    assert_eq!(selection.iter().filter(|v| **v).count(), 2);
    assert!(selection[1] && selection[16]);
    for ids in [
        vec!["missing".into()],
        vec!["request".into(), "request".into()],
    ] {
        assert!(corpus.selection(&ids).is_err());
    }
}

#[test]
fn full_payload_preflight_hashes_public_basis_and_source_wire() {
    let fixture = Fixture::new();
    let corpus = parse(&fixture.manifest).unwrap();
    corpus.preflight(&fixture.root).unwrap();
    for case in corpus.cases() {
        case.load(&fixture.root).unwrap();
    }
    for pointer in [
        "/controls/0/basis",
        "/controls/0/basis_sha256",
        "/positive_cases/0/sha256",
        "/positive_cases/0/public_inputs_sha256",
        "/generator_source_sha256",
        "/policy_sha256",
    ] {
        let mut changed = fixture.manifest.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("unreviewed");
        assert!(parse(&changed).is_err(), "{pointer}");
    }
    for key in ["positive_cases", "controls"] {
        let file = fixture.manifest[key][0]["file"].as_str().unwrap();
        let original = fs::read(fixture.root.join(file)).unwrap();
        fs::write(fixture.root.join(file), b"changed").unwrap();
        assert!(corpus.preflight(&fixture.root).is_err());
        fs::write(fixture.root.join(file), original).unwrap();
    }
    fs::write(
        fixture
            .root
            .join("sources/unsupported-setup.transaction.bin"),
        b"changed",
    )
    .unwrap();
    assert!(corpus.preflight(&fixture.root).is_err());
}

#[test]
fn duplicate_ids_paths_bounds_and_expectation_drift_fail_closed() {
    let original = golden();
    for pointer in [
        "/schema",
        "/proof_kind",
        "/domain_hex",
        "/controls/0/expected_guest_exit",
        "/controls/0/expected_public_output_bytes",
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("incorrect");
        assert!(parse(&changed).is_err(), "{pointer}");
    }
    for file in ["../outside", "/absolute", "./relative", ""] {
        let mut changed = original.clone();
        changed["controls"][0]["file"] = json!(file);
        assert!(parse(&changed).is_err());
    }
    let mut changed = original.clone();
    changed["controls"][0]["id"] = changed["positive_cases"][0]["id"].clone();
    assert!(parse(&changed).is_err());
    let mut changed = original.clone();
    changed["controls"][0]["file"] = changed["controls"][1]["file"].clone();
    assert!(parse(&changed).is_err());
    let mut changed = original.clone();
    changed["controls"][0]["bytes"] = json!(0);
    assert!(parse(&changed).is_err());
    let mut changed = original.clone();
    changed["controls"].as_array_mut().unwrap().pop();
    assert!(parse(&changed).is_err());
    assert!(Corpus::parse(
        &vec![b' '; 1024 * 1024 + 1],
        original["policy_sha256"].as_str().unwrap()
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn symlink_escape_and_public_mutation_are_not_execution_controls() {
    let fixture = Fixture::new();
    let corpus = parse(&fixture.manifest).unwrap();
    let case = &corpus.cases()[0];
    let input = fixture.root.join(&case.file);
    fs::remove_file(&input).unwrap();
    std::os::unix::fs::symlink("/dev/null", &input).unwrap();
    assert!(corpus.preflight(&fixture.root).is_err());
    fs::remove_file(&input).unwrap();
    fs::write(&input, [1, 0]).unwrap();
    fs::write(fixture.root.join(&case.public_inputs_file), [0u8; 256]).unwrap();
    assert!(case.load(&fixture.root).is_err());
}

#[test]
fn only_returned_guest_report_can_satisfy_strict_observer() {
    let fixture = Fixture::new();
    let (_, expected) = parse(&fixture.manifest).unwrap().cases()[0]
        .load(&fixture.root)
        .unwrap();
    observe(
        Observation::Returned {
            exit_code: 0,
            public: &expected,
        },
        &expected,
        false,
    )
    .unwrap();
    observe(
        Observation::Returned {
            exit_code: u64::MAX,
            public: &[],
        },
        &expected,
        true,
    )
    .unwrap();
    for observation in [
        Observation::HostError,
        Observation::Panic,
        Observation::Timeout,
        Observation::Missing,
        Observation::Returned {
            exit_code: 0,
            public: &[],
        },
        Observation::Returned {
            exit_code: 1,
            public: &expected,
        },
    ] {
        assert!(observe(observation, &expected, true).is_err());
    }
    assert!(observe(
        Observation::Returned {
            exit_code: 1,
            public: &[]
        },
        &expected,
        false
    )
    .is_err());
}

#[test]
fn corpus_cli_is_cpu_only_and_has_no_proving_or_single_input_shortcut() {
    let base = [
        "client",
        "execute-corpus",
        "--elf",
        "e",
        "--profile",
        "p",
        "--out",
        "new",
        "--corpus",
        "manifest.json",
        "--setup-receipt",
        "receipt",
    ];
    let args = Args::try_parse_from(base).unwrap();
    args.validate(Some("cpu")).unwrap();
    for backend in ["cuda", "network", "mock", "light"] {
        assert!(args.validate(Some(backend)).is_err());
    }
    for extra in [
        vec!["--input", "input"],
        vec!["--public", "public"],
        vec!["--batch", "batch"],
        vec!["--expect-rejection"],
    ] {
        let args = Args::try_parse_from(base.into_iter().chain(extra)).unwrap();
        assert!(args.validate(Some("cpu")).is_err());
    }
}
