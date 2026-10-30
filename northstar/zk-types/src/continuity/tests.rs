use super::*;

fn payloads() -> [IngressPayloadV2; 5] {
    [
        IngressPayloadV2::Delegation {
            account: [1; 32],
            record: [2; 32],
            owner: [3; 32],
            lamports: 10,
            data_len: 2,
            data_hash: [4; 32],
            executable: false,
            rent_epoch: 0,
        },
        IngressPayloadV2::SolDeposit {
            recipient: [1; 32],
            receipt: [2; 32],
            amount: 10,
            cumulative_deposited: 20,
        },
        IngressPayloadV2::TokenDeposit {
            bridge: [1; 32],
            session_bridge: [2; 32],
            account: [3; 32],
            receipt: [4; 32],
            mint: [5; 32],
            token_program: [6; 32],
            origin_deposited: 7,
            amount: 3,
            cumulative_deposited: 10,
        },
        IngressPayloadV2::Freeze {
            account: [1; 32],
            request: [2; 32],
            requested_l1_slot: 10,
        },
        IngressPayloadV2::Release {
            account: [1; 32],
            request: [2; 32],
        },
    ]
}

#[test]
fn ingress_record_encodings_are_bounded_and_exact() {
    for payload in payloads() {
        payload.validate().unwrap();
        let record = IngressRecordV2 {
            discriminator: INGRESS_RECORD_DISCRIMINATOR_V2,
            version: CHECKPOINT_FORMAT_VERSION_V2,
            session_context: [8; 32],
            sequence: 1,
            l1_slot: 10,
            previous_root: [0; 32],
            payload,
            root: [9; 32],
            bump: 7,
        };
        let mut encoded = borsh::to_vec(&record).unwrap();
        assert!(encoded.len() <= MAX_INGRESS_ENCODING_BYTES_V2);
        assert_eq!(
            borsh::from_slice::<IngressRecordV2>(&encoded).unwrap(),
            record
        );
        encoded.push(0);
        assert!(borsh::from_slice::<IngressRecordV2>(&encoded).is_err());
    }
}

#[test]
fn ingress_payloads_require_positive_monotonic_credit() {
    let mut sol = payloads()[1];
    if let IngressPayloadV2::SolDeposit { amount, .. } = &mut sol {
        *amount = 0;
    }
    assert_eq!(sol.validate(), Err(ContinuityError::InvalidPayload));
    let mut token = payloads()[2];
    if let IngressPayloadV2::TokenDeposit {
        cumulative_deposited,
        ..
    } = &mut token
    {
        *cumulative_deposited = 6;
    }
    assert_eq!(token.validate(), Err(ContinuityError::InvalidPayload));
    let mut delegation = payloads()[0];
    if let IngressPayloadV2::Delegation { executable, .. } = &mut delegation {
        *executable = true;
    }
    assert_eq!(delegation.validate(), Err(ContinuityError::InvalidPayload));
    assert_eq!(
        IngressPayloadV2::Release {
            account: [1; 32],
            request: [0; 32]
        }
        .validate(),
        Err(ContinuityError::InvalidPayload)
    );
}

#[test]
fn continuity_proof_domains_are_canonical_and_family_bound() {
    let execution = TransitionFamilyV2::Execution.proof_domain();
    let ingress = TransitionFamilyV2::Ingress.proof_domain();
    assert_ne!(execution, ingress);
    assert_ne!(
        execution,
        FrBytes::er_step_domain_v1(ER_STEP_PROOF_KIND_CONTINUITY, 1)
    );
    assert!(FrBytes::new(execution.to_bytes()).is_ok());
    assert!(FrBytes::new(ingress.to_bytes()).is_ok());
    assert_eq!(execution.as_bytes()[30], ER_STEP_PROOF_KIND_CONTINUITY);
    let value = FrBytes::from_u64(1);
    let mut inputs = StepPublicInputsV2 {
        domain: execution,
        session_context: value,
        slot_step: value,
        pre_state_root: value,
        post_state_root: value,
        transition_effect_root: value,
        input_context_root: value,
        settlement_effect_root: value,
    };
    inputs.validate(TransitionFamilyV2::Execution).unwrap();
    assert_eq!(
        inputs.validate(TransitionFamilyV2::Ingress),
        Err(ZkTypeError::InvalidProofDomain)
    );
    inputs.input_context_root = FrBytes::ZERO;
    assert_eq!(
        inputs.validate(TransitionFamilyV2::Execution),
        Err(ZkTypeError::MissingPublicCommitment)
    );
}
