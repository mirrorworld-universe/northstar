mod support;

use {
    northstar_vrf_oracle::{Error, Mode, OracleKey, VrfResult, verify},
    support::{Vector, array, key, metadata, other_key, request},
    zeroize::Zeroizing,
};

#[test]
fn published_c2sp_creation_matches_exact_proof_and_output() {
    let vector = Vector::load();
    let signer = key();
    assert_eq!(signer.public_key(), array(&vector.public_key));
    assert_eq!(
        signer
            .prove_bytes(&hex::decode(&vector.alpha).unwrap())
            .unwrap(),
        vector.result()
    );
}

#[test]
fn published_c2sp_verification_needs_no_secret_or_local_generation() {
    let vector = Vector::load();
    let output = vector
        .result()
        .verify_bytes(
            array(&vector.public_key),
            &hex::decode(&vector.alpha).unwrap(),
        )
        .unwrap();
    assert_eq!(hex::encode(output), vector.output);
}

#[test]
fn public_vector_rejects_wrong_input_key_proof_and_output() {
    let vector = Vector::load();
    let public_key = array(&vector.public_key);
    let alpha = hex::decode(&vector.alpha).unwrap();
    assert!(
        vector
            .result()
            .verify_bytes(public_key, b"different input")
            .is_err()
    );
    assert!(
        vector
            .result()
            .verify_bytes(other_key().public_key(), &alpha)
            .is_err()
    );
    assert!(vector.result().verify_bytes([0; 32], &alpha).is_err());
    let mut changed = vector.result();
    changed.proof.replace_range(0..2, "ff");
    assert!(changed.verify_bytes(public_key, &alpha).is_err());
    changed = vector.result();
    changed.output.replace_range(0..2, "00");
    assert!(changed.verify_bytes(public_key, &alpha).is_err());
    changed = vector.result();
    changed.proof.replace_range(96..160, &"ff".repeat(32));
    assert!(changed.verify_bytes(public_key, &alpha).is_err());
    changed = vector.result();
    changed.proof.pop();
    assert!(changed.verify_bytes(public_key, &alpha).is_err());
    changed = vector.result();
    changed.output = changed.output.to_uppercase();
    assert!(changed.verify_bytes(public_key, &alpha).is_err());
}

#[test]
fn invalid_secret_scalars_are_rejected() {
    assert!(matches!(
        OracleKey::from_scalar(Zeroizing::new([0; 32])),
        Err(Error::Secret)
    ));
    assert!(matches!(
        OracleKey::from_scalar(Zeroizing::new([255; 32])),
        Err(Error::Secret)
    ));
}

#[test]
fn every_request_field_and_profile_is_bound() {
    let original = request();
    let profile = metadata();
    let proof = key().prove(&profile, &original).unwrap();
    verify(&profile, &original, &proof).unwrap();
    let mut changes = Vec::new();
    let mut changed = original.clone();
    changed.id[0] ^= 1;
    changes.push(changed);
    let mut changed = original.clone();
    changed.public_key = other_key().public_key();
    changes.push(changed);
    let mut changed = original.clone();
    changed.input.push(0);
    changes.push(changed);
    let mut changed = original.clone();
    changed.context_digest[0] ^= 1;
    changes.push(changed);
    let mut changed = original.clone();
    changed.not_before += 1;
    changes.push(changed);
    let mut changed = original.clone();
    changed.expires_at += 1;
    changes.push(changed);
    for changed in changes {
        assert_ne!(
            original.binding(&profile).unwrap(),
            changed.binding(&profile).unwrap()
        );
        assert!(verify(&profile, &changed, &proof).is_err());
    }
    let mut changed_profile = profile.clone();
    changed_profile.chain[0] ^= 1;
    assert!(verify(&changed_profile, &original, &proof).is_err());
    let mut rebound = original;
    rebound.metadata_digest = changed_profile.digest().unwrap();
    assert!(verify(&changed_profile, &rebound, &proof).is_err());
}

#[test]
fn offline_encoding_matches_fixed_public_fixture() {
    let profile = metadata();
    let request = request();
    assert_eq!(
        hex::encode(profile.digest().unwrap()),
        "f8a47d0a3ce85e2dc73dccd3ae999a78c8f3cc316029cd91fe07a1b6336f4187"
    );
    assert_eq!(request.alpha(&profile).unwrap().len(), 219);
    assert_eq!(
        hex::encode(request.binding(&profile).unwrap()),
        "7f8527f8fa0a8884d1523d3aabe454472c0d4496c5455b492563e958a87ab25c"
    );
}

#[test]
fn wrong_signer_does_not_produce_a_request_result() {
    assert!(matches!(
        other_key().prove(&metadata(), &request()),
        Err(Error::Key)
    ));
}

#[test]
fn suite_and_metadata_fail_closed() {
    let profile = metadata();
    let mut changed = profile.clone();
    changed.mode = Mode::LiveSolana;
    assert!(matches!(changed.validate(), Err(Error::Metadata)));
    let mut changed = profile.clone();
    changed.suite = "RFC9381-EDWARDS25519-SHA512-ELL2".into();
    assert!(changed.validate().is_err());
    let mut changed = profile.clone();
    changed.application_fixture_digest = [0; 32];
    assert!(changed.validate().is_err());
    let mut changed = profile;
    changed.version = 2;
    assert!(changed.validate().is_err());
    assert!(serde_json::from_str::<northstar_vrf_oracle::Metadata>("{}").is_err());
}

#[test]
fn request_validation_bounds_and_encodings() {
    let profile = metadata();
    let original = request();
    let mut cases = Vec::new();
    let mut changed = original.clone();
    changed.input.clear();
    cases.push(changed);
    let mut changed = original.clone();
    changed.input = vec![0; 4097];
    cases.push(changed);
    let mut changed = original.clone();
    changed.expires_at = changed.not_before;
    cases.push(changed);
    let mut changed = original.clone();
    changed.expires_at = u64::MAX;
    cases.push(changed);
    let mut changed = original.clone();
    changed.id = [0; 32];
    cases.push(changed);
    let mut changed = original.clone();
    changed.public_key = [0; 32];
    cases.push(changed);
    for changed in cases {
        assert!(changed.validate(&profile).is_err());
    }
    let invalid = VrfResult {
        proof: "not hex".into(),
        output: "not hex".into(),
    };
    assert!(verify(&profile, &original, &invalid).is_err());
}
