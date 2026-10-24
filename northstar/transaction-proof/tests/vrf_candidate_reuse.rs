//! Preflight/binding guards over actual canonical captures. EntryGuard never
//! supplies an execution output; actual interpreter differentials run separately.
use {
    northstar_transaction_proof::{public_inputs_bytes, vrf_candidate::*},
    std::{env, fs, path::PathBuf},
};

#[derive(Default)]
struct EntryGuard {
    calls: usize,
}
impl CandidateReplayEngine for EntryGuard {
    fn execute(
        &mut self,
        _: &CandidateExecutionInputV1,
    ) -> Result<CandidateExecutionOutputV1, CandidateError> {
        self.calls = self.calls.checked_add(1).unwrap();
        Err(CandidateError::Unsupported(CandidateUnsupported::Engine))
    }
}

fn fixtures() -> Vec<(CandidateEnvelopeV1, CandidatePolicyV1, Vec<u8>)> {
    let root = PathBuf::from(env::var_os("VRF_REUSE_CASES_ROOT").expect("actual cases root"));
    ["A", "B", "C"]
        .into_iter()
        .map(|name| {
            let directory = root.join(format!("case-{name}/binding"));
            let envelope =
                decode_candidate(&fs::read(directory.join("witness.borsh")).unwrap()).unwrap();
            let policy = CandidatePolicyV1 {
                profile_sha256: hex::decode(
                    "c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d",
                )
                .unwrap()
                .try_into()
                .unwrap(),
                programs: envelope.input.programs.to_vec().try_into().unwrap(),
            };
            assert_eq!(
                candidate_profile_sha256(&envelope.input.profile).unwrap(),
                policy.profile_sha256
            );
            let public = fs::read(directory.join("public-inputs.bin")).unwrap();
            assert_eq!(public.len(), 256);
            (envelope, policy, public)
        })
        .collect()
}

fn signature_change(envelope: &mut CandidateEnvelopeV1) {
    let mut wire = envelope.input.transaction_bytes.clone().into_vec();
    assert_eq!(wire[0], 1);
    wire[1] ^= 1;
    envelope.input.transaction_bytes = BoundedVec::new(wire).unwrap();
}

fn rejected_before_engine(
    envelope: &CandidateEnvelopeV1,
    policy: &CandidatePolicyV1,
    error: CandidateError,
) {
    let mut engine = EntryGuard::default();
    assert_eq!(verify_candidate(envelope, policy, &mut engine), Err(error));
    assert_eq!(engine.calls, 0);
    assert_eq!(
        bind_candidate_execution(
            &envelope.input,
            &envelope.expected_output,
            &envelope.checkpoint,
        ),
        Err(error)
    );
}

#[test]
#[ignore = "requires VRF_REUSE_CASES_ROOT with actual A/B/C canonical bindings"]
fn invariant_standalone_binding_and_internal_authenticated_dispatch() {
    for (envelope, policy, expected) in fixtures() {
        let public = bind_candidate_execution(
            &envelope.input,
            &envelope.expected_output,
            &envelope.checkpoint,
        )
        .unwrap();
        assert_eq!(public_inputs_bytes(public).as_slice(), expected.as_slice());
        assert_eq!(
            candidate_readonly_root(&envelope.input).unwrap(),
            envelope.checkpoint.readonly_context_root
        );
        let mut engine = EntryGuard::default();
        assert_eq!(
            verify_candidate(&envelope, &policy, &mut engine),
            Err(CandidateError::Unsupported(CandidateUnsupported::Engine))
        );
        assert_eq!(engine.calls, 1);
    }
}

#[test]
#[ignore = "requires VRF_REUSE_CASES_ROOT with actual A/B/C canonical bindings"]
fn invariant_public_and_internal_signature_guards() {
    for (mut envelope, policy, _) in fixtures() {
        signature_change(&mut envelope);
        rejected_before_engine(&envelope, &policy, CandidateError::Signature);
        assert_eq!(
            candidate_readonly_context_leaf(&envelope.input),
            Err(CandidateError::Signature)
        );
        assert_eq!(
            candidate_readonly_root(&envelope.input),
            Err(CandidateError::Signature)
        );
    }
}

#[test]
#[ignore = "requires VRF_REUSE_CASES_ROOT with actual A/B/C canonical bindings"]
fn invariant_public_and_internal_prestate_guards() {
    for (mut envelope, policy, _) in fixtures() {
        let mut pre = envelope.checkpoint.pre_state.clone().into_vec();
        let value = pre
            .iter_mut()
            .find(|value| {
                envelope
                    .input
                    .accounts
                    .iter()
                    .any(|entry| entry.key == value.account)
            })
            .unwrap();
        value.lamports = value.lamports.checked_add(1).unwrap();
        envelope.checkpoint.pre_state = BoundedVec::new(pre).unwrap();
        rejected_before_engine(&envelope, &policy, CandidateError::Checkpoint);
    }
}

#[test]
#[ignore = "requires VRF_REUSE_CASES_ROOT with actual A/B/C canonical bindings"]
fn invariant_public_and_internal_readonly_context_guards() {
    for (mut envelope, policy, _) in fixtures() {
        let mut accounts = envelope.input.accounts.clone().into_vec();
        let clock = accounts
            .iter_mut()
            .find(|entry| entry.key == CLOCK_ID)
            .unwrap();
        let mut data = clock.state.as_ref().unwrap().data.clone().into_vec();
        *data.last_mut().unwrap() ^= 1;
        clock.state.as_mut().unwrap().data = BoundedVec::new(data).unwrap();
        envelope.input.accounts = BoundedVec::new(accounts).unwrap();
        // The signed wire is unchanged; its authenticated context is not.
        validate_candidate_input(&envelope.input, &policy).unwrap();
        assert_ne!(
            candidate_readonly_root(&envelope.input).unwrap(),
            envelope.checkpoint.readonly_context_root
        );
        rejected_before_engine(&envelope, &policy, CandidateError::Checkpoint);
    }
}

#[test]
#[ignore = "requires VRF_REUSE_CASES_ROOT with actual A/B/C canonical bindings"]
fn invariant_revalidation_is_local_to_each_verify_call() {
    for (mut envelope, policy, _) in fixtures() {
        let mut engine = EntryGuard::default();
        assert_eq!(
            verify_candidate(&envelope, &policy, &mut engine),
            Err(CandidateError::Unsupported(CandidateUnsupported::Engine))
        );
        assert_eq!(engine.calls, 1);
        signature_change(&mut envelope);
        assert_eq!(
            verify_candidate(&envelope, &policy, &mut engine),
            Err(CandidateError::Signature)
        );
        assert_eq!(engine.calls, 1);
    }
}

#[test]
#[ignore = "requires VRF_REUSE_CASES_ROOT with actual A/B/C canonical bindings"]
fn invariant_cross_layer_settlement_remains_rejected() {
    for (mut envelope, policy, _) in fixtures() {
        envelope.checkpoint.settlement_effect_root[31] ^= 1;
        rejected_before_engine(
            &envelope,
            &policy,
            CandidateError::Unsupported(CandidateUnsupported::CrossLayerSettlement),
        );
    }
}
