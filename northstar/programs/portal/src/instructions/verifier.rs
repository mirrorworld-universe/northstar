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

#[cfg(feature = "zk-verifier-bounded-vrf")]
mod candidate_key;

#[cfg(feature = "zk-verifier-bounded-vrf")]
struct CandidateProfile {
    checkpoint_steps: u32,
    program_vkey_hash: [u8; 32],
    fixed_inputs_key: groth16_solana::groth16::Groth16Verifyingkey<'static>,
}

#[cfg(feature = "zk-verifier-bounded-vrf")]
pub(super) fn bounded_vrf_checkpoint_steps() -> u64 {
    u64::from(candidate_key::CHECKPOINT_STEPS)
}

#[cfg(feature = "zk-verifier-bounded-vrf")]
fn candidate_profile() -> Result<&'static CandidateProfile, PortalError> {
    let profile = candidate_key::PROFILE
        .as_ref()
        .ok_or(PortalError::StepProofVerificationFailed)?;
    if !(1..=16).contains(&profile.checkpoint_steps)
        || profile.checkpoint_steps != candidate_key::CHECKPOINT_STEPS
        || profile.program_vkey_hash == [0; 32]
        || profile.program_vkey_hash == SP1_REPLAY_PROGRAM_VKEY_HASH
        || FrBytes::new(profile.program_vkey_hash).is_err()
        || profile.fixed_inputs_key.nr_pubinputs != 2
        || profile.fixed_inputs_key.vk_ic.len() != 3
        || profile.fixed_inputs_key.vk_ic[0] == [0; 64]
    {
        return Err(PortalError::StepProofVerificationFailed);
    }
    Ok(profile)
}

fn fixed_input_key(
    program_vkey_hash: [u8; 32],
) -> Result<&'static groth16_solana::groth16::Groth16Verifyingkey<'static>, PortalError> {
    if program_vkey_hash == SP1_REPLAY_PROGRAM_VKEY_HASH {
        return Ok(&verifier_key::SP1_FIXED_INPUT_VERIFYING_KEY);
    }
    #[cfg(feature = "zk-verifier-bounded-vrf")]
    {
        let profile = candidate_profile()?;
        if program_vkey_hash == profile.program_vkey_hash {
            return Ok(&profile.fixed_inputs_key);
        }
    }
    Err(PortalError::StepProofVerificationFailed)
}

const SP1_REPLAY_PROGRAM_VKEY_HASH: [u8; 32] = [
    0x00, 0x63, 0xa1, 0x0e, 0x31, 0xe9, 0x06, 0x25, 0xb7, 0x6d, 0x06, 0xbf, 0xa2, 0x2c, 0xbf, 0x0a,
    0x4a, 0x8f, 0xa0, 0x85, 0x0c, 0xcf, 0x71, 0xe0, 0x63, 0xd2, 0x43, 0xbd, 0x1a, 0x7a, 0xa3, 0x1b,
];

