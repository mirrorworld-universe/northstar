use crate::proof_jobs::ProofJobBinding;

#[derive(Clone)]
pub struct VerifiedProof {
    binding: ProofJobBinding,
    bytes: [u8; northstar_zk_types::SP1_GROTH16_PROOF_V1_LEN],
}

impl VerifiedProof {
    #[cfg(feature = "proof-coordinator")]
    pub fn verify(binding: ProofJobBinding, proof: &[u8]) -> Result<Self, &'static str> {
        use {
            borsh::BorshDeserialize,
            northstar_zk_types::{
                ErStepPublicInputsV1, FullTransactionPublicInputsV1, Sp1Groth16ProofV1,
            },
        };
        Sp1Groth16ProofV1::from_bytes(proof).map_err(|_| "proof envelope rejected")?;
        let public = ErStepPublicInputsV1::try_from_slice(&binding.public_inputs)
            .map_err(|_| "noncanonical proof public inputs")?;
        FullTransactionPublicInputsV1::try_from(public).map_err(|_| "proof domain rejected")?;
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../zkvm-replay/partial-candidate-v1.json"))
                .map_err(|_| "proof candidate manifest")?;
        let key = manifest["program_vkey_hash"]
            .as_str()
            .ok_or("proof program key missing")?;
        sp1_verifier::Groth16Verifier::verify(
            proof,
            &binding.public_inputs,
            key,
            &sp1_verifier::GROTH16_VK_BYTES,
        )
        .map_err(|_| "proof cryptographic verification failed")?;
        Ok(Self {
            binding,
            bytes: proof.try_into().map_err(|_| "proof length")?,
        })
    }

    pub fn bytes_for(
        &self,
        binding: &ProofJobBinding,
    ) -> Result<&[u8; northstar_zk_types::SP1_GROTH16_PROOF_V1_LEN], &'static str> {
        if &self.binding != binding {
            return Err("verified proof job binding changed");
        }
        Ok(&self.bytes)
    }

    #[cfg(test)]
    pub(crate) fn for_reconciliation_test(
        binding: ProofJobBinding,
        bytes: [u8; northstar_zk_types::SP1_GROTH16_PROOF_V1_LEN],
    ) -> Self {
        Self { binding, bytes }
    }
}

#[cfg(all(test, feature = "proof-coordinator"))]
mod tests {
    use super::*;

    #[test]
    fn verifies_frozen_candidate_and_rejects_changed_proof_or_public_inputs() {
        let proof =
            include_bytes!("../zkvm-replay/evidence/sp1-v6.8.0/northstar-sp1-groth16-onchain.bin");
        let mut binding = ProofJobBinding {
            portal: [1; 32],
            session: [2; 32],
            checkpoint: [3; 32],
            challenge: [4; 32],
            er_slot: 5,
            step: 6,
            public_inputs: *include_bytes!(
                "../zkvm-replay/evidence/sp1-v6.8.0/northstar-sp1-public-inputs.bin"
            ),
        };
        let verified = VerifiedProof::verify(binding.clone(), proof).unwrap();
        assert_eq!(verified.bytes_for(&binding).unwrap(), proof);
        binding.step += 1;
        assert!(verified.bytes_for(&binding).is_err());
        let mut changed = *proof;
        changed[355] ^= 1;
        assert!(VerifiedProof::verify(binding.clone(), &changed).is_err());
        binding.public_inputs[255] ^= 1;
        assert!(VerifiedProof::verify(binding, proof).is_err());
    }
}
