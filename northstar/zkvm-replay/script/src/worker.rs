use {
    anyhow::{anyhow, bail, Result},
    serde::Deserialize,
    serde_json::json,
    sp1_sdk::{HashableKey, ProveRequest, Prover, ProverClient, ProvingKey, SP1Stdin},
    std::{fs, path::Path, time::Instant},
    tokio::io::{AsyncBufReadExt, BufReader},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    profile: String,
}

pub async fn serve(root: &Path) -> Result<()> {
    let started = Instant::now();
    let client = ProverClient::from_env().await;
    let client_init_ms = started.elapsed().as_millis();
    let started = Instant::now();
    let key = client.setup(super::ELF).await?;
    let setup_ms = started.elapsed().as_millis();
    let program_vkey_hash = key.verifying_key().bytes32();
    fs::write(
        root.join("ready.tmp"),
        serde_json::to_vec(&json!({
            "program_vkey_hash": program_vkey_hash,
            "client_init_ms": client_init_ms,
            "setup_ms": setup_ms,
        }))?,
    )?;
    fs::rename(root.join("ready.tmp"), root.join("ready.json"))?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let request: Request = serde_json::from_str(&line)?;
        if request.id.len() != 32
            || !request.id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || request.profile.len() > 128
        {
            bail!("invalid worker request");
        }
        let work = root.join(&request.id);
        let result: Result<()> = async {
            let encoded = fs::read(work.join("witness.bin"))?;
            let witness = northstar_zkvm_replay_shared::decode_witness(&encoded)
                .map_err(|error| anyhow!("decode fixture: {error:?}"))?;
            let expected = northstar_zkvm_replay_shared::public_inputs_bytes(
                northstar_zkvm_replay_shared::replay(&witness)
                    .map_err(|error| anyhow!("native replay: {error:?}"))?,
            );
            let mut stdin = SP1Stdin::new();
            stdin.write_vec(encoded.clone());
            let started = Instant::now();
            let proof = client.prove(&key, stdin).groth16().await?;
            let prove_ms = started.elapsed().as_millis();
            if proof.public_values.as_slice() != expected {
                bail!("Groth16 public values differ from canonical inputs");
            }
            let started = Instant::now();
            client.verify(&proof, key.verifying_key(), None)?;
            let verify_ms = started.elapsed().as_millis();
            let bytes = proof.bytes();
            if bytes.len() != 356 || prove_ms == 0 || prove_ms > 120_000 {
                bail!("proof envelope or prove-and-wrap limit exceeded");
            }
            proof.save(work.join("northstar-sp1-groth16.bin"))?;
            fs::write(work.join("northstar-sp1-groth16-onchain.bin"), &bytes)?;
            fs::write(work.join("northstar-sp1-public-inputs.bin"), expected)?;
            let measurements = json!({
                "schema": "northstar-sp1-benchmark-v2",
                "profile": request.profile,
                "program_vkey_hash": program_vkey_hash,
                "public_inputs": hex::encode(expected),
                "witness_bytes": encoded.len(),
                "vm_rows": witness.vm_rows.len(),
                "executed_units": witness.result.executed_units,
                "worker_request_id": request.id,
                "sp1_groth16_vkey_sha256": super::GROTH16_VKEY_SHA256,
                "sp1_verifier_root": hex::encode(*sp1_verifier::VK_ROOT_BYTES),
                "phases": [{
                    "phase": "groth16",
                    "prove_and_wrap_ms": prove_ms,
                    "verify_ms": verify_ms,
                    "onchain_proof_bytes": bytes.len(),
                    "onchain_proof_path": "northstar-sp1-groth16-onchain.bin",
                    "public_inputs_path": "northstar-sp1-public-inputs.bin",
                    "artifact_bytes": fs::metadata(work.join("northstar-sp1-groth16.bin"))?.len(),
                    "proof_layout": {
                        "groth16_vkey_hash_prefix": hex::encode(&bytes[..4]),
                        "exit_code": hex::encode(&bytes[4..36]),
                        "vk_root": hex::encode(&bytes[36..68]),
                        "proof_nonce": hex::encode(&bytes[68..100]),
                        "raw_groth16_bytes": bytes.len() - 100,
                    },
                }],
            });
            fs::write(
                work.join("measurements.json"),
                serde_json::to_vec_pretty(&measurements)?,
            )?;
            Ok(())
        }
        .await;
        let response = match &result {
            Ok(()) => json!({"id": request.id, "ok": true}),
            Err(error) => json!({"id": request.id, "ok": false, "error": error.to_string()}),
        };
        fs::write(work.join("result.tmp"), serde_json::to_vec(&response)?)?;
        fs::rename(work.join("result.tmp"), work.join("result.json"))?;
        // A failed request ends this worker so its next caller gets clean prover state.
        result?;
    }
    Ok(())
}
