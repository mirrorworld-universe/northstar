use {
    crate::{Error, Metadata, Request, Result},
    serde::{Deserialize, Serialize},
    vrf_r255::{Proof, PublicKey, SecretKey},
    zeroize::Zeroizing,
};

/// No Debug/Serialize implementation. Real key provisioning is deliberately absent.
/// Upstream's temporary expanded SecretKey is Copy and does not zeroize on drop.
pub struct OracleKey(Zeroizing<[u8; 32]>);

impl OracleKey {
    /// Imports a canonical nonzero scalar, not an Ed25519 seed or Solana keypair.
    /// The caller remains responsible for any copies outside this allocation.
    pub fn from_scalar(bytes: Zeroizing<[u8; 32]>) -> Result<Self> {
        if !bool::from(SecretKey::from_bytes(*bytes).is_some()) {
            return Err(Error::Secret);
        }
        Ok(Self(bytes))
    }

    fn expanded(&self) -> SecretKey {
        SecretKey::from_bytes(*self.0).unwrap()
    }

    pub fn public_key(&self) -> [u8; 32] {
        PublicKey::from(self.expanded()).to_bytes()
    }

    pub fn prove(&self, metadata: &Metadata, request: &Request) -> Result<VrfResult> {
        request.validate(metadata)?;
        if self.public_key() != request.public_key {
            return Err(Error::Key);
        }
        self.prove_bytes(&request.alpha(metadata)?)
    }

    /// Raw suite operation for published vectors, not an application fulfillment.
    pub fn prove_bytes(&self, alpha: &[u8]) -> Result<VrfResult> {
        let proof = self.expanded().prove(alpha).to_bytes();
        let output = verify_bytes(self.public_key(), alpha, proof)?;
        Ok(VrfResult {
            proof: hex::encode(proof),
            output: hex::encode(output),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VrfResult {
    pub proof: String,
    pub output: String,
}

impl VrfResult {
    pub fn verify_bytes(&self, public_key: [u8; 32], alpha: &[u8]) -> Result<[u8; 64]> {
        let proof = decode::<80>(&self.proof)?;
        let expected = decode::<64>(&self.output)?;
        let output = verify_bytes(public_key, alpha, proof)?;
        if output != expected {
            return Err(Error::Proof);
        }
        Ok(output)
    }
}

/// Independent public-only verification: no signer or secret-key argument.
pub fn verify(metadata: &Metadata, request: &Request, result: &VrfResult) -> Result<[u8; 64]> {
    result.verify_bytes(request.public_key, &request.alpha(metadata)?)
}

fn verify_bytes(public_key: [u8; 32], alpha: &[u8], proof: [u8; 80]) -> Result<[u8; 64]> {
    let key = PublicKey::from_bytes(public_key).ok_or(Error::Proof)?;
    let proof = Proof::from_bytes(proof).ok_or(Error::Proof)?;
    Option::from(key.verify(alpha, &proof)).ok_or(Error::Proof)
}

fn decode<const N: usize>(value: &str) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| Error::Proof)?;
    if hex::encode(bytes) != value {
        return Err(Error::Proof);
    }
    Ok(bytes)
}
