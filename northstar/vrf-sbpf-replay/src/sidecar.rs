//! Native-only canonical sidecar conversion and independent binding validation.
use {
    crate::{
        AgaveReplayEngine, CONSUMER, COORDINATOR, feature_set, pinned_programs, reserved_keys,
        runtime_identities,
    },
    northstar_transaction_proof::{
        checkpoint::StateAccountValueV1, session_context_bytes_v1, vrf_candidate::*,
    },
    serde_json::{Value, json},
    solana_compute_budget::compute_budget::ComputeBudget,
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hash,
    std::{fs, path::Path},
};

type Result<T> = std::result::Result<T, String>;
fn proof<T>(value: std::result::Result<T, CandidateError>) -> Result<T> {
    value.map_err(|e| format!("candidate: {e:?}"))
}
fn need(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn string(value: &Value) -> Result<&str> {
    value.as_str().ok_or_else(|| "expected string".into())
}
fn number(value: &Value) -> Result<u64> {
    value.as_u64().ok_or_else(|| "expected exact u64".into())
}
fn array(value: &Value) -> Result<&Vec<Value>> {
    value.as_array().ok_or_else(|| "expected array".into())
}
fn key(value: &Value) -> Result<[u8; 32]> {
    string(value)?
        .parse::<Pubkey>()
        .map(|k| k.to_bytes())
        .map_err(|e| e.to_string())
}
fn bytes(value: &Value) -> Result<Vec<u8>> {
    serde_json::from_value(value.clone()).map_err(|e| e.to_string())
}
fn hex(value: &[u8]) -> String {
    value.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex<const N: usize>(value: &str) -> Result<[u8; N]> {
    need(
        value.len() == N.saturating_mul(2)
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "canonical hex required",
    )?;
    let mut output = [0; N];
    for (pair, byte) in value.as_bytes().as_chunks::<2>().0.iter().zip(&mut output) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).map_err(|e| e.to_string())?, 16)
            .map_err(|e| e.to_string())?;
    }
    Ok(output)
}
fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    need(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= limit,
        "bounded regular artifact required",
    )?;
    fs::read(path).map_err(|e| e.to_string())
}
fn json(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}
fn account(value: &Value) -> Result<CandidateAccountInputV1> {
    let raw = &value["account"];
    Ok(CandidateAccountInputV1 {
        key: key(&value["pubkey"])?,
        state: if raw.is_null() {
            None
        } else {
            Some(CandidateAccountStateV1 {
                lamports: number(&raw["lamports"])?,
                owner: key(&raw["owner"])?,
                executable: raw["executable"]
                    .as_bool()
                    .ok_or("expected executable bool")?,
                rent_epoch: number(&raw["rent_epoch"])?,
                data: proof(BoundedVec::new(bytes(&raw["data"])?))?,
            })
        },
    })
}
fn accounts(value: &Value) -> Result<Vec<CandidateAccountInputV1>> {
    let mut values = array(value)?
        .iter()
        .map(account)
        .collect::<Result<Vec<_>>>()?;
    values.sort_by_key(|a| a.key);
    need(
        values.windows(2).all(|p| p[0].key < p[1].key),
        "duplicate account key",
    )?;
    Ok(values)
}
fn state(value: &Value) -> Result<Vec<StateAccountValueV1>> {
    Ok(accounts(value)?
        .into_iter()
        .filter_map(|a| a.state.map(|s| candidate_state_value(a.key, &s)))
        .collect())
}
fn decimal(value: &Value) -> Result<u128> {
    let s = string(value)?;
    let number = s.parse::<u128>().map_err(|e| e.to_string())?;
    need(number.to_string() == s, "canonical decimal required")?;
    Ok(number)
}
fn session_context(session: &Value) -> Result<Vec<u8>> {
    need(
        session["schema"] == "northstar.vrf-candidate-portal-session.v1",
        "session schema",
    )?;
    let grid = u64::try_from(decimal(&session["grid_id"])?).map_err(|e| e.to_string())?;
    let expected = session_context_bytes_v1(
        key(&session["program_id"])?,
        key(&session["session"])?,
        grid,
        decimal(&session["nonce"])?,
        key(&session["validator"])?,
    );
    need(
        unhex::<149>(string(&session["session_context_preimage_hex"])?)?.as_slice() == expected,
        "session context source mismatch",
    )?;
    Ok(expected)
}

