use {
    curve25519_dalek::edwards::CompressedEdwardsY,
    thiserror::Error,
    vrf_rfc9381::{
        Proof as _, Verifier as _,
        ec::edwards25519::{EdVrfProof, tai::EdVrfEdwards25519TaiPublicKey},
    },
};
pub const RFC_SUITE_STRING: u8 = 0x03;
pub const PROOF_LENGTH: usize = 80;
pub const OUTPUT_LENGTH: usize = 64;
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("public key must be a canonical, non-small-order Edwards25519 point")]
    PublicKey,
    #[error("seed does not match the expected public key")]
    KeyMismatch,
    #[error("proof must use canonical point and scalar encodings")]
    ProofEncoding,
    #[error("proof is not valid for this key and input")]
    Proof,
    #[error("verified output does not match the supplied output")]
    Output,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VrfResult {
    pub proof: [u8; PROOF_LENGTH],
    pub output: [u8; OUTPUT_LENGTH],
}
fn validated_key(bytes: &[u8; 32]) -> Result<EdVrfEdwards25519TaiPublicKey, Error> {
    let point = CompressedEdwardsY(*bytes)
        .decompress()
        .ok_or(Error::PublicKey)?;
    if point.compress().to_bytes() != *bytes || point.is_small_order() {
        return Err(Error::PublicKey);
    }
    EdVrfEdwards25519TaiPublicKey::from_slice(bytes).map_err(|_| Error::PublicKey)
}
pub fn verify(
    public_key: &[u8; 32],
    alpha: &[u8],
    proof: &[u8],
) -> Result<[u8; OUTPUT_LENGTH], Error> {
    let verifier = validated_key(public_key)?;
    let decoded = EdVrfProof::decode_pi(proof).map_err(|_| Error::ProofEncoding)?;
    if decoded.encode_to_pi() != proof {
        return Err(Error::ProofEncoding);
    }
    verifier
        .verify(alpha, decoded)
        .map(Into::into)
        .map_err(|_| Error::Proof)
}
pub fn verify_output(public_key: &[u8; 32], alpha: &[u8], result: &VrfResult) -> Result<(), Error> {
    if verify(public_key, alpha, &result.proof)? != result.output {
        return Err(Error::Output);
    }
    Ok(())
}
