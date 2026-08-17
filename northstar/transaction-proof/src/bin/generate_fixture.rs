fn main() {
    let witness = northstar_transaction_proof::fixture::build_replay_witness_v1().unwrap();
    let bytes = northstar_transaction_proof::encode_witness(&witness).unwrap();
    let output = std::env::args().nth(1).expect("output path");
    std::fs::write(output, bytes).unwrap();
}
