use {
    crate::{PortalError, VerifyErStepProofV1},
    groth16_solana::groth16::{negate_g1_be, Groth16Verifier},
    northstar_zk_types::{
        ErStepPublicInputsV1, FrBytes, FullTransactionPublicInputsV1, Sp1Groth16ProofV1,
        SP1_GROTH16_VK_ROOT,
    },
    pinocchio::{AccountView as AccountInfo, ProgramResult},
    pinocchio_idl_macros::p_instruction,
    solana_sha256_hasher::hashv,
};

mod verifier_key;

const SP1_REPLAY_PROGRAM_VKEY_HASH: [u8; 32] = [
    0x00, 0x50, 0x53, 0x5e, 0x1d, 0x64, 0x50, 0xca, 0x9f, 0x99, 0xea, 0x6a, 0xc4, 0x33, 0xac, 0xc1,
    0x4e, 0x0d, 0xef, 0xb4, 0x79, 0x5e, 0xe3, 0x6f, 0x3e, 0x8f, 0xb3, 0x75, 0x15, 0x5f, 0x9e, 0x6c,
];

fn sp1_public_inputs(
    public_input_bytes: &[u8; 256],
    proof_nonce: FrBytes,
) -> Result<[[u8; 32]; 5], PortalError> {
    let mut fields = [[0; 32]; 8];
    for (field, bytes) in fields.iter_mut().zip(public_input_bytes.chunks_exact(32)) {
        field.copy_from_slice(bytes);
    }
    let public_inputs = ErStepPublicInputsV1::from_array(fields)
        .map_err(|_| PortalError::StepProofVerificationFailed)?;
    FullTransactionPublicInputsV1::try_from(public_inputs)
        .map_err(|_| PortalError::StepProofVerificationFailed)?;

    let mut committed_values_digest = hashv(&[public_input_bytes]).to_bytes();
    committed_values_digest[0] &= 0x1f;
    Ok([
        SP1_REPLAY_PROGRAM_VKEY_HASH,
        committed_values_digest,
        [0; 32],
        SP1_GROTH16_VK_ROOT,
        proof_nonce.to_bytes(),
    ])
}

pub(crate) fn verify_er_step_proof_v1(
    proof: &[u8],
    public_inputs: &[u8; 256],
) -> Result<(), PortalError> {
    let proof = Sp1Groth16ProofV1::from_bytes(proof)
        .map_err(|_| PortalError::StepProofVerificationFailed)?;
    let groth16_inputs = sp1_public_inputs(public_inputs, proof.proof_nonce)?;
    // SP1 emits Gnark's non-negated A; groth16-solana folds the standard
    // pairing equation with -A.
    let proof_a = negate_g1_be(&proof.proof.a);
    let mut verifier = Groth16Verifier::<5>::new(
        &proof_a,
        &proof.proof.b,
        &proof.proof.c,
        &groth16_inputs,
        &verifier_key::SP1_GROTH16_VERIFYING_KEY,
    )
    .map_err(|_| PortalError::StepProofVerificationFailed)?;
    verifier
        .verify()
        .map_err(|_| PortalError::StepProofVerificationFailed)
}

