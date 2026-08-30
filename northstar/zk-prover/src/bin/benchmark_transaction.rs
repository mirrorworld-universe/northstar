use {
    ark_bn254::{Bn254, Fr},
    ark_ff::PrimeField,
    ark_groth16::{prepare_verifying_key, Groth16},
    ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem},
    ark_serialize::{CanonicalSerialize, Compress},
    ark_std::rand::SeedableRng,
    northstar_transaction_proof::{
        encode_witness,
        fixture::{assemble_replay_witness_v1, benchmark_profile_v1},
        public_inputs_bytes, replay,
    },
    northstar_zk_prover::{
        prove, setup,
        transaction::{execution_table_metrics, SbpfExecutionTableCircuitV1},
    },
    rand_chacha::ChaCha20Rng,
    serde_json::json,
    sha2::{Digest, Sha256},
    solana_runtime::conformance::proof_fixture::execute_full_transaction_benchmark_fixture_v1,
    std::{env, time::Instant},
};

fn main() {
    let profile_name = env::args()
        .nth(1)
        .expect("benchmark profile: 1k, 10k, or 100k");
    let profile = benchmark_profile_v1(&profile_name).expect("known benchmark profile");

    let tracing_started = Instant::now();
    let executed = execute_full_transaction_benchmark_fixture_v1(profile.iterations);
    executed.assert_expected_success();
    let trace_bytes = executed
        .trace
        .canonical_bytes()
        .expect("encode canonical trace");
    let tracing_ms = tracing_started.elapsed().as_millis();

    let witness_started = Instant::now();
    let witness = assemble_replay_witness_v1(executed).expect("assemble deterministic witness");
    let encoded_witness = encode_witness(&witness).expect("encode deterministic witness");
    let witness_ms = witness_started.elapsed().as_millis();
    assert_eq!(witness.vm_rows.len(), profile.expected_vm_rows);

    let execution_started = Instant::now();
    let public = replay(&witness).expect("replay deterministic fixture");
    let execution_ms = execution_started.elapsed().as_millis();
    let table = execution_table_metrics(&witness);
    let account_data_bytes = witness
        .pre_accounts
        .iter()
        .chain(&witness.post_accounts)
        .chain(&witness.readonly_accounts)
        .map(|account| account.data.len())
        .sum::<usize>();
    let accounts = witness
        .pre_accounts
        .len()
        .saturating_add(witness.readonly_accounts.len());
    let circuit = SbpfExecutionTableCircuitV1 { public, witness };

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
            "schema": "northstar-custom-sbpf-benchmark-v1",
            "profile": profile.name,
            "iterations": profile.iterations,
            "tracing_ms": tracing_ms,
            "witness_generation_ms": witness_ms,
            "native_execution_ms": execution_ms,
            "constraint_generation_ms": constraints_ms,
            "setup_ms": setup_ms,
            "prove_ms": prove_ms,
            "verify_ms": verify_ms,
            "trace_bytes": trace_bytes.len(),
            "trace_sha256": hex::encode(Sha256::digest(&trace_bytes)),
            "witness_bytes": encoded_witness.len(),
            "witness_sha256": hex::encode(Sha256::digest(&encoded_witness)),
            "public_inputs": hex::encode(public_inputs_bytes(circuit.public)),
            "rows": table.rows,
            "opcodes": table.opcodes,
            "alu_rows": table.alu_rows,
            "branch_rows": table.branch_rows,
            "load_rows": table.load_rows,
            "store_rows": table.store_rows,
            "call_rows": table.call_rows,
            "exit_rows": table.exit_rows,
            "syscalls": table.syscalls,
            "accounts": accounts,
            "account_data_bytes": account_data_bytes,
            "executed_units": circuit.witness.result.executed_units,
            "constraints": constraints,
            "constraints_per_row": constraints as f64 / table.rows as f64,
            "proving_key_bytes": proving_key.serialized_size(Compress::Yes),
            "verifying_key_bytes": proving_key.vk.serialized_size(Compress::Yes),
            "proof_bytes": proof.serialized_size(Compress::Yes),
        }))
        .expect("encode benchmark JSON")
    );
}