fn sp1_public_inputs(
    public_input_bytes: &[u8; 256],
    proof_nonce: FrBytes,
) -> Result<[[u8; 32]; 5], PortalError> {
    let mut fields = [[0; 32]; 8];
    for (field, bytes) in fields
        .iter_mut()
        .zip(public_input_bytes.as_chunks::<32>().0)
    {
        field.copy_from_slice(bytes);
    }
    let public_inputs = ErStepPublicInputsV1::from_array(fields)
        .map_err(|_| PortalError::StepProofVerificationFailed)?;
    let program_vkey_hash = if FullTransactionPublicInputsV1::try_from(public_inputs).is_ok() {
        SP1_REPLAY_PROGRAM_VKEY_HASH
    } else {
        #[cfg(feature = "zk-verifier-bounded-vrf")]
        {
            northstar_zk_types::BoundedVrfPublicInputsV1::try_from(public_inputs)
                .map_err(|_| PortalError::StepProofVerificationFailed)?;
            candidate_profile()?.program_vkey_hash
        }
        #[cfg(not(feature = "zk-verifier-bounded-vrf"))]
        return Err(PortalError::StepProofVerificationFailed);
    };

    let mut committed_values_digest = hashv(&[public_input_bytes]).to_bytes();
    committed_values_digest[0] &= 0x1f;
    Ok([
        program_vkey_hash,
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
    crate::profile::mark("NS_CU verifier.entry");
    let proof = Sp1Groth16ProofV1::from_bytes(proof)
        .map_err(|_| PortalError::StepProofVerificationFailed)?;
    let groth16_inputs = sp1_public_inputs(public_inputs, proof.proof_nonce)?;
    crate::profile::mark("NS_CU verifier.envelope");
    // SP1 emits Gnark's non-negated A; groth16-solana folds the standard
    // pairing equation with -A.
    let proof_a = negate_g1_be(&proof.proof.a);
    let variable_inputs = [groth16_inputs[1], groth16_inputs[4]];
    let mut verifier = Groth16Verifier::<2>::new(
        &proof_a,
        &proof.proof.b,
        &proof.proof.c,
        &variable_inputs,
        fixed_input_key(groth16_inputs[0])?,
    )
    .map_err(|_| PortalError::StepProofVerificationFailed)?;
    crate::profile::mark("NS_CU verifier.msm_pairing_start");
    let result = verifier
        .verify()
        .map_err(|_| PortalError::StepProofVerificationFailed);
    crate::profile::mark("NS_CU verifier.msm_pairing_end");
    result
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
        alloc::{format, vec::Vec},
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
        for (output, input) in bytes
            .as_chunks_mut::<32>()
            .0
            .iter_mut()
            .zip(public.to_array())
        {
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
    fn actual_legacy_proof_accepts_only_its_original_domain() {
        let proof = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../zkvm-replay/evidence/g5-a10g-sha-v1/corpus/resolver/\
             northstar-sp1-groth16-onchain.bin"
        ));
        let public = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../zkvm-replay/evidence/g5-a10g-sha-v1/corpus/resolver/\
             northstar-sp1-public-inputs.bin"
        ));
        assert!(verify_er_step_proof_v1(proof, public).is_ok());
        let envelope = Sp1Groth16ProofV1::from_bytes(proof).unwrap();
        let Ok(inputs) = sp1_public_inputs(public, envelope.proof_nonce) else {
            panic!("retained legacy public inputs must map");
        };
        let canonical_ic = canonical_outer_ic();
        let full_key = groth16_solana::groth16::Groth16Verifyingkey {
            vk_ic: &canonical_ic,
            ..verifier_key::SP1_GROTH16_VERIFYING_KEY
        };
        let proof_a = negate_g1_be(&envelope.proof.a);
        assert!(Groth16Verifier::<5>::new(
            &proof_a,
            &envelope.proof.b,
            &envelope.proof.c,
            &inputs,
            &full_key
        )
        .unwrap()
        .verify()
        .is_ok());
        for (kind, version) in [(0, 1), (1, 1), (2, 0), (2, 2), (3, 1), (3, 2), (4, 1)] {
            let mut changed = *public;
            changed[..32].copy_from_slice(&FrBytes::er_step_domain_v1(kind, version).to_bytes());
            assert!(verify_er_step_proof_v1(proof, &changed).is_err());
        }
        for index in 0..8 {
            let mut changed = *public;
            changed[index * 32..(index + 1) * 32].copy_from_slice(&BN254_FR_MODULUS_BE);
            assert!(verify_er_step_proof_v1(proof, &changed).is_err());
        }
        for index in [0, 4, 36, 68, 100, 164, 292, 355] {
            let mut changed = *proof;
            changed[index] ^= 1;
            assert!(verify_er_step_proof_v1(&changed, public).is_err());
        }
        for length in [0, 255, 355] {
            assert!(verify_er_step_proof_v1(&proof[..length], public).is_err());
        }
    }

    #[test]
    fn historical_v7c_candidate_proof_is_rejected_by_selected_key() {
        let proof = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/zk_verifier/fixtures/bounded-vrf-preview-v1/proof.bin"
        ));
        let public = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/zk_verifier/fixtures/bounded-vrf-preview-v1/public-inputs.bin"
        ));
        let historical: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/zk_verifier/fixtures/bounded-vrf-preview-v1/manifest.json"
        )))
        .unwrap();
        assert_eq!(
            historical["program_vkey_hash"],
            "0x001a26a674f911be746ffa5ea2dbdd412b0ee692bd34ebf2562b0b3bd25bd311"
        );
        assert_eq!(
            historical["proof_sha256"],
            hex::encode(hashv(&[proof]).to_bytes())
        );
        assert_eq!(
            historical["public_inputs_sha256"],
            hex::encode(hashv(&[public]).to_bytes())
        );
        assert!(verify_er_step_proof_v1(proof, public).is_err());
    }

    #[test]
    fn actual_integrated_v11_proof_requires_its_configured_key_and_domain() {
        let proof = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/zk_verifier/fixtures/bounded-vrf-integrated-v11/proof.bin"
        ));
        let public = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/zk_verifier/fixtures/bounded-vrf-integrated-v11/public-inputs.bin"
        ));
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/zk_verifier/fixtures/bounded-vrf-integrated-v11/manifest.json"
        )))
        .unwrap();
        let selected: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/keys/bounded-vrf-v1.json"
        )))
        .unwrap();
        for field in [
            "program_vkey_hash",
            "elf_sha256",
            "profile_sha256",
            "policy_profile_sha256",
            "checkpoint_steps",
        ] {
            assert_eq!(fixture[field], selected[field]);
        }
        assert_eq!(
            fixture["proof_sha256"],
            hex::encode(hashv(&[proof]).to_bytes())
        );
        assert_eq!(
            fixture["public_inputs_sha256"],
            hex::encode(hashv(&[public]).to_bytes())
        );
        assert_eq!(fixture["production_acceptance"], false);
        assert_eq!(fixture["acceptance_window"], false);
        #[cfg(not(feature = "zk-verifier-bounded-vrf"))]
        assert!(verify_er_step_proof_v1(proof, public).is_err());
        #[cfg(feature = "zk-verifier-bounded-vrf")]
        {
            let Some(profile) = candidate_key::PROFILE.as_ref() else {
                panic!("retained actual candidate fixture requires its configured key");
            };
            assert_eq!(
                fixture["program_vkey_hash"],
                format!("0x{}", hex::encode(profile.program_vkey_hash))
            );
            assert!(verify_er_step_proof_v1(proof, public).is_ok());
            let envelope = Sp1Groth16ProofV1::from_bytes(proof).unwrap();
            let Ok(inputs) = sp1_public_inputs(public, envelope.proof_nonce) else {
                panic!("actual candidate inputs must map");
            };
            let canonical_ic = canonical_outer_ic();
            let full_key = groth16_solana::groth16::Groth16Verifyingkey {
                vk_ic: &canonical_ic,
                ..verifier_key::SP1_GROTH16_VERIFYING_KEY
            };
            let proof_a = negate_g1_be(&envelope.proof.a);
            assert!(Groth16Verifier::<5>::new(
                &proof_a,
                &envelope.proof.b,
                &envelope.proof.c,
                &inputs,
                &full_key
            )
            .unwrap()
            .verify()
            .is_ok());
            for index in [0, 4, 36, 68, 100, 164, 292, 355] {
                let mut changed = *proof;
                changed[index] ^= 1;
                assert!(verify_er_step_proof_v1(&changed, public).is_err());
            }
            for index in 0..8 {
                let mut changed = *public;
                changed[index * 32 + 31] ^= 1;
                assert!(verify_er_step_proof_v1(proof, &changed).is_err());
            }
            let mut legacy = *public;
            legacy[..32].copy_from_slice(
                &FrBytes::er_step_domain_v1(ER_STEP_PROOF_KIND_FULL_TRANSACTION, 1).to_bytes(),
            );
            assert!(verify_er_step_proof_v1(proof, &legacy).is_err());
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

    fn canonical_outer_ic() -> Vec<[u8; 64]> {
        let mut points = verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic.to_vec();
        // Independently decompressed from pinned outer key by the tooling test.
        // The unused legacy exit IC retains a sign bit in its y coordinate.
        points[3] = hex::decode("097c875a6ebd0999b06e7267ff3d8a6bf859bb9635abae07cb6b3534ba409a832d84b73fb0f397e356b524f2d0e9ddc9d0f6ac75e566041147ee8554879183ec").unwrap().try_into().unwrap();
        points
    }

    #[test]
    fn fixed_input_key_matches_original_linear_combination() {
        use solana_bn254::prelude::{alt_bn128_g1_addition_be, alt_bn128_g1_multiplication_be};

        fn prepare(
            key: &groth16_solana::groth16::Groth16Verifyingkey,
            inputs: &[[u8; 32]],
        ) -> Vec<u8> {
            let mut point = key.vk_ic[0].to_vec();
            for (base, scalar) in key.vk_ic[1..].iter().zip(inputs) {
                let product =
                    alt_bn128_g1_multiplication_be(&[base.as_slice(), scalar.as_slice()].concat())
                        .unwrap();
                point = alt_bn128_g1_addition_be(&[point, product].concat()).unwrap();
            }
            point
        }

        let canonical_ic = canonical_outer_ic();
        let full_key = groth16_solana::groth16::Groth16Verifyingkey {
            vk_ic: &canonical_ic,
            ..verifier_key::SP1_GROTH16_VERIFYING_KEY
        };
        for nonce in [0, 1, 9, u64::MAX] {
            let mut public = public_inputs();
            public[255] ^= nonce as u8;
            let Ok(inputs) = sp1_public_inputs(&public, FrBytes::from_u64(nonce)) else {
                panic!("canonical inputs must map");
            };
            assert_eq!(
                prepare(&full_key, &inputs),
                prepare(
                    &verifier_key::SP1_FIXED_INPUT_VERIFYING_KEY,
                    &[inputs[1], inputs[4]]
                ),
            );
        }
    }

    #[test]
    fn candidate_and_unknown_domains_fail_closed_until_configured() {
        for kind in [0, 1, 3, 4, 255] {
            for version in [0, 1, 2, 255] {
                let mut public = public_inputs();
                public[..32].copy_from_slice(&FrBytes::er_step_domain_v1(kind, version).to_bytes());
                #[cfg(feature = "zk-verifier-bounded-vrf")]
                if kind == 3 && version == 1 && candidate_key::PROFILE.is_some() {
                    let Ok(inputs) = sp1_public_inputs(&public, FrBytes::from_u64(1)) else {
                        panic!("configured candidate public inputs must map");
                    };
                    assert_ne!(inputs[0], SP1_REPLAY_PROGRAM_VKEY_HASH);
                    assert!(fixed_input_key(inputs[0]).is_ok());
                    assert!(verify_er_step_proof_v1(&proof_bytes(), &public).is_err());
                    continue;
                }
                assert!(sp1_public_inputs(&public, FrBytes::from_u64(1)).is_err());
                assert!(verify_er_step_proof_v1(&proof_bytes(), &public).is_err());
            }
        }
        assert!(fixed_input_key([0; 32]).is_err());
        assert!(fixed_input_key(FrBytes::from_u64(17).to_bytes()).is_err());
    }

    #[test]
    fn rejects_empty_commitments_in_both_domains() {
        for kind in [2, 3] {
            for index in [1, 3, 4, 5, 6, 7] {
                let mut public = public_inputs();
                public[..32].copy_from_slice(&FrBytes::er_step_domain_v1(kind, 1).to_bytes());
                public[index * 32..(index + 1) * 32].fill(0);
                assert!(sp1_public_inputs(&public, FrBytes::from_u64(1)).is_err());
            }
        }
    }

    fn synthetic_folded_ic() -> [u8; 64] {
        hex::decode("22d139de6916f51bf69372b7c1ef8b369212052c22025bf12193bbff551a9e981bf70cf43fbdaf1caa2610a935714996b1df00870c4f770af56a76a81e28a521").unwrap().try_into().unwrap()
    }

    #[test]
    fn actual_legacy_proof_rejects_synthetic_distinct_guest_key() {
        let proof = Sp1Groth16ProofV1::from_bytes(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../zkvm-replay/evidence/g5-a10g-sha-v1/corpus/resolver/\
             northstar-sp1-groth16-onchain.bin"
        )))
        .unwrap();
        let public = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../zkvm-replay/evidence/g5-a10g-sha-v1/corpus/resolver/\
             northstar-sp1-public-inputs.bin"
        ));
        let Ok(inputs) = sp1_public_inputs(public, proof.proof_nonce) else {
            panic!("retained legacy public inputs must map");
        };
        let variable = [inputs[1], inputs[4]];
        let ic = [
            synthetic_folded_ic(),
            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[2],
            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[5],
        ];
        let synthetic_key = groth16_solana::groth16::Groth16Verifyingkey {
            nr_pubinputs: 2,
            vk_ic: &ic,
            ..verifier_key::SP1_GROTH16_VERIFYING_KEY
        };
        let proof_a = negate_g1_be(&proof.proof.a);
        assert!(Groth16Verifier::<2>::new(
            &proof_a,
            &proof.proof.b,
            &proof.proof.c,
            &variable,
            &synthetic_key
        )
        .unwrap()
        .verify()
        .is_err());
    }

    #[test]
    fn synthetic_candidate_fold_matches_independent_bn254_arithmetic() {
        use solana_bn254::prelude::{alt_bn128_g1_addition_be, alt_bn128_g1_multiplication_be};
        // Tooling control only: scalar 17 is not an accepted guest key.
        let mut expected = verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[0].to_vec();
        for (index, scalar) in [
            (1, FrBytes::from_u64(17).to_bytes()),
            (4, SP1_GROTH16_VK_ROOT),
        ] {
            let base = verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[index];
            let product =
                alt_bn128_g1_multiplication_be(&[base.as_slice(), scalar.as_slice()].concat())
                    .unwrap();
            expected = alt_bn128_g1_addition_be(&[expected, product].concat()).unwrap();
        }
        assert_eq!(expected, synthetic_folded_ic());
        assert!(fixed_input_key(FrBytes::from_u64(17).to_bytes()).is_err());
    }

    #[cfg(feature = "zk-verifier-bounded-vrf")]
    #[test]
    fn candidate_manifest_matches_compiled_profile() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/keys/bounded-vrf-v1.json"
        )))
        .unwrap();
        assert_eq!(manifest["proof_kind"], 3);
        assert_eq!(manifest["proof_version"], 1);
        assert_eq!(
            manifest["checkpoint_steps"],
            candidate_key::CHECKPOINT_STEPS
        );
        assert!((1..=16).contains(&candidate_key::CHECKPOINT_STEPS));
        assert_eq!(manifest["production_acceptance"], false);
        assert_eq!(manifest["public_input_bytes"], 256);
        assert_eq!(
            manifest["domain"],
            hex::encode(FrBytes::er_step_domain_v1(3, 1).to_bytes())
        );
        assert_eq!(
            manifest["sp1_groth16_vk_root"],
            format!("0x{}", hex::encode(SP1_GROTH16_VK_ROOT))
        );
        assert_eq!(
            manifest["outer_groth16_vk_sha256"],
            "4388a21c687fdd5f218d7e3d13190cac4c5355818d3605fd5fb811df468ee696"
        );
        assert_eq!(
            manifest["public_input_fields"],
            serde_json::json!([
                "domain",
                "session_context",
                "slot_step",
                "pre_state_root",
                "post_state_root",
                "tx_effect_root",
                "readonly_l1_root",
                "settlement_effect_root"
            ])
        );
        match candidate_key::PROFILE.as_ref() {
            None => {
                assert_eq!(manifest["status"], "unconfigured-fail-closed");
                assert!(manifest["program_vkey_hash"].is_null());
                assert!(manifest["fixed_inputs_ic_be"].is_null());
            }
            Some(profile) => {
                assert_eq!(profile.checkpoint_steps, candidate_key::CHECKPOINT_STEPS);
                assert_eq!(
                    manifest["program_vkey_hash"],
                    format!("0x{}", hex::encode(profile.program_vkey_hash))
                );
                assert_eq!(
                    manifest["fixed_inputs_ic_be"],
                    hex::encode(profile.fixed_inputs_key.vk_ic[0])
                );
                assert_eq!(
                    manifest["generated_rust_sha256"],
                    hex::encode(hashv(&[include_bytes!("verifier/candidate_key.rs")]).to_bytes())
                );
                for field in [
                    "elf_sha256",
                    "sdk_verifying_key_sha256",
                    "setup_receipt_sha256",
                    "profile_sha256",
                    "policy_profile_sha256",
                ] {
                    let digest = hex::decode(manifest[field].as_str().unwrap()).unwrap();
                    assert_eq!(digest.len(), 32);
                    assert_ne!(digest, [0; 32]);
                }
                for (field, bytes) in [
                    (
                        "profile_sha256",
                        include_bytes!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/keys/bounded-vrf-deployment-v1.json"
                        ))
                        .as_slice(),
                    ),
                    (
                        "setup_receipt_sha256",
                        include_bytes!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/keys/bounded-vrf-setup-v1.json"
                        ))
                        .as_slice(),
                    ),
                    (
                        "sdk_verifying_key_sha256",
                        include_bytes!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/keys/bounded-vrf-sp1-verifying-key.bin"
                        ))
                        .as_slice(),
                    ),
                ] {
                    assert_eq!(manifest[field], hex::encode(hashv(&[bytes]).to_bytes()));
                }
                let deployment: serde_json::Value = serde_json::from_str(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/keys/bounded-vrf-deployment-v1.json"
                )))
                .unwrap();
                let receipt: serde_json::Value = serde_json::from_str(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/keys/bounded-vrf-setup-v1.json"
                )))
                .unwrap();
                assert_eq!(deployment["guest_status"], "authenticated-envelope");
                assert_eq!(deployment["checkpoint_steps"], profile.checkpoint_steps);
                assert_eq!(deployment["production_acceptance"], false);
                assert_eq!(receipt["production_acceptance"], false);
                assert_eq!(receipt["program_vkey_hash"], manifest["program_vkey_hash"]);
                for field in ["elf_sha256", "sp1_version", "policy_profile_sha256"] {
                    assert_eq!(deployment[field], manifest[field]);
                    assert_eq!(receipt[field], manifest[field]);
                }
                assert_eq!(receipt["profile_sha256"], manifest["profile_sha256"]);
                assert_eq!(
                    receipt["verifying_key_sha256"],
                    manifest["sdk_verifying_key_sha256"]
                );
            }
        }
    }

    #[cfg(feature = "zk-verifier-bounded-vrf")]
    #[test]
    fn candidate_fixed_input_key_matches_full_key_when_configured() {
        use solana_bn254::prelude::{alt_bn128_g1_addition_be, alt_bn128_g1_multiplication_be};
        let Some(profile) = candidate_key::PROFILE.as_ref() else {
            assert!(candidate_profile().is_err());
            return;
        };
        assert!(candidate_profile().is_ok());
        let mut expected = verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[0].to_vec();
        for (index, scalar) in [(1, profile.program_vkey_hash), (4, SP1_GROTH16_VK_ROOT)] {
            let base = verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[index];
            let product =
                alt_bn128_g1_multiplication_be(&[base.as_slice(), scalar.as_slice()].concat())
                    .unwrap();
            expected = alt_bn128_g1_addition_be(&[expected, product].concat()).unwrap();
        }
        assert_eq!(profile.fixed_inputs_key.vk_ic[0].as_slice(), expected);
        assert_eq!(
            profile.fixed_inputs_key.vk_ic[1],
            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[2]
        );
        assert_eq!(
            profile.fixed_inputs_key.vk_ic[2],
            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[5]
        );
        assert_eq!(
            profile.fixed_inputs_key.vk_alpha_g1,
            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_alpha_g1
        );
        assert_eq!(
            profile.fixed_inputs_key.vk_beta_g2,
            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_beta_g2
        );
        assert_eq!(
            profile.fixed_inputs_key.vk_gamma_g2,
            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_gamma_g2
        );
        assert_eq!(
            profile.fixed_inputs_key.vk_delta_g2,
            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_delta_g2
        );
        assert!(profile.fixed_inputs_key.vk_commitment.is_none());
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
