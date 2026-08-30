fn main() {
    let mut args = std::env::args().skip(1);
    let output = args.next().expect("output path");
    let witness = match args.next() {
        Some(iterations) => {
            northstar_transaction_proof::fixture::build_benchmark_replay_witness_v1(
                iterations.parse().expect("u32 iteration count"),
            )
        }
        None => northstar_transaction_proof::fixture::build_replay_witness_v1(),
    }
    .unwrap();
    let rows = witness.vm_rows.len();
    let units = witness.result.executed_units;
    let bytes = northstar_transaction_proof::encode_witness(&witness).unwrap();
    std::fs::write(output, &bytes).unwrap();
    println!("rows={rows} units={units} witness_bytes={}", bytes.len());
}
