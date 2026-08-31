use {
    ark_bn254::{Bn254, Fr},
    ark_ff::PrimeField,
    ark_groth16::{prepare_verifying_key, Groth16},
    ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem},
    ark_serialize::{CanonicalSerialize, Compress},
    ark_std::rand::SeedableRng,
    northstar_transaction_proof::{fixture::build_replay_witness_v1, replay},
    northstar_zk_prover::{
        prove, setup,
        transaction::{execution_table_metrics, SbpfExecutionTableCircuitV1},
    },
    rand_chacha::ChaCha20Rng,
    serde_json::json,
    std::time::Instant,
};

fn main() {
    let table_started = Instant::now();
    let witness = build_replay_witness_v1().expect("build deterministic fixture");
    let public = replay(&witness).expect("replay deterministic fixture");
    let circuit = SbpfExecutionTableCircuitV1 { public, witness };
    let table_ms = table_started.elapsed().as_millis();
    let table = execution_table_metrics(&circuit.witness);

    let constraints_started = Instant::now();
    let cs = ConstraintSystem::<Fr>::new_ref();
    circuit
        .clone()
        .generate_constraints(cs.clone())
        .expect("generate execution-table constraints");
    assert!(cs.is_satisfied().expect("check constraints"));
    let constraints_ms = constraints_started.elapsed().as_millis();
    let constraints = cs.num_constraints();

    let setup_started = Instant::now();
    let mut setup_rng = ChaCha20Rng::from_seed([31; 32]);
    let proving_key = setup(circuit.clone(), &mut setup_rng).expect("Groth16 setup");
    let setup_ms = setup_started.elapsed().as_millis();

    let prove_started = Instant::now();
    let mut proof_rng = ChaCha20Rng::from_seed([32; 32]);
    let proof = prove(&proving_key, circuit.clone(), &mut proof_rng).expect("Groth16 prove");
    let prove_ms = prove_started.elapsed().as_millis();

    let verify_started = Instant::now();
    let prepared = prepare_verifying_key(&proving_key.vk);
    let inputs = circuit
        .public
        .to_array()
        .map(|value| Fr::from_be_bytes_mod_order(&value));
    assert!(Groth16::<Bn254>::verify_proof(&prepared, &proof, &inputs).expect("Groth16 verify"));
    let verify_ms = verify_started.elapsed().as_millis();

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "table_ms": table_ms,
            "constraint_generation_ms": constraints_ms,
            "setup_ms": setup_ms,
            "prove_ms": prove_ms,
            "verify_ms": verify_ms,
            "rows": table.rows,
            "constraints": constraints,
            "constraints_per_row": constraints as f64 / table.rows as f64,
            "syscalls": table.syscalls,
            "proving_key_bytes": proving_key.serialized_size(Compress::Yes),
            "verifying_key_bytes": proving_key.vk.serialized_size(Compress::Yes),
            "proof_bytes": proof.serialized_size(Compress::Yes),
        }))
        .expect("encode benchmark JSON")
    );
}
