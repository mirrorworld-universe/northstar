fn main() {
    let mut args = std::env::args().skip(1);
    let output = args.next().expect("output path");
    let iterations = args.next();
    let trace_output = args.next();
    let (witness, trace_bytes) = match iterations {
        Some(iterations) => {
            let artifacts =
                northstar_transaction_proof::fixture::build_benchmark_replay_artifacts_v1(
                    iterations.parse().expect("u32 iteration count"),
                )
                .unwrap();
            (artifacts.0, Some(artifacts.1))
        }
        None => (
            northstar_transaction_proof::fixture::build_replay_witness_v1().unwrap(),
            None,
        ),
    };
    let rows = witness.vm_rows.len();
    let units = witness.result.executed_units;
    let bytes = northstar_transaction_proof::encode_witness(&witness).unwrap();
    std::fs::write(output, &bytes).unwrap();
    if let Some(trace_output) = trace_output {
        std::fs::write(trace_output, trace_bytes.expect("scaled fixture trace")).unwrap();
    }
    println!("rows={rows} units={units} witness_bytes={}", bytes.len());
}
