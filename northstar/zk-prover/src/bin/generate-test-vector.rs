use {
    ark_ff::{BigInteger, PrimeField},
    ark_serialize::{CanonicalSerialize, Compress},
    ark_std::rand::SeedableRng,
    northstar_zk_prover::{
        constraint_count, proof_to_solana, prove, sample_circuit, setup, verifying_key_to_solana,
        ACCOUNT_TREE_DEPTH,
    },
    rand_chacha::ChaCha20Rng,
    serde_json::json,
    std::{
        env, fs,
        path::PathBuf,
        time::{Duration, Instant},
    },
};

const SETUP_SEED: [u8; 32] = [7; 32];
const PROOF_SEED: [u8; 32] = [8; 32];

fn field_hex(value: ark_bn254::Fr) -> String {
    let encoded = value.into_bigint().to_bytes_be();
    format!("0x{:0>64}", hex::encode(encoded))
}

fn bytes_hex(bytes: impl AsRef<[u8]>) -> String {
    format!("0x{}", hex::encode(bytes))
}

#[allow(clippy::arithmetic_side_effects)]
fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn main() {
    let output_dir = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("northstar/zk-prover/test-vectors"));
    fs::create_dir_all(&output_dir).expect("create test-vector directory");

    let circuit = sample_circuit().expect("build sample transition");
    let constraints = constraint_count(circuit.clone()).expect("count constraints");

    let setup_started = Instant::now();
    let mut setup_rng = ChaCha20Rng::from_seed(SETUP_SEED);
    let proving_key = setup(circuit.clone(), &mut setup_rng).expect("generate Groth16 parameters");
    let setup_ms = duration_ms(setup_started.elapsed());

    let mut proof_rng = ChaCha20Rng::from_seed(PROOF_SEED);
    let proof = prove(&proving_key, circuit.clone(), &mut proof_rng).expect("generate test proof");
    let proof_raw = proof_to_solana(&proof).expect("convert proof to Solana format");
    let verifying_key =
        verifying_key_to_solana(&proving_key.vk).expect("convert verifying key to Solana format");

    let public_inputs: Vec<String> = circuit.public.to_array().iter().map(bytes_hex).collect();
    let siblings: Vec<String> = circuit
        .witness
        .siblings
        .iter()
        .copied()
        .map(field_hex)
        .collect();

    let mut proving_times_ms = Vec::with_capacity(10);
    for iteration in 0..10u8 {
        let seed = 32u8
            .checked_add(iteration)
            .expect("benchmark seed does not overflow");
        let mut rng = ChaCha20Rng::from_seed([seed; 32]);
        let started = Instant::now();
        prove(&proving_key, circuit.clone(), &mut rng).expect("benchmark proof");
        proving_times_ms.push(duration_ms(started.elapsed()));
    }
    proving_times_ms.sort_by(f64::total_cmp);
    let proving_median_ms = proving_times_ms[4].midpoint(proving_times_ms[5]);

    let vector = json!({
        "schema": "northstar-one-account-transition-v1",
        "warning": "Deterministic test-only Groth16 setup; never use these parameters for production funds.",
        "circuit": {
            "account_tree_depth": ACCOUNT_TREE_DEPTH,
            "constraints": constraints,
            "public_input_count": public_inputs.len()
        },
        "deterministic_seeds": {
            "setup": bytes_hex(SETUP_SEED),
            "proof": bytes_hex(PROOF_SEED)
        },
        "public_inputs_be": public_inputs,
        "proof_be": {
            "a": bytes_hex(proof_raw.a),
            "b": bytes_hex(proof_raw.b),
            "c": bytes_hex(proof_raw.c)
        },
        "verifying_key_be": {
            "alpha_g1": bytes_hex(verifying_key.alpha_g1),
            "beta_g2": bytes_hex(verifying_key.beta_g2),
            "gamma_g2": bytes_hex(verifying_key.gamma_g2),
            "delta_g2": bytes_hex(verifying_key.delta_g2),
            "ic": verifying_key.ic.iter().map(bytes_hex).collect::<Vec<_>>()
        },
        "witness": {
            "account_id": field_hex(circuit.witness.pre.account_id),
            "owner": field_hex(circuit.witness.pre.owner),
            "pre_lamports": circuit.witness.pre.lamports,
            "post_lamports": circuit.witness.post.lamports,
            "pre_data_root": field_hex(circuit.witness.pre.data_root),
            "post_data_root": field_hex(circuit.witness.post.data_root),
            "siblings": siblings,
            "path_bits_le": circuit.witness.path_bits
        }
    });
    let vector_path = output_dir.join("one-account-transition-v1.json");
    fs::write(
        &vector_path,
        format!("{}\n", serde_json::to_string_pretty(&vector).unwrap()),
    )
    .expect("write test vector");

    let metrics = json!({
        "constraints": constraints,
        "setup_ms": setup_ms,
        "warm_proving_ms": proving_times_ms,
        "warm_proving_median_ms": proving_median_ms,
        "proving_key_compressed_bytes": proving_key.serialized_size(Compress::Yes),
        "verifying_key_compressed_bytes": proving_key.vk.serialized_size(Compress::Yes),
        "solana_proof_bytes": proof_raw.to_bytes().len(),
        "solana_verifying_key_bytes": 1_024
    });
    let metrics_path = output_dir.join("one-account-transition-v1.metrics.json");
    fs::write(
        &metrics_path,
        format!("{}\n", serde_json::to_string_pretty(&metrics).unwrap()),
    )
    .expect("write benchmark metrics");

    println!("wrote {}", vector_path.display());
    println!("wrote {}", metrics_path.display());
    println!("constraints={constraints}");
    println!("setup_ms={setup_ms:.3}");
    println!("warm_proving_median_ms={proving_median_ms:.3}");
}
