use super::*;

#[test]
fn actual_session_source_codec_matches_node_identity_vector() {
    let raw = include_bytes!("../../fixtures/session-source-v1.json");
    let source: SessionSource = serde_json::from_slice(raw).unwrap();
    assert_eq!(source.source_bytes().unwrap(), raw);
    assert_eq!(
        hex::encode(source.context().unwrap()),
        source.session_context_preimage_hex
    );
    assert_eq!(
        sha256(raw),
        "3090785d6f77ac985378453971d9af883073a9ba30a3c73652c8a380002da12f"
    );
    assert_eq!(
        sha256(&source.context().unwrap()),
        "81598c3a6d9f5438bc7e5d2d5c460462d56bbed2d6692ff520a32e4b8f483c52"
    );
    let mut identity = b"northstar-local-portal-session-identity-v1".to_vec();
    identity.extend_from_slice(&(source.genesis_hash.len() as u32).to_le_bytes());
    identity.extend_from_slice(source.genesis_hash.as_bytes());
    identity.extend_from_slice(&source.context().unwrap());
    assert_eq!(
        sha256(&identity),
        "e4cd05cc01b8bbbf9dc10b8e9b327b708ba7a622300680ac04c3c92395947eba"
    );
}

#[test]
fn declared_labels_and_context_fields_cannot_replace_source_or_envelope_binding() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest();
    let batch = Batch::parse(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    let case = &batch.cases[0];
    let original = fixture.prepare(case);
    for pointer in [
        "/session_identity_sha256",
        "/session_source_sha256",
        "/session/session_context_preimage_hex",
        "/session/grid_id",
        "/session/nonce",
        "/session/validator",
        "/session/genesis_hash",
        "/portal_program/program_id",
        "/portal_program/elf_sha256",
    ] {
        let mut prepared = original.clone();
        *prepared.pointer_mut(pointer).unwrap() = json!("arbitrary-new-label");
        let changed = fixture.rewrite(case, &prepared);
        assert!(changed.load(&fixture.0).is_err(), "{pointer}");
    }
    for nonce in ["01", "340282366920938463463374607431768211456"] {
        let mut prepared = original.clone();
        prepared["session"]["nonce"] = json!(nonce);
        assert!(fixture.rewrite(case, &prepared).load(&fixture.0).is_err());
    }
    let mut prepared = original.clone();
    prepared["session"]["extra"] = json!(true);
    assert!(fixture.rewrite(case, &prepared).load(&fixture.0).is_err());
    let mut prepared = original.clone();
    prepared
        .as_object_mut()
        .unwrap()
        .remove("session_source_sha256");
    assert!(fixture.rewrite(case, &prepared).load(&fixture.0).is_err());
    let mut prepared = original;
    prepared["session"] = fixture.prepare(&batch.cases[1])["session"].clone();
    reseal_session(&mut prepared);
    let changed = fixture.rewrite(case, &prepared);
    let error = changed.load(&fixture.0).err().unwrap().to_string();
    assert!(error.contains("canonical envelope context"), "{error}");
}

fn reseal_session(prepared: &mut serde_json::Value) {
    let source: SessionSource = serde_json::from_value(prepared["session"].clone()).unwrap();
    prepared["session_source_sha256"] = json!(sha256(&source.source_bytes().unwrap()));
    let mut identity = b"northstar-local-portal-session-identity-v1".to_vec();
    identity.extend_from_slice(&(source.genesis_hash.len() as u32).to_le_bytes());
    identity.extend_from_slice(source.genesis_hash.as_bytes());
    identity.extend_from_slice(&source.context().unwrap());
    prepared["session_identity_sha256"] = json!(sha256(&identity));
}

#[test]
fn same_actual_context_with_new_genesis_label_still_cannot_count_twice() {
    let fixture = Fixture::new();
    let batch = Batch::parse(&serde_json::to_vec(&fixture.manifest()).unwrap()).unwrap();
    let first = batch.cases[0].load(&fixture.0).unwrap();
    let mut prepared = fixture.prepare(&batch.cases[0]);
    prepared["session"]["genesis_hash"] = json!(bs58::encode([99; 32]).into_string());
    reseal_session(&mut prepared);
    let changed = fixture.rewrite(&batch.cases[0], &prepared);
    let second = changed.load(&fixture.0).unwrap();
    assert_ne!(
        first.qualified_session.identity_sha256,
        second.qualified_session.identity_sha256
    );
    assert_ne!(
        first.qualified_session.source_sha256,
        second.qualified_session.source_sha256
    );
    let mut identities = BatchIdentities::default();
    identities.observe(&first).unwrap();
    assert!(identities.observe(&second).is_err());
}

#[test]
fn prepared_deployment_drift_and_repeated_context_are_rejected() {
    let fixture = Fixture::new();
    let batch = Batch::parse(&serde_json::to_vec(&fixture.manifest()).unwrap()).unwrap();
    let first = batch.cases[0].load(&fixture.0).unwrap();
    let mut identities = BatchIdentities::default();
    identities.observe(&first).unwrap();
    assert!(identities.observe(&first).is_err());
    let mut prepared = fixture.prepare(&batch.cases[1]);
    prepared["portal_program"]["elf_sha256"] = json!(sha256(b"different synthetic deployment"));
    let case = fixture.rewrite(&batch.cases[1], &prepared);
    let second = case.load(&fixture.0).unwrap();
    assert!(identities.observe(&second).is_err());
}
