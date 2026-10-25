#![no_main]
sp1_zkvm::entrypoint!(main);

include!(concat!(env!("OUT_DIR"), "/policy.rs"));

pub fn main() {
    #[cfg(feature = "crypto-checks")]
    northstar_zkvm_vrf_shared::crypto_vectors::check();
    let bytes = sp1_zkvm::io::read_vec();
    println!("cycle-tracker-report-start: bounded-sbpf-replay");
    let public = northstar_zkvm_vrf_shared::replay(&bytes, APPROVED_PROFILE_SHA256)
        .expect("authenticated actual bounded interpreter execution");
    println!("cycle-tracker-report-end: bounded-sbpf-replay");
    sp1_zkvm::io::commit_slice(&public);
}
