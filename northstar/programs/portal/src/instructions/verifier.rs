use {
    crate::{PortalError, VerifyErStepProofV1},
    groth16_solana::groth16::Groth16Verifier,
    northstar_zk_types::{ErStepPublicInputsV1, Groth16ProofRaw},
    pinocchio::ProgramResult,
};

mod verifier_key;

pub fn process_verify_er_step_proof_v1(
    VerifyErStepProofV1 {
        proof,
        public_inputs,
    }: VerifyErStepProofV1,
) -> ProgramResult {
    let proof = Groth16ProofRaw::from_bytes(&proof)
        .map_err(|_| PortalError::StepProofVerificationFailed)?;
    let public_inputs = ErStepPublicInputsV1::from_array(public_inputs)
        .map_err(|_| PortalError::StepProofVerificationFailed)?
        .to_array();
    let mut verifier = Groth16Verifier::<8>::new(
        &proof.a,
        &proof.b,
        &proof.c,
        &public_inputs,
        &verifier_key::VERIFYING_KEY,
    )
    .map_err(|_| PortalError::StepProofVerificationFailed)?;
    verifier
        .verify()
        .map_err(|_| PortalError::StepProofVerificationFailed.into())
}

#[cfg(test)]
mod tests {
    use {super::*, northstar_zk_types::BN254_FR_MODULUS_BE, serde_json::Value};

    fn decode<const N: usize>(value: &str) -> [u8; N] {
        let decoded = hex::decode(value.strip_prefix("0x").unwrap()).unwrap();
        decoded.try_into().unwrap()
    }

    fn test_instruction() -> VerifyErStepProofV1 {
        let vector: Value = serde_json::from_str(include_str!(
            "../../../../zk-prover/test-vectors/one-account-transition-v1.json"
        ))
        .unwrap();
        let proof = &vector["proof_be"];
        let mut proof_bytes = [0; 256];
        proof_bytes[..64].copy_from_slice(&decode::<64>(proof["a"].as_str().unwrap()));
        proof_bytes[64..192].copy_from_slice(&decode::<128>(proof["b"].as_str().unwrap()));
        proof_bytes[192..].copy_from_slice(&decode::<64>(proof["c"].as_str().unwrap()));
        let public = vector["public_inputs_be"].as_array().unwrap();
        let public_inputs =
            core::array::from_fn(|index| decode::<32>(public[index].as_str().unwrap()));
        VerifyErStepProofV1 {
            proof: proof_bytes,
            public_inputs,
        }
    }

    #[test]
    fn verifies_generated_transition_vector() {
        process_verify_er_step_proof_v1(test_instruction()).unwrap();
    }

    #[test]
    fn rejects_mutated_proof() {
        let mut instruction = test_instruction();
        instruction.proof[63] ^= 1;
        assert_eq!(
            process_verify_er_step_proof_v1(instruction),
            Err(PortalError::StepProofVerificationFailed.into())
        );
    }

    #[test]
    fn rejects_mutated_public_input() {
        let mut instruction = test_instruction();
        instruction.public_inputs[7][31] ^= 1;
        assert_eq!(
            process_verify_er_step_proof_v1(instruction),
            Err(PortalError::StepProofVerificationFailed.into())
        );
    }

    #[test]
    fn rejects_noncanonical_public_input() {
        let mut instruction = test_instruction();
        instruction.public_inputs[0] = BN254_FR_MODULUS_BE;
        assert_eq!(
            process_verify_er_step_proof_v1(instruction),
            Err(PortalError::StepProofVerificationFailed.into())
        );
    }
}
