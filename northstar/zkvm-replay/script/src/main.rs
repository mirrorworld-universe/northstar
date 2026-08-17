use {
    anyhow::{anyhow, bail, Result},
    serde::Serialize,
    sp1_sdk::{include_elf, Elf, ProveRequest, Prover, ProverClient, ProvingKey, SP1Stdin},
    std::{env, fs, time::Instant},
};

const ELF: Elf = include_elf!("northstar-zkvm-replay-program");

#[derive(Serialize)]
struct Measurement {
    phase: &'static str,
    wall_ms: u128,
    bytes: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let command = env::args().nth(1).unwrap_or_else(|| "execute".to_string());
    if !matches!(command.as_str(), "execute" | "core" | "groth16" | "all") {
        bail!("usage: northstar-zkvm-replay-script [execute|core|groth16|all]");
    }
    let encoded = include_bytes!("../../fixture-v1.bin").to_vec();
    let witness = northstar_zkvm_replay_shared::decode_witness(&encoded)
        .map_err(|error| anyhow!("decode tracked real-Bank fixture: {error:?}"))?;
    let expected = northstar_zkvm_replay_shared::public_inputs_bytes(
        northstar_zkvm_replay_shared::replay(&witness)
            .map_err(|error| anyhow!("native replay: {error:?}"))?,
    );
    let mut stdin = SP1Stdin::new();
    stdin.write_vec(encoded.clone());
    let client = ProverClient::from_env().await;
    let mut measurements = Vec::new();

    if command == "execute" || command == "all" {
        let started = Instant::now();
        let (public, report) = client.execute(ELF, stdin.clone()).await?;
        if public.as_slice() != expected {
            bail!("SP1 execute public values differ from canonical inputs");
        }
        measurements.push(Measurement {
            phase: "sp1_execute",
            wall_ms: started.elapsed().as_millis(),
            bytes: Some(encoded.len()),
        });
        println!("execute cycles={}", report.total_instruction_count());
    }

    let setup_started = Instant::now();
    let key = client.setup(ELF).await?;
    measurements.push(Measurement {
        phase: "sp1_setup",
        wall_ms: setup_started.elapsed().as_millis(),
        bytes: None,
    });

    if command == "core" || command == "all" {
        let started = Instant::now();
        let proof = client.prove(&key, stdin.clone()).core().await?;
        client.verify(&proof, key.verifying_key(), None)?;
        if proof.public_values.as_slice() != expected {
            bail!("SP1 core public values differ from canonical inputs");
        }
        measurements.push(Measurement {
            phase: "sp1_core_prove_verify",
            wall_ms: started.elapsed().as_millis(),
            bytes: Some(proof.bytes().len()),
        });
        proof.save("northstar-sp1-core.bin")?;
    }

    if command == "groth16" || command == "all" {
        let started = Instant::now();
        let proof = client.prove(&key, stdin).groth16().await?;
        client.verify(&proof, key.verifying_key(), None)?;
        if proof.public_values.as_slice() != expected {
            bail!("SP1 Groth16 public values differ from canonical inputs");
        }
        measurements.push(Measurement {
            phase: "sp1_groth16_prove_verify",
            wall_ms: started.elapsed().as_millis(),
            bytes: Some(proof.bytes().len()),
        });
        proof.save("northstar-sp1-groth16.bin")?;
    }

    fs::write(
        "sp1-measurements.json",
        serde_json::to_vec_pretty(&measurements)?,
    )?;
    println!("public_inputs=0x{}", hex::encode(expected));
    Ok(())
}
