use {
    clap::Parser,
    northstar_zk_types::{FrBytes, BN254_FR_MODULUS_BE},
    northstar_zkvm_vrf_client::*,
    serde_json::json,
};

fn public() -> [u8; 256] {
    let mut result = [0; 256];
    result[..32].copy_from_slice(FrBytes::er_step_domain_v1(3, 1).as_bytes());
    for index in 1..8 {
        result[index * 32 + 31] = index as u8;
    }
    result
}

fn synthetic_elf_header() -> [u8; 64] {
    let mut elf = [0; 64];
    elf[..7].copy_from_slice(&[127, b'E', b'L', b'F', 2, 1, 1]);
    elf[16..20].copy_from_slice(&[2, 0, 243, 0]);
    elf[20] = 1;
    elf[52] = 64;
    elf
}

#[test]
fn canonical_candidate_public_fields_and_legacy_separation() {
    let valid = public();
    assert_eq!(public_values(&valid).unwrap(), valid);
    for kind in [0, 1, 2, 4] {
        let mut changed = valid;
        changed[30] = kind;
        assert!(public_values(&changed).is_err());
    }
    let mut changed = valid;
    changed[31] = 2;
    assert!(public_values(&changed).is_err());
    for index in [1, 3, 4, 5, 6, 7] {
        let mut changed = valid;
        changed[index * 32..index * 32 + 32].fill(0);
        assert!(public_values(&changed).is_err());
    }
    for index in 0..8 {
        let mut changed = valid;
        changed[index * 32..index * 32 + 32].copy_from_slice(&BN254_FR_MODULUS_BE);
        assert!(public_values(&changed).is_err());
    }
    assert!(public_values(&valid[..255]).is_err());
}

#[test]
fn returned_report_alone_does_not_imply_acceptance() {
    let expected = public();
    assert!(execution_result(0, &expected, &expected, false).is_ok());
    assert!(execution_result(1, &expected, &expected, false).is_err());
    assert!(execution_result(1, &[], &expected, false).is_err());
    assert!(execution_result(0, &[], &expected, true).is_err());
    assert!(execution_result(1, &expected, &expected, true).is_err());
    assert!(execution_result(1, &[], &expected, true).is_ok());
    assert!(execution_result(1u64 << 32, &[], &expected, false).is_err());
    assert!(execution_result(1u64 << 32, &[], &expected, true).is_ok());
    assert!(execution_result(u64::MAX, &expected, &expected, true).is_err());
    let mut changed = expected;
    changed[255] ^= 1;
    assert!(execution_result(0, &changed, &expected, false).is_err());
}

#[test]
fn canonical_elf_profile_and_qualification_rejection() {
    let synthetic = synthetic_elf_header();
    let elf = synthetic.as_slice();
    let mut value = json!({"proof_kind":3,"proof_version":1,"public_input_fields":FIELDS,
        "production_acceptance":false,"elf_sha256":sha256(elf),"guest_status":"authenticated-envelope","policy_profile_sha256":sha256(b"synthetic policy")});
    assert!(profile(&serde_json::to_vec(&value).unwrap(), elf).is_ok());
    value["guest_status"] = json!("qualification-only");
    assert!(profile(&serde_json::to_vec(&value).unwrap(), elf).is_err());
    value["guest_status"] = json!("authenticated-envelope");
    value["elf_sha256"] = json!("wrong");
    assert!(profile(&serde_json::to_vec(&value).unwrap(), elf).is_err());
}

#[test]
fn unsupported_isa_flags_reject_before_sdk_setup() {
    let valid = synthetic_elf_header();
    elf_header(&valid).unwrap();
    for flag in [1, 2, 4, 8, 16] {
        let mut changed = valid;
        changed[48] = flag;
        assert!(elf_header(&changed).is_err());
    }
    let mut changed = valid;
    changed[4] = 1;
    assert!(elf_header(&changed).is_err());
    changed = valid;
    changed[18] = 62;
    assert!(elf_header(&changed).is_err());
    assert!(elf_header(&valid[..63]).is_err());
}

#[test]
fn network_mock_and_unreviewed_backends_never_initialize() {
    let key = Args::try_parse_from([
        "client",
        "key",
        "--elf",
        "x",
        "--profile",
        "p",
        "--out",
        "new",
    ])
    .unwrap();
    assert!(key.validate(None).is_ok());
    for backend in ["network", "hosted", "mock", "light", "cuda", "other"] {
        assert!(key.validate(Some(backend)).is_err());
    }
    let proof = Args::try_parse_from([
        "client",
        "groth16",
        "--elf",
        "x",
        "--profile",
        "p",
        "--out",
        "new",
        "--input",
        "i",
        "--public",
        "v",
        "--setup-receipt",
        "s",
        "--samples",
        "3",
    ])
    .unwrap();
    assert!(proof.validate(Some("cuda")).is_ok());
    for backend in ["cpu", "network", "mock", "hosted"] {
        assert!(proof.validate(Some(backend)).is_err());
    }
}

#[test]
fn exact_receipt_binding_and_nonlegacy_derived_key() {
    let receipt = SetupReceipt {
        schema: "northstar-sp1-setup-v1".into(),
        sp1_version: "6.8.0".into(),
        elf_sha256: "elf".into(),
        profile_sha256: "profile".into(),
        policy_profile_sha256: sha256(b"synthetic policy"),
        program_vkey_hash: format!("0x{:064x}", 1),
        verifying_key_sha256: "vk".into(),
        production_acceptance: false,
    };
    assert!(receipt.check(&receipt).is_ok());
    let mut changed = receipt.clone();
    changed.program_vkey_hash = format!("0x{:064x}", 2);
    assert!(changed.check(&receipt).is_err());
    changed = receipt.clone();
    changed.profile_sha256 = "other".into();
    assert!(changed.check(&receipt).is_err());
    changed = receipt.clone();
    changed.verifying_key_sha256 = "other".into();
    assert!(changed.check(&receipt).is_err());
    for key in [
        LEGACY_KEY.to_string(),
        format!("0x{:064x}", 0),
        format!("0x{}", hex::encode(BN254_FR_MODULUS_BE)),
    ] {
        assert!(program_key(&key).is_err());
    }
}

#[test]
fn internal_dependency_versions_are_bound_not_only_top_level_sdk() {
    check_lock(
        "name = \"sp1-sdk\"\nversion = \"6.8.0\"\nname = \"slop-algebra\"\nversion = \"6.8.0\"",
    )
    .unwrap();
    assert!(check_lock(
        "name = \"sp1-sdk\"\nversion = \"6.8.0\"\nname = \"sp1-primitives\"\nversion = \"6.10.0\""
    )
    .is_err());
    assert!(check_lock("no SDK packages").is_err());
    check_lock(include_str!("../Cargo.lock")).unwrap();
}

#[test]
fn fresh_artifacts_cannot_overwrite_prior_evidence() {
    let root = std::env::temp_dir().join(format!(
        "vrf-client-guard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("artifact");
    new_file(&file, b"original").unwrap();
    assert!(new_file(&file, b"different").is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"original");
    assert!(read_bounded(&file, 3).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
