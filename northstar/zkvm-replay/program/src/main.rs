#![no_main]
sp1_zkvm::entrypoint!(main);

pub fn main() {
    let witness_bytes = sp1_zkvm::io::read_vec();
    println!("cycle-tracker-report-start: decode_witness");
    let witness = northstar_zkvm_replay_shared::decode_witness(&witness_bytes)
        .expect("canonical witness encoding");
    println!("cycle-tracker-report-end: decode_witness");
    let public = northstar_zkvm_replay_shared::replay(&witness).expect("supported replay");
    sp1_zkvm::io::commit_slice(&northstar_zkvm_replay_shared::public_inputs_bytes(public));
}
