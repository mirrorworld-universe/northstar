use {
    anyhow::{bail, ensure, Context, Result},
    bincode::Options,
    clap::Parser,
    futures_util::FutureExt,
    northstar_zkvm_vrf_client::{self as guards, Args, Batch, Command, SetupReceipt},
    serde_json::{json, Value},
    sp1_sdk::{
        env::{EnvProver, EnvProvingKey},
        Elf, HashableKey, ProveRequest, Prover, ProverClient, ProvingKey, SP1Proof,
        SP1ProofWithPublicValues, SP1Stdin,
    },
    std::{
        env, fs,
        path::Path,
        time::{Instant, SystemTime, UNIX_EPOCH},
    },
};

fn json_file(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    guards::new_file(path, &serde_json::to_vec_pretty(value)?)
}
fn source_hash() -> String {
    guards::sha256(
        &[
            include_bytes!("../Cargo.toml").as_slice(),
            include_bytes!("../Cargo.lock").as_slice(),
            include_bytes!("lib.rs").as_slice(),
            include_bytes!("main.rs").as_slice(),
            include_bytes!("session.rs").as_slice(),
            include_bytes!("corpus.rs").as_slice(),
            include_bytes!("../../zk-types/Cargo.toml").as_slice(),
            include_bytes!("../../zk-types/src/lib.rs").as_slice(),
            include_bytes!("../../zk-types/src/trace.rs").as_slice(),
        ]
        .concat(),
    )
}
fn checkpoint(out: &Path, state: &Value) -> Result<()> {
    let pending = out.join("measurements.pending.json");
    fs::write(&pending, serde_json::to_vec_pretty(state)?)?;
    fs::rename(pending, out.join("measurements.json"))?;
    Ok(())
}
fn active(args: &Args, state: &mut Value, phase: &str, context: &Value) -> Result<()> {
    state["active_phase"] = json!({"phase":phase,"context":context,"started_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()});
    checkpoint(&args.out, state)
}
fn phase(state: &mut Value, mut value: Value, context: &Value) {
    for (key, item) in context.as_object().unwrap() {
        value[key] = item.clone();
    }
    state["phases"].as_array_mut().unwrap().push(value);
    state["active_phase"] = Value::Null;
}

struct InputRun<'a> {
    artifacts: &'a Path,
    input: Vec<u8>,
    expected: [u8; 256],
    rejection: bool,
    context: Value,
}

async fn input_run(
    args: &Args,
    client: &EnvProver,
    key: &EnvProvingKey,
    elf: Elf,
    state: &mut Value,
    input: InputRun<'_>,
) -> Result<()> {
    guards::new_file(&input.artifacts.join("input.bin"), &input.input)?;
    let mut stdin = SP1Stdin::new();
    stdin.write_vec(input.input);
    active(args, state, "execute", &input.context)?;
    let started = Instant::now();
    let (public, report) = client
        .execute(elf, stdin.clone())
        .cycle_limit(args.cycle_limit)
        .await?;
    let execute_ms = started.elapsed().as_millis();
    guards::new_file(
        &input.artifacts.join("execution-public-values.bin"),
        public.as_slice(),
    )?;
    phase(
        state,
        json!({"phase":"execute","wall_ms":execute_ms,"exit_code":report.exit_code,
        "cycles":report.total_instruction_count(),"syscalls":report.total_syscall_count(),"cycle_tracker":&report.cycle_tracker,
        "public_output_bytes":public.as_slice().len(),"public_output_sha256":guards::sha256(public.as_slice()),"cpu_only_control":true,"strict_observer_passed":false}),
        &input.context,
    );
    checkpoint(&args.out, state)?;
    guards::corpus::observe(
        guards::corpus::Observation::Returned {
            exit_code: report.exit_code,
            public: public.as_slice(),
        },
        &input.expected,
        input.rejection,
    )?;
    state["phases"].as_array_mut().unwrap().last_mut().unwrap()["strict_observer_passed"] =
        json!(true);
    checkpoint(&args.out, state)?;
    if matches!(args.command, Command::Execute | Command::ExecuteCorpus) {
        return Ok(());
    }
    for sample in 0..args.samples {
        let attempt = state["proof_attempts_started"].as_u64().unwrap();
        state["proof_attempts_started"] = json!(attempt + 1);
        active(args, state, "groth16", &input.context)?;
        let started = Instant::now();
        let proof = client.prove(key, stdin.clone()).groth16().await?;
        let prove_ms = started.elapsed().as_millis();
        let sdk = bincode::serialize(&proof)?;
        let unverified = input
            .artifacts
            .join(format!("sample-{sample}-unverified-sdk-proof.bin"));
        guards::new_file(&unverified, &sdk)?;
        phase(
            state,
            json!({"phase":"groth16-generated-unverified","attempt_sequence":attempt,
            "prove_and_wrap_ms":prove_ms,"sdk_proof_sha256":guards::sha256(&sdk),
            "unverified_sdk_proof_path":unverified.strip_prefix(&args.out)?}),
            &input.context,
        );
        active(args, state, "compare-public-values", &input.context)?;
        ensure!(
            proof.public_values.as_slice() == input.expected,
            "wrapped proof public values mismatch"
        );
        active(args, state, "sdk_verify", &input.context)?;
        let started = Instant::now();
        client.verify(&proof, key.verifying_key(), None)?;
        let verify_ms = started.elapsed().as_millis();
        let bytes = wrapped_bytes(&proof)?;
        let sdk_path = input
            .artifacts
            .join(format!("sample-{sample}-northstar-sp1-groth16.bin"));
        let proof_path = input
            .artifacts
            .join(format!("sample-{sample}-northstar-sp1-groth16-onchain.bin"));
        let public_path = input
            .artifacts
            .join(format!("sample-{sample}-northstar-sp1-public-inputs.bin"));
        guards::new_file(&sdk_path, &sdk)?;
        guards::new_file(&proof_path, &bytes)?;
        guards::new_file(&public_path, &input.expected)?;
        phase(
            state,
            json!({"phase":"groth16","sample":sample,"attempt_sequence":attempt,"process_cold":attempt==0,
            "prove_and_wrap_ms":prove_ms,"verify_ms":verify_ms,"within_120s_prove_wrap":prove_ms<=120_000,
            "sdk_verified":true,"onchain_proof_bytes":bytes.len(),"proof_sp1_version":proof.sp1_version,
            "onchain_proof_path":proof_path.strip_prefix(&args.out)?,"public_inputs_path":public_path.strip_prefix(&args.out)?,
            "sdk_proof_path":sdk_path.strip_prefix(&args.out)?,"proof_sha256":guards::sha256(&bytes),
            "public_inputs_sha256":guards::sha256(&input.expected),"sdk_proof_sha256":guards::sha256(&sdk)}),
            &input.context,
        );
        checkpoint(&args.out, state)?;
    }
    Ok(())
}

async fn batch_run(
    args: &Args,
    batch: Batch,
    client: &EnvProver,
    key: &EnvProvingKey,
    elf: Elf,
    state: &mut Value,
) -> Result<()> {
    let root = args
        .batch
        .as_ref()
        .unwrap()
        .canonicalize()?
        .parent()
        .unwrap()
        .to_owned();
    state["declared_attempt_window"] =
        json!(batch.cases.iter().map(|c| &c.case_id).collect::<Vec<_>>());
    let cases: Vec<_> = batch
        .warmup
        .into_iter()
        .map(|c| (false, c))
        .chain(batch.cases.into_iter().map(|c| (true, c)))
        .collect();
    state["cases"] = json!(cases.iter().map(|(window,case)| json!({"case_id":case.case_id,"acceptance_window":window,"status":"not-attempted",
        "input_sha256":case.input_sha256,"public_inputs_sha256":case.public_inputs_sha256,"prepare_record_sha256":case.prepare_record_sha256})).collect::<Vec<_>>());
    checkpoint(&args.out, state)?;
    let mut identities = guards::BatchIdentities::default();
    for (index, (window, case)) in cases.into_iter().enumerate() {
        state["cases"][index]["status"] = json!("started");
        checkpoint(&args.out, state)?;
        let result: Result<()> = async {
            let loaded = case.load(&root)?;
            if window {
                identities.observe(&loaded)?;
            }
            let artifacts = args.out.join(format!("case-{index}-{}", case.case_id));
            fs::create_dir(&artifacts)?;
            guards::new_file(&artifacts.join("portal-prepared.json"), &loaded.prepare)?;
            state["cases"][index]["session"] = json!(loaded.session);
            state["cases"][index]["session_identity_sha256"] = json!(loaded.qualified_session.identity_sha256);
            state["cases"][index]["session_context_sha256"] = json!(loaded.qualified_session.context_sha256);
            state["cases"][index]["session_source_sha256"] = json!(loaded.qualified_session.source_sha256);
            state["cases"][index]["portal_elf_sha256"] = json!(loaded.qualified_session.portal_elf_sha256);
            state["cases"][index]["checkpoint"] = json!(loaded.checkpoint);
            state["cases"][index]["challenge"] = json!(loaded.challenge);
            state["cases"][index]["deadline_slot"] = json!(loaded.deadline_slot);
            state["cases"][index]["deadline_boundary"] = json!("actual-slot-check-required-by-integration-before-resolve");
            input_run(args, client, key, elf.clone(), state, InputRun { artifacts:&artifacts,input:loaded.input,expected:loaded.expected,
                rejection:false, context:json!({"case_id":case.case_id,"acceptance_window":window,"prepare_record_sha256":case.prepare_record_sha256}) }).await
        }.await;
        state["cases"][index]["status"] = json!(if result.is_ok() {
            "sdk-verified"
        } else {
            "failed"
        });
        if let Err(error) = &result {
            state["cases"][index]["error"] = json!(format!("{error:#}"));
        }
        checkpoint(&args.out, state)?;
        result?;
    }
    state["validation_level"] = json!("three-distinct-sdk-proofs-not-portal-resolution");
    Ok(())
}

fn panic_error(payload: Box<dyn std::any::Any + Send>) -> anyhow::Error {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic");
    anyhow::anyhow!("SDK/client panic (not a guest rejection receipt): {message}")
}

async fn corpus_run(
    args: &Args,
    plan: (guards::corpus::Corpus, Vec<bool>, std::path::PathBuf),
    client: &EnvProver,
    key: &EnvProvingKey,
    elf: Elf,
    state: &mut Value,
) -> Result<()> {
    let (corpus, selected, root) = plan;
    let mut all_selected_passed = true;
    for (index, case) in corpus.cases().into_iter().enumerate() {
        if !selected[index] {
            continue;
        }
        state["corpus_cases"][index]["status"] = json!("started");
        checkpoint(&args.out, state)?;
        let started = Instant::now();
        let execution = async {
            let artifacts = args.out.join(&case.id);
            fs::create_dir(&artifacts)?;
            let (input, expected) = case.load(&root)?;
            guards::new_file(&artifacts.join("expected-public-values.bin"), &expected)?;
            input_run(args, client, key, elf.clone(), state, InputRun {
                artifacts: &artifacts, input, expected, rejection: case.expected_rejection,
                context: json!({"case_id":case.id,"acceptance_window":false,"control_corpus":true,"expected_rejection":case.expected_rejection}),
            }).await
        };
        let (result, stop, failure_status) = match tokio::time::timeout(
            std::time::Duration::from_secs(300),
            std::panic::AssertUnwindSafe(execution).catch_unwind(),
        )
        .await
        {
            Ok(Ok(result)) => (result, false, "failed"),
            Ok(Err(payload)) => (Err(panic_error(payload)), true, "host-panic"),
            Err(_) => (
                Err(anyhow::anyhow!(
                    "case timeout; no successful guest observation"
                )),
                true,
                "timed-out",
            ),
        };
        let observation = state["phases"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|p| p["phase"] == "execute" && p["case_id"] == case.id)
            .cloned()
            .unwrap_or(Value::Null);
        let row = &mut state["corpus_cases"][index];
        row["wall_ms"] = json!(started.elapsed().as_millis());
        row["execution"] = observation;
        row["passed"] = json!(result.is_ok());
        row["status"] = json!(if result.is_ok() {
            "passed"
        } else {
            failure_status
        });
        if let Err(error) = &result {
            row["error"] = json!(format!("{error:#}"));
        }
        let artifacts = args.out.join(&case.id);
        if artifacts.is_dir() {
            json_file(&artifacts.join("case-result.json"), row)?;
        }
        checkpoint(&args.out, state)?;
        all_selected_passed &= result.is_ok();
        if stop {
            break;
        }
    }
    let selected_complete = state["corpus_cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["selected"] == true)
        .all(|row| row["passed"] == true);
    state["selected_all_passed"] = json!(all_selected_passed && selected_complete);
    state["all_passed"] =
        json!(selected.iter().all(|s| *s) && all_selected_passed && selected_complete);
    state["validation_level"] = json!("cpu-control-corpus-only-not-a-proof-or-portal-acceptance");
    checkpoint(&args.out, state)?;
    ensure!(
        all_selected_passed && selected_complete,
        "control corpus has failed or unobserved selected cases"
    );
    Ok(())
}

async fn run(args: &Args, state: &mut Value) -> Result<()> {
    guards::check_lock(include_str!("../Cargo.lock"))?;
    let elf_bytes = guards::read_bounded(&args.elf, 128 * 1024 * 1024)?;
    let profile_bytes = guards::read_bounded(&args.profile, 1024 * 1024)?;
    state["elf_sha256"] = json!(guards::sha256(&elf_bytes));
    state["profile_sha256"] = json!(guards::sha256(&profile_bytes));
    guards::new_file(&args.out.join("profile.json"), &profile_bytes)?;
    checkpoint(&args.out, state)?;
    let profile = guards::profile(&profile_bytes, &elf_bytes)?;
    state["policy_profile_sha256"] = json!(profile.policy_profile_sha256);
    let elf: Elf = elf_bytes.clone().into();
    let expected = args
        .public_inputs
        .as_ref()
        .map(|path| -> Result<_> { guards::public_values(&guards::read_bounded(path, 256)?) })
        .transpose()?;
    if let Some(value) = expected {
        state["expected_public_inputs_sha256"] = json!(guards::sha256(&value));
        state["public_inputs"] = json!(hex::encode(value));
    }
    let encoded = args
        .input
        .as_ref()
        .map(|path| guards::read_bounded(path, guards::INPUT_LIMIT))
        .transpose()?;
    if let Some(bytes) = &encoded {
        state["input_sha256"] = json!(guards::sha256(bytes));
        state["input_bytes"] = json!(bytes.len());
    }
    let batch = args
        .batch
        .as_ref()
        .map(|path| -> Result<_> {
            let bytes = guards::read_bounded(path, 1024 * 1024)?;
            state["batch_manifest_sha256"] = json!(guards::sha256(&bytes));
            guards::new_file(&args.out.join("batch.json"), &bytes)?;
            guards::Batch::parse(&bytes)
        })
        .transpose()?;
    let corpus = args
        .corpus
        .as_ref()
        .map(|path| -> Result<_> {
            let bytes = guards::read_bounded(path, 1024 * 1024)?;
            state["corpus_manifest_sha256"] = json!(guards::sha256(&bytes));
            guards::new_file(&args.out.join("corpus-manifest.json"), &bytes)?;
            let corpus = guards::corpus::Corpus::parse(&bytes, &profile.policy_profile_sha256)?;
            let selected = corpus.selection(&args.case_ids)?;
            state["corpus_cases"] = json!(corpus
                .cases()
                .iter()
                .zip(&selected)
                .map(|(case, selected)| {
                    let mut row = serde_json::to_value(case).unwrap();
                    row["selected"] = json!(selected);
                    row["status"] = json!(if *selected {
                        "not-attempted"
                    } else {
                        "not-selected"
                    });
                    row["passed"] = json!(false);
                    row
                })
                .collect::<Vec<_>>());
            state["full_corpus_selected"] = json!(selected.iter().all(|s| *s));
            checkpoint(&args.out, state)?;
            let root = path
                .canonicalize()?
                .parent()
                .context("corpus manifest parent")?
                .to_owned();
            corpus.preflight(&root)?;
            state["corpus_artifact_hashes_checked"] = json!(true);
            Ok((corpus, selected, root))
        })
        .transpose()?;
    active(args, state, "client_init", &json!({}))?;
    let init = Instant::now();
    let client = ProverClient::from_env().await;
    state["client_init_ms"] = json!(init.elapsed().as_millis());
    active(args, state, "setup", &json!({}))?;
    let started = Instant::now();
    let key = client.setup(elf.clone()).await?;
    let setup_ms = started.elapsed().as_millis();
    let key_hash = key.verifying_key().bytes32();
    guards::program_key(&key_hash)?;
    let vk_bytes = bincode::serialize(key.verifying_key())?;
    let receipt = SetupReceipt {
        schema: "northstar-sp1-setup-v1".into(),
        sp1_version: "6.8.0".into(),
        elf_sha256: guards::sha256(&elf_bytes),
        profile_sha256: guards::sha256(&profile_bytes),
        policy_profile_sha256: profile.policy_profile_sha256,
        program_vkey_hash: key_hash.clone(),
        verifying_key_sha256: guards::sha256(&vk_bytes),
        production_acceptance: false,
    };
    if let Some(path) = &args.setup_receipt {
        let bytes = guards::read_bounded(path, 1024 * 1024)?;
        state["input_setup_receipt_sha256"] = json!(guards::sha256(&bytes));
        guards::new_file(&args.out.join("input-setup-receipt.json"), &bytes)?;
        checkpoint(&args.out, state)?;
        let supplied: SetupReceipt = serde_json::from_slice(&bytes)?;
        supplied.check(&receipt)?;
    }
    guards::new_file(&args.out.join("verifying-key.bin"), &vk_bytes)?;
    json_file(&args.out.join("setup-receipt.json"), &receipt)?;
    state["setup_receipt_sha256"] = json!(guards::sha256(&fs::read(
        args.out.join("setup-receipt.json")
    )?));
    state["program_vkey_hash"] = json!(key_hash);
    state["verifying_key_sha256"] = json!(receipt.verifying_key_sha256);
    phase(
        state,
        json!({"phase":"setup","wall_ms":setup_ms}),
        &json!({}),
    );
    checkpoint(&args.out, state)?;
    if args.command == Command::Key {
        state["validation_level"] = json!("setup-only-requires-independent-review");
        return Ok(());
    }
    if let Some(batch) = batch {
        return batch_run(args, batch, &client, &key, elf, state).await;
    }
    if let Some((corpus, selected, root)) = corpus {
        return corpus_run(args, (corpus, selected, root), &client, &key, elf, state).await;
    }
    let expected = expected.context("missing canonical expected public values")?;
    if matches!(args.command, Command::Execute | Command::Groth16) {
        input_run(
            args,
            &client,
            &key,
            elf,
            state,
            InputRun {
                artifacts: &args.out,
                input: encoded.context("missing input")?,
                expected,
                rejection: args.expect_rejection,
                context: json!({"case_id":"single-input","acceptance_window":false}),
            },
        )
        .await?;
        state["validation_level"] = json!(match args.command {
            Command::Execute if args.expect_rejection => "observed-nonzero-exit-and-no-output",
            Command::Execute => "cpu-execution-only-not-a-proof",
            _ => "sdk-verified-single-input-development-not-three-distinct-resolutions",
        });
        return Ok(());
    }
    let proof_bytes = guards::read_bounded(args.proof.as_ref().unwrap(), 64 * 1024 * 1024)?;
    let proof: SP1ProofWithPublicValues = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(64 * 1024 * 1024)
        .reject_trailing_bytes()
        .deserialize(&proof_bytes)?;
    let bytes = wrapped_bytes(&proof)?;
    ensure!(
        proof.public_values.as_slice() == expected,
        "serialized proof public values mismatch"
    );
    active(args, state, "verify", &json!({}))?;
    let started = Instant::now();
    client.verify(&proof, key.verifying_key(), None)?;
    let verify_ms = started.elapsed().as_millis();
    phase(
        state,
        json!({"phase":"verify","verify_ms":verify_ms,"sdk_verified":true,"sdk_proof_sha256":guards::sha256(&proof_bytes),
        "proof_sha256":guards::sha256(&bytes),"public_inputs_sha256":guards::sha256(&expected)}),
        &json!({}),
    );
    guards::new_file(&args.out.join("northstar-sp1-groth16.bin"), &proof_bytes)?;
    guards::new_file(&args.out.join("northstar-sp1-groth16-onchain.bin"), &bytes)?;
    guards::new_file(&args.out.join("northstar-sp1-public-inputs.bin"), &expected)?;
    state["validation_level"] = json!("standalone-sdk-verification-not-portal-resolution");
    Ok(())
}
fn wrapped_bytes(proof: &SP1ProofWithPublicValues) -> Result<Vec<u8>> {
    match &proof.proof {
        SP1Proof::Groth16(value) => {
            hex::decode(&value.encoded_proof).context("Groth16 encoding")?;
        }
        _ => bail!("only a real Groth16 proof is supported"),
    }
    ensure!(proof.tee_proof.is_none(), "TEE envelopes are unsupported");
    let bytes = proof.bytes();
    guards::proof_envelope(&bytes)?;
    Ok(bytes)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    args.validate(env::var("SP1_PROVER").ok().as_deref())?;
    if args.command == Command::Groth16 && !cfg!(feature = "cuda") {
        bail!("rebuild with --features cuda");
    }
    fs::create_dir(&args.out).context("fresh output directory required")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&args.out, fs::Permissions::from_mode(0o700))?;
    }
    let started = Instant::now();
    let client_bytes = fs::read(env::current_exe()?)?;
    let mut state = json!({"schema":"northstar-vrf-sp1-client-v1","status":"started","command":args.command,
        "sp1_version":"6.8.0","sp1_circuit_version":sp1_sdk::SP1_CIRCUIT_VERSION,"proof_kind":3,"proof_version":1,
        "client_sha256":guards::sha256(&client_bytes),"source_sha256":source_hash(),"cargo_lock_sha256":guards::sha256(include_bytes!("../Cargo.lock")),
        "started_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),"backend":env::var("SP1_PROVER").unwrap_or_else(|_|"cpu".into()),
        "cache_state":args.cache_state,"cycle_limit":args.cycle_limit,"proof_attempts_started":0,
        "cold_definition":"first proof in one client; initialization/setup/CPU execution are separately timed",
        "production_acceptance":false,"portal_resolution_compute_units":null,"phases":[]});
    drop(client_bytes);
    if args.command == Command::ExecuteCorpus {
        state["all_passed"] = json!(false);
        state["selected_all_passed"] = json!(false);
        state["proof_generated"] = json!(false);
        state["prior_results_imported"] = json!(false);
    }
    checkpoint(&args.out, &state)?;
    let result = match std::panic::AssertUnwindSafe(run(&args, &mut state))
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        Err(payload) => Err(panic_error(payload)),
    };
    state["total_wall_ms"] = json!(started.elapsed().as_millis());
    state["status"] = json!(if result.is_ok() {
        "completed"
    } else {
        "failed-partial-artifacts-retained"
    });
    state["exit_code"] = json!(if result.is_ok() { 0 } else { 1 });
    if let Err(error) = &result {
        state["error"] = json!(format!("{error:#}"));
    }
    let targets: Vec<_> = state["phases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["phase"] == "groth16")
        .filter_map(|p| p["within_120s_prove_wrap"].as_bool())
        .collect();
    state["proof_target_met"] = if targets.is_empty() {
        Value::Null
    } else {
        json!(targets.iter().all(|v| *v))
    };
    let window_targets: Vec<_> = state["phases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["phase"] == "groth16" && p["acceptance_window"] == true)
        .filter_map(|p| p["within_120s_prove_wrap"].as_bool())
        .collect();
    state["declared_window_proof_target_met"] = if window_targets.len() == 3 {
        json!(window_targets.iter().all(|v| *v))
    } else {
        Value::Null
    };
    checkpoint(&args.out, &state)?;
    result
}
