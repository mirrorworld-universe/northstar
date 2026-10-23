use {
    anyhow::{anyhow, bail, Context, Result},
    serde::Deserialize,
    serde_json::json,
    sp1_sdk::{
        include_elf, Elf, HashableKey, ProveRequest, Prover, ProverClient, ProvingKey, SP1Stdin,
    },
    std::{env, fs, path::PathBuf, time::Instant},
};

const ELF: Elf = include_elf!("northstar-zkvm-replay-program");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    label: String,
    witness: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let manifest = args.next().context("expected corpus JSON path")?;
    let output = PathBuf::from(args.next().context("expected new output directory")?);
    let preflight_only = match args.next().as_deref() {
        None => false,
        Some("--preflight") => true,
        _ => bail!("expected --preflight or no third argument"),
    };
    if args.next().is_some() || (!preflight_only && env::var("SP1_PROVER").as_deref() != Ok("cuda"))
    {
        bail!(
            "usage: SP1_PROVER=cuda northstar-proof-corpus MANIFEST OUTPUT_DIRECTORY [--preflight]"
        );
    }
    let mut cases: Vec<Case> = serde_json::from_slice(&fs::read(&manifest)?)?;
    let base = std::path::Path::new(&manifest)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    for case in &mut cases {
        if case.witness.is_relative() {
            case.witness = base.join(&case.witness);
        }
    }
    if cases.is_empty() {
        bail!("empty corpus");
    }
    fs::create_dir(&output)?;
    let mut preflight = Vec::new();
    let mut labels = std::collections::BTreeSet::new();
    for case in &cases {
        if case.label.is_empty()
            || !case
                .label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || !labels.insert(&case.label)
        {
            bail!("invalid or duplicate corpus label");
        }
        let result = (|| -> Result<_> {
            let encoded = fs::read(&case.witness)?;
            let witness = northstar_zkvm_replay_shared::decode_witness(&encoded)
                .map_err(|error| anyhow!("decode: {error:?}"))?;
            northstar_zkvm_replay_shared::replay(&witness)
                .map_err(|error| anyhow!("native replay: {error:?}"))?;
            Ok(
                json!({"label": case.label, "ok": true, "witness_bytes": encoded.len(),
                "vm_rows": witness.vm_rows.len(), "executed_units": witness.result.executed_units}),
            )
        })();
        preflight.push(match result {
            Ok(value) => value,
            Err(error) => json!({"label": case.label, "ok": false, "error": error.to_string()}),
        });
    }
    drop(labels);
    fs::write(
        output.join("preflight.json"),
        serde_json::to_vec_pretty(&preflight)?,
    )?;
    if preflight.iter().any(|case| case["ok"] != true) {
        bail!("corpus includes unsupported witnesses; see preflight.json");
    }
    if preflight_only {
        return Ok(());
    }
    let started = Instant::now();
    let client = ProverClient::from_env().await;
    let key = client.setup(ELF).await?;
    let startup_ms = started.elapsed().as_millis();
    let program_vkey_hash = key.verifying_key().bytes32();
    let mut outcomes = Vec::new();
    for (index, case) in cases.into_iter().enumerate() {
        let work = output.join(&case.label);
        fs::create_dir(&work)?;
        let encoded = fs::read(&case.witness)?;
        let witness = northstar_zkvm_replay_shared::decode_witness(&encoded)
            .map_err(|error| anyhow!("{}: decode: {error:?}", case.label))?;
        let expected = northstar_zkvm_replay_shared::public_inputs_bytes(
            northstar_zkvm_replay_shared::replay(&witness)
                .map_err(|error| anyhow!("{}: native replay: {error:?}", case.label))?,
        );
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(encoded.clone());
        eprintln!("CORPUS {} start", case.label);
        let started = Instant::now();
        let proof = client.prove(&key, stdin).groth16().await?;
        let prove_and_wrap_ms = started.elapsed().as_millis();
        if proof.public_values.as_slice() != expected {
            bail!("{}: public inputs differ from native replay", case.label);
        }
        let started = Instant::now();
        client.verify(&proof, key.verifying_key(), None)?;
        let verify_ms = started.elapsed().as_millis();
        let bytes = proof.bytes();
        if bytes.len() != 356 {
            bail!("{}: unexpected proof envelope length", case.label);
        }
        proof.save(work.join("northstar-sp1-groth16.bin"))?;
        fs::write(work.join("northstar-sp1-groth16-onchain.bin"), &bytes)?;
        fs::write(work.join("northstar-sp1-public-inputs.bin"), expected)?;
        let measured = json!({
            "schema": "northstar-proof-corpus-v1",
            "label": case.label,
            "index": index,
            "process_cold": index == 0,
            "program_vkey_hash": program_vkey_hash,
            "witness_bytes": encoded.len(),
            "vm_rows": witness.vm_rows.len(),
            "executed_units": witness.result.executed_units,
            "public_inputs": hex::encode(expected),
            "prove_and_wrap_ms": prove_and_wrap_ms,
            "verify_ms": verify_ms,
            "onchain_proof_bytes": bytes.len(),
            "within_120_seconds": prove_and_wrap_ms + verify_ms <= 120_000,
        });
        fs::write(
            work.join("measurements.json"),
            serde_json::to_vec_pretty(&measured)?,
        )?;
        outcomes.push(measured);
        fs::write(
            output.join("summary.json"),
            serde_json::to_vec_pretty(&json!({
                "startup_ms": startup_ms,
                "program_vkey_hash": program_vkey_hash,
                "cases": outcomes,
            }))?,
        )?;
        eprintln!(
            "CORPUS {} prove_and_wrap_ms={prove_and_wrap_ms} verify_ms={verify_ms}",
            case.label
        );
    }
    Ok(())
}
