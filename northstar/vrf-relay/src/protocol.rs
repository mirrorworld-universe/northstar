use {
    crate::crypto::{PROOF_LENGTH, VrfResult},
    sha2::{Digest, Sha256},
};
pub const APPLICATION_SUITE: u8 = 1;
pub const ALPHA_DOMAIN: &[u8] = b"sonic-vrf/v1";
pub const FULFILL_DISCRIMINATOR: [u8; 8] = [143, 2, 52, 206, 174, 164, 247, 72];
pub const VERIFY_DISCRIMINATOR: [u8; 8] = [133, 161, 141, 48, 120, 198, 88, 150];
/// Public request fields, not evidence that an RPC account is authentic or current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlphaInput {
    pub program_id: [u8; 32],
    pub request_address: [u8; 32],
    pub seed: [u8; 32],
    pub request_slot: u64,
    pub hash_slot: u64,
    pub slot_hash: [u8; 32],
}
impl AlphaInput {
    pub fn alpha(&self) -> [u8; 32] {
        Sha256::new()
            .chain_update(ALPHA_DOMAIN)
            .chain_update(self.program_id)
            .chain_update(self.request_address)
            .chain_update(self.seed)
            .chain_update(self.request_slot.to_le_bytes())
            .chain_update(self.hash_slot.to_le_bytes())
            .chain_update(self.slot_hash)
            .finalize()
            .into()
    }
}
/// Coordinator's fulfill argument is still a Borsh Vec, unlike the verifier's fixed array.
/// This only encodes instruction data; accounts, privileges and signatures are not supplied.
pub fn fulfill_data(proof: &[u8; PROOF_LENGTH]) -> [u8; 92] {
    let mut data = [0; 92];
    data[..8].copy_from_slice(&FULFILL_DISCRIMINATOR);
    data[8..12].copy_from_slice(&(PROOF_LENGTH as u32).to_le_bytes());
    data[12..].copy_from_slice(proof);
    data
}
/// Current verifier CPI: four fixed arrays, no suite tag, length prefix or return data.
/// Encoding alone does not verify the supplied result.
pub fn verify_data(public_key: &[u8; 32], alpha: &[u8; 32], result: &VrfResult) -> [u8; 216] {
    let mut data = [0; 216];
    data[..8].copy_from_slice(&VERIFY_DISCRIMINATOR);
    data[8..40].copy_from_slice(public_key);
    data[40..72].copy_from_slice(alpha);
    data[72..152].copy_from_slice(&result.proof);
    data[152..].copy_from_slice(&result.output);
    data
}
