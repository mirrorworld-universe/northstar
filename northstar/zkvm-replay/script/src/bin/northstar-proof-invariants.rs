use {
    anyhow::{anyhow, ensure, Result},
    borsh::to_vec as encode_witness,
    northstar_zkvm_replay_shared::{decode_witness, public_inputs_bytes, replay},
    serde_json::json,
    sp1_sdk::{include_elf, Elf, Prover, ProverClient, SP1Stdin},
    std::{env, fs},
};

const ELF: Elf = include_elf!("northstar-zkvm-replay-program");

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let fixture = args
        .next()
        .ok_or_else(|| anyhow!("usage: northstar-proof-invariants FIXTURE RESULTS"))?;
    let output = args
        .next()
        .ok_or_else(|| anyhow!("results path is required"))?;
    ensure!(args.next().is_none(), "unexpected argument");
    let encoded = fs::read(fixture)?;
    let witness = decode_witness(&encoded).map_err(|error| anyhow!("decode: {error:?}"))?;
    let expected =
        public_inputs_bytes(replay(&witness).map_err(|error| anyhow!("native replay: {error:?}"))?);
    let client = ProverClient::from_env().await;
    let mut stdin = SP1Stdin::new();
    stdin.write_vec(encoded.clone());
    let (public, report) = client.execute(ELF, stdin).await?;
    ensure!(report.exit_code == 0, "valid guest execution failed");
    ensure!(public.as_slice() == expected, "valid public inputs changed");

    let mut signature = witness.clone();
    ensure!(
        signature.transaction_bytes.first() == Some(&1) && signature.transaction_bytes.len() > 65,
        "signature control requires a single-signature fixture"
    );
    signature.transaction_bytes[1] ^= 1;
    let mut instruction = witness.clone();
    instruction
        .vm_rows
        .first_mut()
        .ok_or_else(|| anyhow!("instruction control requires VM rows"))?
        .instruction[0] ^= 1;
    let mut truncated = encoded;
    truncated.pop();
    let cases = [
        (
            "signature_binding",
            encode_witness(&signature).map_err(|error| anyhow!("encode: {error:?}"))?,
        ),
        (
            "instruction_binding",
            encode_witness(&instruction).map_err(|error| anyhow!("encode: {error:?}"))?,
        ),
        ("complete_witness_encoding", truncated),
    ];
    let mut controls = Vec::new();
    for (label, bytes) in cases {
        let native_rejected = decode_witness(&bytes)
            .and_then(|value| replay(&value))
            .is_err();
        ensure!(native_rejected, "native control accepted: {label}");
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(bytes);
        let (public, execution) = client.execute(ELF, stdin).await?;
        ensure!(
            execution.exit_code != 0 && public.as_slice().is_empty(),
            "guest control accepted: {label}"
        );
        controls.push(json!({
            "label": label,
            "native_rejected": true,
            "guest_rejected": true,
            "guest_exit_code": execution.exit_code,
        }));
    }
    fs::write(
        output,
        serde_json::to_vec_pretty(&json!({
            "schema": "northstar-sp1-guest-invariants-v1",
            "valid_public_inputs_match": true,
            "cycles": report.total_instruction_count(),
            "controls": controls,
        }))?,
    )?;
    Ok(())
}
