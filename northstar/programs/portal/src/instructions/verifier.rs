use {
    crate::{PortalError, VerifyErStepProofV1},
    groth16_solana::groth16::Groth16Verifier,
    northstar_zk_types::{ErStepPublicInputsV1, Groth16ProofRaw},
    pinocchio::{AccountView as AccountInfo, ProgramResult},
    pinocchio_idl_macros::p_instruction,
};

mod verifier_key;

#[p_instruction(
    id = 28,
    data = [proof: [u8; 256], public_inputs: [u8; 256]]
)]
pub fn process_verify_er_step_proof_v1(
    accounts: &mut [AccountInfo],
    VerifyErStepProofV1 {
        proof,
        public_inputs,
    }: VerifyErStepProofV1,
) -> ProgramResult {
    let _ = accounts;
    let proof = Groth16ProofRaw::from_bytes(&proof)
        .map_err(|_| PortalError::StepProofVerificationFailed)?;
    let mut groth16_inputs = [[0; 32]; 8];
    for (output, input) in groth16_inputs
        .iter_mut()
        .zip(public_inputs.chunks_exact(32))
    {
        output.copy_from_slice(input);
    }
    let public_inputs = ErStepPublicInputsV1::from_array(groth16_inputs)
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
        let mut public_inputs = [0; 256];
        for (output, input) in public_inputs.chunks_exact_mut(32).zip(public) {
            output.copy_from_slice(&decode::<32>(input.as_str().unwrap()));
        }
        VerifyErStepProofV1 {
            proof: proof_bytes,
            public_inputs,
        }
    }

    #[test]
    fn verifies_generated_transition_vector() {
        process_verify_er_step_proof_v1(&mut [], test_instruction()).unwrap();
    }

    #[test]
    fn rejects_mutated_proof() {
        let mut instruction = test_instruction();
        instruction.proof[63] ^= 1;
        assert_eq!(
            process_verify_er_step_proof_v1(&mut [], instruction),
            Err(PortalError::StepProofVerificationFailed.into())
        );
    }

    #[test]
    fn rejects_mutated_public_input() {
        let mut instruction = test_instruction();
        instruction.public_inputs[255] ^= 1;
        assert_eq!(
            process_verify_er_step_proof_v1(&mut [], instruction),
            Err(PortalError::StepProofVerificationFailed.into())
        );
    }

    #[test]
    fn rejects_noncanonical_public_input() {
        let mut instruction = test_instruction();
        instruction.public_inputs[..32].copy_from_slice(&BN254_FR_MODULUS_BE);
        assert_eq!(
            process_verify_er_step_proof_v1(&mut [], instruction),
            Err(PortalError::StepProofVerificationFailed.into())
        );
    }
}
