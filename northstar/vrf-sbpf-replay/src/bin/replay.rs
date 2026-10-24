use {
    northstar_transaction_proof::vrf_candidate::{
        CandidateExecutionInputV1, CandidatePolicyV1, CandidateReplayEngine, MAX_ENVELOPE_BYTES,
        candidate_profile_sha256, decode_candidate, verify_candidate,
    },
    northstar_vrf_sbpf_replay::{
        AgaveReplayEngine, development_profile, pinned_programs, runtime_identities,
    },
    serde_json::json,
    std::{env, fs, process::ExitCode},
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("expected 64 lowercase hexadecimal characters".into());
    }
    let mut result = [0; 32];
    for (pair, byte) in value.as_bytes().as_chunks::<2>().0.iter().zip(&mut result) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).map_err(|_| "invalid digest")?, 16)
            .map_err(|_| "invalid digest")?;
    }
    Ok(result)
}
fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.as_slice() == ["identities"] {
        let (vm, syscalls, budget) = runtime_identities();
        let profile = development_profile();
        let profile_sha256 =
            candidate_profile_sha256(&profile).map_err(|e| format!("profile: {e:?}"))?;
        let programs: Vec<_> = pinned_programs()
            .into_iter()
            .map(|p| {
                json!({
                    "program_id": hex(&p.program_id), "programdata_id": hex(&p.programdata_id),
                    "loader_id": hex(&p.loader_id), "sbpf_version":p.sbpf_version,
                    "elf_sha256":hex(&p.elf_sha256),
                })
            })
            .collect();
        println!(
            "{}",
            json!({"classification":"compiled-engine-identities", "proof_kind":3, "proof_version":1,
            "vm_config_sha256":hex(&vm), "syscall_registry_sha256":hex(&syscalls),
            "compute_budget_sha256":hex(&budget), "programs":programs,
            "profile_sha256":hex(&profile_sha256),"checkpoint_steps":profile.checkpoint_steps,
            "transaction_max_age":profile.max_processing_age,"profile_revision_scope":"pinned source base; guest key pins candidate changes"})
        );
        return Ok(());
    }
    if args.len() != 3 || !matches!(args[0].as_str(), "execute-input" | "verify-envelope") {
        return Err(
            "usage: vrf-sbpf-replay identities | execute-input INPUT_BORSH OUTPUT_BORSH | \
             verify-envelope ENVELOPE_BORSH EXPECTED_PROFILE_SHA256"
                .into(),
        );
    }
    let metadata = fs::metadata(&args[1]).map_err(|e| e.to_string())?;
    if metadata.len() > MAX_ENVELOPE_BYTES as u64 {
        return Err("input bound".into());
    }
    let bytes = fs::read(&args[1]).map_err(|e| e.to_string())?;
    if args[0] == "execute-input" {
        let input: CandidateExecutionInputV1 =
            borsh::from_slice(&bytes).map_err(|e| e.to_string())?;
        if borsh::to_vec(&input).map_err(|e| e.to_string())? != bytes {
            return Err("noncanonical input".into());
        }
        let output = AgaveReplayEngine
            .execute(&input)
            .map_err(|e| format!("actual replay: {e:?}"))?;
        fs::write(&args[2], borsh::to_vec(&output).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        println!(
            "{}",
            json!({"classification":"native-execution-diagnostic", "proof_generated":false,
            "status":format!("{:?}",output.status), "executed_units":output.executed_units,
            "loaded_accounts_data_size":output.loaded_accounts_data_size,
            "log_commitment":hex(&output.log_commitment)})
        );
    } else {
        let envelope = decode_candidate(&bytes).map_err(|e| format!("decode: {e:?}"))?;
        let policy = CandidatePolicyV1 {
            profile_sha256: digest(&args[2])?,
            programs: pinned_programs(),
        };
        let public = verify_candidate(&envelope, &policy, &mut AgaveReplayEngine)
            .map_err(|e| format!("verify: {e:?}"))?;
        println!(
            "{}",
            json!({"classification":"native-bound-candidate", "proof_generated":false,
            "public_inputs": public.to_array().map(|field| hex(&field))})
        );
    }
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