struct Prepared {
    manifest: Value,
    envelope: Vec<u8>,
    public: [u8; 256],
    transaction: Vec<u8>,
}

fn prepare(
    capture_bytes: &[u8],
    collector_bytes: &[u8],
    session: &Value,
    session_sha256: [u8; 32],
) -> Result<Prepared> {
    let capture = json(capture_bytes)?;
    let collector = json(collector_bytes)?;
    need(
        capture["domain"] == "northstar.er-vrf-candidate-capture.v1" && capture["version"] == 1,
        "capture schema",
    )?;
    need(
        capture["capture_rejected"].is_null()
            && capture["atomic_bank_operation_lock"] == true
            && capture["prestate_rechecked_before_commit"] == true,
        "authoritative atomic capture required",
    )?;
    need(
        string(&capture["collector_sha256"])? == hex(&hash(collector_bytes).to_bytes()),
        "collector digest",
    )?;
    let raw = &capture["input"];
    let observed = &capture["execution"];
    let ids = &collector["identities"];
    need(
        u128::from(number(&ids["grid_id"])?) == decimal(&session["grid_id"])?
            && ids["session_pda"].as_str() == session["session"].as_str(),
        "capture must originate from the matching actual Portal session/grid; relabeling is \
         unsupported",
    )?;
    need(
        &ids["actual_portal_session"] == session
            && ids["manager"] == session["validator"]
            && ids["portal_program_id"] == session["program_id"]
            && string(&ids["actual_portal_session_json_sha256"])? == hex(&session_sha256),
        "full actual Portal session/genesis/source identity differs",
    )?;
    need(
        ids["library_source_revision"] == "d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd"
            && ids["runtime_source_revision"] == "34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52",
        "pinned source base",
    )?;
    need(
        raw["zero_fee_policy"] == true
            && number(&raw["lamports_per_signature"])? == 0
            && number(&raw["blockhash_lamports_per_signature"])? == 0
            && number(&observed["fee"])? == 0
            && raw["compute_budget_override"] == false
            && number(&raw["epoch_total_stake"])? == 0,
        "unsupported runtime/fee override",
    )?;
    need(
        number(&raw["transaction_max_age"])? == 900 && number(&raw["max_processing_age"])? == 1800,
        "unsupported transaction age policy",
    )?;
    let budget = ComputeBudget::new_with_defaults(false);
    let environment = solana_syscalls::create_program_runtime_environment(
        &feature_set(),
        &budget.to_budget(),
        false,
        false,
    )
    .map_err(|e| e.to_string())?;
    need(
        string(&raw["vm_config_debug"])? == format!("{:?}", environment.get_config()),
        "captured/compiled VM configuration differs",
    )?;
    need(
        string(&observed["execution_cost_debug"])? == format!("{:?}", budget.to_cost()),
        "captured/compiled execution cost differs",
    )?;
    let registry: Vec<(u32, Vec<u8>)> =
        serde_json::from_value(raw["syscall_registry"].clone()).map_err(|e| e.to_string())?;
    let actual_registry: Vec<_> = environment
        .get_function_registry()
        .iter()
        .map(|(k, (name, _))| (k, name.to_vec()))
        .collect();
    need(
        registry == actual_registry,
        "captured/compiled syscall registry differs",
    )?;
    let observed_reserved: std::collections::HashSet<_> = array(&raw["reserved_account_keys"])?
        .iter()
        .map(|v| key(v).map(Pubkey::new_from_array))
        .collect::<Result<_>>()?;
    need(
        observed_reserved == reserved_keys(),
        "captured/compiled reserved keys differ",
    )?;
    let mut features = agave_feature_set::FeatureSet::default();
    features.activate(&agave_feature_set::curve25519_syscall_enabled::id(), 0);
    let mut active = array(&raw["active_features"])?
        .iter()
        .map(|v| {
            Ok(CandidateFeatureV1 {
                id: key(&v[0])?,
                activation_slot: number(&v[1])?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    active.sort_by_key(|v| v.id);
    need(
        active.len() == 1
            && active[0].id == agave_feature_set::curve25519_syscall_enabled::id().to_bytes()
            && active[0].activation_slot == 0,
        "fixed curve feature profile",
    )?;
    let mut inactive = array(&raw["inactive_features"])?
        .iter()
        .map(key)
        .collect::<Result<Vec<_>>>()?;
    inactive.sort();
    let mut actual_inactive: Vec<_> = features.inactive().iter().map(|p| p.to_bytes()).collect();
    actual_inactive.sort();
    need(
        inactive == actual_inactive,
        "incomplete inactive feature set",
    )?;
    let programs = pinned_programs();
    let (vm, syscalls, cost) = runtime_identities();
    let profile = CandidateProfileV1 {
        version: 1,
        library_revision: unhex(string(&ids["library_source_revision"])?)?,
        runtime_revision: unhex(string(&ids["runtime_source_revision"])?)?,
        agave_revision: unhex(string(&ids["runtime_source_revision"])?)?,
        program_identities_sha256: proof(candidate_programs_sha256(&programs))?,
        active_features: proof(BoundedVec::new(active))?,
        inactive_features: proof(BoundedVec::new(inactive))?,
        vm_config_sha256: vm,
        syscall_registry_sha256: syscalls,
        compute_budget_sha256: cost,
        lamports_per_signature: 0,
        checkpoint_steps: 1,
        max_compute_units: 1_400_000,
        heap_bytes: 32_768,
        max_processing_age: 900,
    };
    need(
        profile == crate::development_profile(),
        "captured/compiled fixed profile differs",
    )?;
    let profile_sha256 = proof(candidate_profile_sha256(&profile))?;
    let slot = number(&raw["slot"])?;
    let mut recent = array(&raw["recent_blockhashes"])?
        .iter()
        .map(|v| {
            Ok(CandidateRecentBlockhashV1 {
                blockhash: key(&v["blockhash"])?,
                age: number(&v["age"])?,
                lamports_per_signature: number(&v["lamports_per_signature"])?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    recent.sort_by_key(|v| v.blockhash);
    let transaction = bytes(&raw["signed_transaction"])?;
    need(
        hex(&hash(&transaction).to_bytes()) == string(&capture["signed_transaction_sha256"])?,
        "signed wire digest",
    )?;
    let input = CandidateExecutionInputV1 {
        session_context: proof(BoundedVec::new(session_context(session)?))?,
        er_slot: slot,
        step_index: 0,
        transaction_bytes: proof(BoundedVec::new(transaction.clone()))?,
        profile,
        runtime: CandidateRuntimeV1 {
            slot,
            observation_slot: slot,
            recent_blockhashes: proof(BoundedVec::new(recent))?,
            compute_unit_limit: number(&raw["compute_unit_limit"])?,
            heap_bytes: u32::try_from(number(&observed["heap_size"])?)
                .map_err(|e| e.to_string())?,
        },
        accounts: proof(BoundedVec::new(accounts(&raw["accounts"])?))?,
        programs: proof(BoundedVec::new(programs.to_vec()))?,
    };
    let policy = CandidatePolicyV1 {
        profile_sha256,
        programs,
    };
    let admitted = proof(validate_candidate_input(&input, &policy))?;
    let output = proof(AgaveReplayEngine.execute(&input))?;
    let status = if observed["status"] == json!({"Ok":null}) {
        CandidateExecutionStatusV1::Success
    } else {
        let error = &observed["status"]["Err"]["InstructionError"];
        let index = u16::try_from(number(&error[0])?).map_err(|e| e.to_string())?;
        let code = u32::try_from(number(&error[1]["Custom"])?).map_err(|e| e.to_string())?;
        need(
            observed["status"] == json!({"Err":{"InstructionError":[index,{"Custom":code}]}}),
            "unencoded captured status",
        )?;
        CandidateExecutionStatusV1::InstructionFailure {
            instruction_index: index,
            error_kind: 1,
            custom_error: code,
        }
    };
    let observed_output = CandidateExecutionOutputV1 {
        status,
        accounts: proof(BoundedVec::new(accounts(&capture["committed_accounts"])?))?,
        executed_units: number(&observed["executed_units"])?,
        loaded_accounts_data_size: number(&observed["loaded_accounts_data_size"])?,
        transaction_fee: number(&observed["fee"])?,
        prioritization_fee: 0,
        return_data: Some(CandidateReturnDataV1 {
            program_id: key(&observed["return_data_program_id"])?,
            data: proof(BoundedVec::new(bytes(&observed["return_data"])?))?,
        }),
        log_commitment: proof(candidate_log_commitment(
            &serde_json::from_value::<Vec<String>>(observed["logs"].clone())
                .map_err(|e| e.to_string())?,
        ))?,
    };
    need(
        output == observed_output,
        "actual interpreter output differs from complete authoritative capture",
    )?;
    let path = CandidateMerklePathV1 {
        leaf_index: 0,
        leaf_count: 1,
        siblings: BoundedVec::default(),
    };
    let checkpoint = proof(build_candidate_checkpoint(
        &input,
        &output,
        proof(BoundedVec::new(state(&capture["checkpoint_pre_accounts"])?))?,
        path.clone(),
        path,
    ))?;
    need(
        checkpoint.post_state.as_ref()
            == state(&capture["checkpoint_committed_accounts"])?.as_slice(),
        "complete checkpoint state differs",
    )?;
    let envelope = CandidateEnvelopeV1 {
        magic: CANDIDATE_MAGIC,
        version: CANDIDATE_VERSION,
        proof_kind: CANDIDATE_PROOF_KIND,
        proof_version: CANDIDATE_PROOF_VERSION,
        input,
        expected_output: output,
        checkpoint,
    };
    let public = proof(verify_candidate(&envelope, &policy, &mut AgaveReplayEngine))?;
    let fields = public.to_array();
    let mut public_bytes = [0; 256];
    for (field, out) in fields
        .iter()
        .zip(public_bytes.as_chunks_mut::<32>().0.iter_mut())
    {
        out.copy_from_slice(field);
    }
    let program_id = admitted.account_keys[usize::from(admitted.instructions[1].program_id_index)];
    let role = if program_id == CONSUMER.to_bytes() {
        "request"
    } else if program_id == COORDINATOR.to_bytes() {
        "fulfillment_callback"
    } else {
        return Err("unsupported role".into());
    };
    let manifest = json!({
        "schema":"northstar.vrf-candidate-portal-binding.v1","proof_kind":3,"proof_version":1,
        "program_id":session["program_id"],"session":session["session"],"grid_id":session["grid_id"],
        "nonce":session["nonce"],"validator":session["validator"],"session_context_preimage_hex":session["session_context_preimage_hex"],
        "er_slot":slot.to_string(),"step_index":0,"checkpoint_steps":1,"profile_sha256":hex(&profile_sha256),
        "public_inputs_hex":hex(&public_bytes),"state_roots":[hex(&fields[3]),hex(&fields[4])],
        "transaction_tree_root":hex(&fields[5]),"readonly_context_root":hex(&fields[6]),"settlement_effect_root":hex(&fields[7]),
        "captures":[{"capture_file":"capture.json","capture_sha256":hex(&hash(capture_bytes).to_bytes()),
            "transaction_file":"transaction.bin","transaction_sha256":hex(&hash(&transaction).to_bytes()),"role":role}],
        "collector_file":"collector.json","witness_file":"witness.borsh","public_inputs_file":"public-inputs.bin",
        "classification":"native-actual-ER-candidate-binding","proof_generated":false,"cross_layer_settlement":false,
    });
    Ok(Prepared {
        manifest,
        envelope: proof(encode_candidate(&envelope))?,
        public: public_bytes,
        transaction,
    })
}

pub fn produce(capture_path: &Path, session_path: &Path, output: &Path) -> Result<()> {
    let capture = read(capture_path, 16 * 1024 * 1024)?;
    let collector = read(
        &capture_path
            .parent()
            .ok_or("capture parent")?
            .join("collector.json"),
        1024 * 1024,
    )?;
    let session_bytes = read(session_path, 1024 * 1024)?;
    let session = json(&session_bytes)?;
    let prepared = prepare(
        &capture,
        &collector,
        &session,
        hash(&session_bytes).to_bytes(),
    )?;
    fs::create_dir(output).map_err(|e| e.to_string())?;
    for (name, bytes) in [
        ("capture.json", capture),
        ("collector.json", collector),
        ("transaction.bin", prepared.transaction),
        ("witness.borsh", prepared.envelope),
        ("public-inputs.bin", prepared.public.to_vec()),
        (
            "binding.json",
            serde_json::to_vec_pretty(&prepared.manifest).map_err(|e| e.to_string())?,
        ),
    ] {
        fs::write(output.join(name), bytes).map_err(|e| e.to_string())?;
    }
    println!(
        "{}",
        json!({"schema":"northstar.vrf-candidate-binding-producer.v1","execution_verified":true,
        "profile_sha256":prepared.manifest["profile_sha256"],"checkpoint_steps":1,"proof_generated":false})
    );
    Ok(())
}

pub fn validate(binding_path: &Path, session_path: &Path) -> Result<()> {
    let binding_bytes = read(binding_path, 1024 * 1024)?;
    let manifest = json(&binding_bytes)?;
    need(
        array(&manifest["captures"])?.len() == 1,
        "only reviewed one-step profile supported",
    )?;
    let root = binding_path.parent().ok_or("binding parent")?;
    let capture = read(&root.join("capture.json"), 16 * 1024 * 1024)?;
    let collector = read(&root.join("collector.json"), 1024 * 1024)?;
    let session_bytes = read(session_path, 1024 * 1024)?;
    let session = json(&session_bytes)?;
    let prepared = prepare(
        &capture,
        &collector,
        &session,
        hash(&session_bytes).to_bytes(),
    )?;
    need(
        manifest == prepared.manifest,
        "binding manifest differs from actual canonical replay",
    )?;
    need(
        read(&root.join("transaction.bin"), 1232)? == prepared.transaction,
        "wire artifact differs",
    )?;
    need(
        read(&root.join("witness.borsh"), 16 * 1024 * 1024)? == prepared.envelope,
        "witness artifact differs",
    )?;
    need(
        read(&root.join("public-inputs.bin"), 256)? == prepared.public,
        "public artifact differs",
    )?;
    println!(
        "{}",
        json!({"schema":"northstar.vrf-candidate-portal-binding-validation.v1",
        "binding_sha256":hex(&hash(&binding_bytes).to_bytes()),"public_inputs_sha256":hex(&hash(&prepared.public).to_bytes()),
        "capture_sha256":[hex(&hash(&capture).to_bytes())],"execution_verified":true})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires real matching VRF_CAPTURE_FILE and VRF_SESSION_FILE"]
    fn invariant_actual_session_full_source_binding() -> Result<()> {
        let capture_path = std::env::var("VRF_CAPTURE_FILE").map_err(|e| e.to_string())?;
        let session_path = std::env::var("VRF_SESSION_FILE").map_err(|e| e.to_string())?;
        let capture_path = Path::new(&capture_path);
        let capture = read(capture_path, 16 * 1024 * 1024)?;
        let collector = read(
            &capture_path
                .parent()
                .ok_or("capture parent")?
                .join("collector.json"),
            1024 * 1024,
        )?;
        let session_bytes = read(Path::new(&session_path), 1024 * 1024)?;
        let session = json(&session_bytes)?;
        let original = prepare(
            &capture,
            &collector,
            &session,
            hash(&session_bytes).to_bytes(),
        )?;
        assert_eq!(original.public.len(), 256);
        let mut changed = session.clone();
        changed["genesis_hash"] = json!("11111111111111111111111111111111");
        let encoded = serde_json::to_vec(&changed).map_err(|e| e.to_string())?;
        let error = prepare(&capture, &collector, &changed, hash(&encoded).to_bytes())
            .err()
            .ok_or("changed genesis accepted")?;
        assert!(error.contains("full actual Portal session/genesis/source identity"));
        let mut changed = session;
        let nonce = decimal(&changed["nonce"])?
            .checked_add(1)
            .ok_or("nonce bound")?;
        changed["nonce"] = json!(nonce.to_string());
        let context = session_context_bytes_v1(
            key(&changed["program_id"])?,
            key(&changed["session"])?,
            u64::try_from(decimal(&changed["grid_id"])?).map_err(|e| e.to_string())?,
            nonce,
            key(&changed["validator"])?,
        );
        changed["session_context_preimage_hex"] = json!(hex(&context));
        let encoded = serde_json::to_vec(&changed).map_err(|e| e.to_string())?;
        let error = prepare(&capture, &collector, &changed, hash(&encoded).to_bytes())
            .err()
            .ok_or("changed nonce accepted")?;
        assert!(error.contains("full actual Portal session/genesis/source identity"));
        Ok(())
    }
}
