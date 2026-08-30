use {
    anyhow::{anyhow, bail, Result},
    serde_json::json,
    sp1_sdk::{include_elf, Elf, ProveRequest, Prover, ProverClient, ProvingKey, SP1Stdin},
    std::{collections::BTreeMap, env, fs, time::Instant},
};

const ELF: Elf = include_elf!("northstar-zkvm-replay-program");

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "execute".to_string());
    if !matches!(command.as_str(), "execute" | "core" | "groth16" | "all") {
        bail!(
            "usage: northstar-zkvm-replay-script [execute|core|groth16|all] \
             [fixture] [measurements] [profile]"
        );
    }
    let fixture_path = args
        .next()
        .unwrap_or_else(|| "fixture-v1.bin".to_string());
    let measurement_path = args
        .next()
        .unwrap_or_else(|| "sp1-measurements.json".to_string());
    let profile = args.next().unwrap_or_else(|| "baseline".to_string());
    let encoded = fs::read(&fixture_path)?;
    let witness = northstar_zkvm_replay_shared::decode_witness(&encoded)
        .map_err(|error| anyhow!("decode fixture: {error:?}"))?;
    let expected = northstar_zkvm_replay_shared::public_inputs_bytes(
        northstar_zkvm_replay_shared::replay(&witness)
            .map_err(|error| anyhow!("native replay: {error:?}"))?,
    );
    let mut stdin = SP1Stdin::new();
    stdin.write_vec(encoded.clone());
    let client = ProverClient::from_env().await;
    let mut phases = Vec::new();

    if command == "execute" || command == "all" {
        let started = Instant::now();
        let (public, report) = client.execute(ELF, stdin.clone()).await?;
        let wall_ms = started.elapsed().as_millis();
        if public.as_slice() != expected {
            bail!("SP1 execute public values differ from canonical inputs");
        }
        let cycle_tracker = report
            .cycle_tracker
            .iter()
            .map(|(name, cycles)| (name.clone(), *cycles))
            .collect::<BTreeMap<_, _>>();
        phases.push(json!({
            "phase": "execute",
            "wall_ms": wall_ms,
            "cycles": report.total_instruction_count(),
            "gas": report.gas(),
            "syscalls": report.total_syscall_count(),
            "touched_memory_addresses": report.touched_memory_addresses,
            "cycle_tracker": cycle_tracker,
        }));
    }

    let key = if matches!(command.as_str(), "core" | "groth16" | "all") {
        let started = Instant::now();
        let key = client.setup(ELF).await?;
        phases.push(json!({
            "phase": "setup",
            "wall_ms": started.elapsed().as_millis(),
        }));
        Some(key)
    } else {
        None
    };

    if command == "core" || command == "all" {
        let key = key.as_ref().expect("proof command has setup key");
        let started = Instant::now();
        let proof = client.prove(key, stdin.clone()).core().await?;
        let prove_ms = started.elapsed().as_millis();
        if proof.public_values.as_slice() != expected {
            bail!("SP1 core public values differ from canonical inputs");
        }
        let verify_started = Instant::now();
        client.verify(&proof, key.verifying_key(), None)?;
        let verify_ms = verify_started.elapsed().as_millis();
        let proof_path = "northstar-sp1-core.bin";
        proof.save(proof_path)?;
        phases.push(json!({
            "phase": "core",
            "prove_ms": prove_ms,
            "verify_ms": verify_ms,
            "artifact_bytes": usize::try_from(fs::metadata(proof_path)?.len())?,
        }));
    }

    if command == "groth16" || command == "all" {
        let key = key.as_ref().expect("proof command has setup key");
        let started = Instant::now();
        let proof = client.prove(key, stdin).groth16().await?;
        let prove_wrap_ms = started.elapsed().as_millis();
        if proof.public_values.as_slice() != expected {
            bail!("SP1 Groth16 public values differ from canonical inputs");
        }
        let verify_started = Instant::now();
        client.verify(&proof, key.verifying_key(), None)?;
        let verify_ms = verify_started.elapsed().as_millis();
        let proof_path = "northstar-sp1-groth16.bin";
        let proof_bytes = proof.bytes().len();
        proof.save(proof_path)?;
        phases.push(json!({
            "phase": "groth16",
            "prove_and_wrap_ms": prove_wrap_ms,
            "verify_ms": verify_ms,
            "onchain_proof_bytes": proof_bytes,
            "artifact_bytes": usize::try_from(fs::metadata(proof_path)?.len())?,
        }));
    }

    let output = json!({
        "schema": "northstar-sp1-benchmark-v2",
        "profile": profile,
        "witness_bytes": encoded.len(),
        "vm_rows": witness.vm_rows.len(),
        "executed_units": witness.result.executed_units,
        "public_inputs": hex::encode(expected),
        "phases": phases,
    });
    fs::write(&measurement_path, serde_json::to_vec_pretty(&output)?)?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
