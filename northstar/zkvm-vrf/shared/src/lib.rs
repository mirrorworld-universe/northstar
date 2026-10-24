#[cfg(feature = "crypto-checks")]
pub mod crypto_vectors;

use northstar_transaction_proof::vrf_candidate::{
    CandidateError, CandidatePolicyV1, decode_candidate, verify_candidate,
};

/// This orchestration always instantiates the actual interpreter. The guest
/// supplies its policy digest from compiled code, never from its witness.
pub fn replay(
    bytes: &[u8],
    approved_profile_sha256: [u8; 32],
) -> Result<[u8; 256], CandidateError> {
    #[cfg(feature = "cycle-profiling")]
    println!("cycle-tracker-report-start: candidate_policy");
    let compiled_profile = northstar_vrf_sbpf_replay::development_profile();
    let compiled_digest =
        northstar_transaction_proof::vrf_candidate::candidate_profile_sha256(&compiled_profile)?;
    if compiled_digest != approved_profile_sha256 {
        return Err(CandidateError::Profile);
    }
    #[cfg(feature = "cycle-profiling")]
    println!("cycle-tracker-report-end: candidate_policy");
    #[cfg(feature = "cycle-profiling")]
    println!("cycle-tracker-report-start: candidate_decode");
    let envelope = decode_candidate(bytes)?;
    #[cfg(feature = "cycle-profiling")]
    println!("cycle-tracker-report-end: candidate_decode");
    let policy = CandidatePolicyV1 {
        profile_sha256: approved_profile_sha256,
        programs: northstar_vrf_sbpf_replay::pinned_programs(),
    };
    #[cfg(not(feature = "cycle-profiling"))]
    let mut engine = northstar_vrf_sbpf_replay::AgaveReplayEngine;
    #[cfg(feature = "cycle-profiling")]
    let mut engine = ProfiledEngine;
    #[cfg(feature = "cycle-profiling")]
    println!("cycle-tracker-report-start: candidate_verify");
    let result = verify_candidate(&envelope, &policy, &mut engine);
    #[cfg(feature = "cycle-profiling")]
    println!("cycle-tracker-report-end: candidate_verify");
    let public = result?;
    let mut encoded = [0; 256];
    for (field, output) in public
        .to_array()
        .iter()
        .zip(encoded.as_chunks_mut::<32>().0.iter_mut())
    {
        output.copy_from_slice(field);
    }
    Ok(encoded)
}

#[cfg(feature = "cycle-profiling")]
struct ProfiledEngine;

#[cfg(feature = "cycle-profiling")]
impl northstar_transaction_proof::vrf_candidate::CandidateReplayEngine for ProfiledEngine {
    fn execute(
        &mut self,
        input: &northstar_transaction_proof::vrf_candidate::CandidateExecutionInputV1,
    ) -> Result<
        northstar_transaction_proof::vrf_candidate::CandidateExecutionOutputV1,
        CandidateError,
    > {
        println!("cycle-tracker-report-start: actual_sbpf");
        let result = northstar_transaction_proof::vrf_candidate::CandidateReplayEngine::execute(
            &mut northstar_vrf_sbpf_replay::AgaveReplayEngine,
            input,
        );
        println!("cycle-tracker-report-end: actual_sbpf");
        result
    }
}