#[p_instruction(
    id = 30,
    data = [proof: [u8; 356], public_inputs: [u8; 256]]
)]
pub fn process_verify_er_step_proof_v1(
    accounts: &mut [AccountInfo],
    VerifyErStepProofV1 {
        proof,
        public_inputs,
    }: VerifyErStepProofV1,
) -> ProgramResult {
    let _ = accounts;
    verify_er_step_proof_v1(&proof, &public_inputs).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        northstar_zk_types::{
            ErStepPublicInputsV1, FrBytes, Groth16ProofRaw, BN254_FR_MODULUS_BE,
            ER_STEP_PROOF_KIND_FULL_TRANSACTION, ER_STEP_PROOF_VERSION_V1,
            SP1_GROTH16_PROOF_V1_LEN, SP1_GROTH16_VK_HASH_PREFIX,
        },
    };

    fn public_inputs() -> [u8; 256] {
        let public = ErStepPublicInputsV1 {
            domain: FrBytes::er_step_domain_v1(
                ER_STEP_PROOF_KIND_FULL_TRANSACTION,
                ER_STEP_PROOF_VERSION_V1,
            ),
            session_context: FrBytes::from_u64(1),
            slot_step: FrBytes::from_u64_pair(2, 3),
            pre_state_root: FrBytes::from_u64(4),
            post_state_root: FrBytes::from_u64(5),
            tx_effect_root: FrBytes::from_u64(6),
            readonly_l1_root: FrBytes::from_u64(7),
            settlement_effect_root: FrBytes::from_u64(8),
        };
        let mut bytes = [0; 256];
        for (output, input) in bytes.chunks_exact_mut(32).zip(public.to_array()) {
            output.copy_from_slice(&input);
        }
        bytes
    }

    fn proof_bytes() -> [u8; SP1_GROTH16_PROOF_V1_LEN] {
        let mut bytes = [0; SP1_GROTH16_PROOF_V1_LEN];
        bytes[..4].copy_from_slice(&SP1_GROTH16_VK_HASH_PREFIX);
        bytes[36..68].copy_from_slice(&SP1_GROTH16_VK_ROOT);
        bytes[99] = 1;
        bytes[100..].copy_from_slice(
            &Groth16ProofRaw {
                a: [1; 64],
                b: [2; 128],
                c: [3; 64],
            }
            .to_bytes(),
        );
        bytes
    }

    fn test_instruction() -> VerifyErStepProofV1 {
        VerifyErStepProofV1 {
            proof: proof_bytes(),
            public_inputs: public_inputs(),
        }
    }

    #[test]
    fn replay_program_key_matches_candidate_manifest() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../zkvm-replay/partial-candidate-v1.json"
        )))
        .unwrap();
        assert_eq!(
            manifest["program_vkey_hash"].as_str().unwrap(),
            format!("0x{}", hex::encode(SP1_REPLAY_PROGRAM_VKEY_HASH))
        );
        assert_eq!(manifest["production_acceptance"], false);
    }

    #[test]
    fn maps_eight_portal_fields_to_sp1_inputs() {
        let public = public_inputs();
        let Ok(inputs) = sp1_public_inputs(&public, FrBytes::from_u64(9)) else {
            panic!("canonical public inputs must map to SP1 inputs");
        };
        assert_eq!(inputs[0], SP1_REPLAY_PROGRAM_VKEY_HASH);
        assert_eq!(inputs[2], [0; 32]);
        assert_eq!(inputs[3], SP1_GROTH16_VK_ROOT);
        assert_eq!(inputs[4], FrBytes::from_u64(9).to_bytes());

        let mut expected_digest = hashv(&[&public]).to_bytes();
        expected_digest[0] &= 0x1f;
        assert_eq!(inputs[1], expected_digest);

        let mut changed = public;
        changed[255] ^= 1;
        let Ok(changed_inputs) = sp1_public_inputs(&changed, FrBytes::from_u64(9)) else {
            panic!("changed canonical public inputs must still map");
        };
        assert_ne!(changed_inputs[1], inputs[1]);
    }

    #[test]
    fn rejects_non_full_transaction_domain() {
        let mut public = public_inputs();
        public[..32].copy_from_slice(&FrBytes::er_step_domain_v1(1, 1).to_bytes());
        assert!(matches!(
            sp1_public_inputs(&public, FrBytes::from_u64(9)),
            Err(PortalError::StepProofVerificationFailed)
        ));
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

    #[test]
    fn rejects_invalid_sp1_envelope() {
        let mut instruction = test_instruction();
        instruction.proof[0] ^= 1;
        assert_eq!(
            process_verify_er_step_proof_v1(&mut [], instruction),
            Err(PortalError::StepProofVerificationFailed.into())
        );
    }

    #[test]
    fn rejects_invalid_groth16_points() {
        assert_eq!(
            process_verify_er_step_proof_v1(&mut [], test_instruction()),
            Err(PortalError::StepProofVerificationFailed.into())
        );
    }
}
