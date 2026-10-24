#![no_main]
sp1_zkvm::entrypoint!(main);

/// Compilation-only feasibility guest. No public values are committed until the
/// authenticated candidate envelope is connected; this is not an accepted proof route.
pub fn main() {
    let message: solana_message::Message = sp1_zkvm::io::read();
    let accounts: Vec<(solana_pubkey::Pubkey, solana_account::Account)> = sp1_zkvm::io::read();
    let (slot, compute_unit_limit): (u64, u64) = sp1_zkvm::io::read();
    let message =
        solana_message::SanitizedMessage::try_from_legacy_message(message, &Default::default())
            .expect("sanitized legacy message");
    let output = northstar_vrf_sbpf_replay::execute(&northstar_vrf_sbpf_replay::ExecutionInput {
        message,
        accounts,
        slot,
        compute_unit_limit,
    })
    .expect("bounded actual execution");
    assert!(output.status.is_ok());
    panic!("compilation qualification only; authenticated envelope not yet connected");
}
