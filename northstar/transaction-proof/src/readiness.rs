//! Host-only diagnostics, not a proof verifier or an extension of the guest relation.

use {
    crate::{
        checkpoint, decode_witness, derive_public_inputs, encode_witness, program_elf_hash,
        public_inputs_bytes, replay, validate_accounts_and_result, validate_header, validate_trace,
        validate_transaction, ReplayError, ReplayWitnessV1, SESSION_CONTEXT_DOMAIN_V1,
        SETTLEMENT_POLICY_VERSION_V1, SOL_MEMCPY_KEY,
    },
    northstar_zk_types::{ErStepPublicInputsV1, FullTransactionPublicInputsV1},
    sha2::{Digest, Sha256},
    std::{collections::BTreeMap, fmt::Write},
};

/// Local resource limit, not a new witness ABI limit or production admission rule.
pub const MAX_READINESS_WITNESS_BYTES: usize = 32 * 1024 * 1024;

pub const VRF_BLOCKERS: [&str; 7] = [
    "pinned_request_fixture_missing",
    "pinned_fulfillment_fixture_missing",
    "request_fulfillment_account_effects_unsupported",
    "curve_syscalls_unsupported",
    "callback_cpi_unsupported",
    "guest_slot_sysvar_semantics_unsupported",
    "new_guest_key_approval_required",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Pass,
    Missing,
    Mismatch,
    Rejected(ReplayError),
}

impl From<Result<(), ReplayError>> for Check {
    fn from(value: Result<(), ReplayError>) -> Self {
        match value {
            Ok(()) => Self::Pass,
            Err(error) => Self::Rejected(error),
        }
    }
}

/// These values must come from independently authenticated checkpoint/profile data.
/// Matching a witness against its own derived inputs is only a consistency check.
pub struct ExpectedBindings<'a> {
    pub public_inputs: &'a [u8],
    pub checkpoint_transaction_effect_root: Option<[u8; 32]>,
    pub runtime_profile_sha256: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessReport {
    pub witness_bytes: usize,
    pub witness_sha256: [u8; 32],
    pub transaction_sha256: [u8; 32],
    pub program_elf_sha256: [u8; 32],
    pub runtime_profile_sha256: [u8; 32],
    pub checkpoint_transaction_effect_root: [u8; 32],
    pub derived_public_inputs: Option<[u8; 256]>,
    pub checks: Vec<(&'static str, Check)>,
    pub pre_accounts: usize,
    pub post_accounts: usize,
    pub rollback_accounts: usize,
    pub readonly_accounts: usize,
    pub instruction_data_bytes: usize,
    pub vm_rows: usize,
    pub vm_invocation_tags: usize,
    pub executed_units: u64,
    pub syscall_rows: BTreeMap<u32, usize>,
}

impl ReadinessReport {
    /// Optional anchors may be absent, but a supplied mismatch always rejects.
    pub fn conformant(&self) -> bool {
        self.checks.iter().all(|(name, check)| {
            *check == Check::Pass
                || (*check == Check::Missing
                    && matches!(*name, "checkpoint_root_anchor" | "runtime_profile_anchor"))
        })
    }

    pub fn all_bindings_anchored(&self) -> bool {
        self.checks.iter().all(|(_, check)| *check == Check::Pass)
    }

    /// Stable, line-oriented output. No wall-clock values or inferred application ABI.
    pub fn to_text(&self) -> String {
        let mut output = String::from("schema=northstar-vrf-proof-readiness-v1\n");
        writeln!(output, "conformant={}", self.conformant()).unwrap();
        writeln!(
            output,
            "all_bindings_anchored={}",
            self.all_bindings_anchored()
        )
        .unwrap();
        output.push_str("vrf_ready=false\nproof_generated=false\n");
        for (name, check) in &self.checks {
            writeln!(output, "check.{name}={check:?}").unwrap();
        }
        for (name, hash) in [
            ("witness_sha256", self.witness_sha256),
            ("transaction_sha256", self.transaction_sha256),
            ("program_elf_sha256", self.program_elf_sha256),
            ("runtime_profile_sha256", self.runtime_profile_sha256),
            (
                "checkpoint_transaction_effect_root",
                self.checkpoint_transaction_effect_root,
            ),
        ] {
            writeln!(output, "{name}={}", hex(&hash)).unwrap();
        }
        if let Some(public) = &self.derived_public_inputs {
            writeln!(output, "derived_public_inputs={}", hex(public)).unwrap();
        }
        for (name, count) in [
            ("witness_bytes", self.witness_bytes),
            ("pre_accounts", self.pre_accounts),
            ("post_accounts", self.post_accounts),
            ("rollback_accounts", self.rollback_accounts),
            ("readonly_accounts", self.readonly_accounts),
            ("instruction_data_bytes", self.instruction_data_bytes),
            ("vm_rows", self.vm_rows),
            ("vm_invocation_tags", self.vm_invocation_tags),
        ] {
            writeln!(output, "observed.{name}={count}").unwrap();
        }
        writeln!(output, "observed.executed_units={}", self.executed_units).unwrap();
        for (key, count) in &self.syscall_rows {
            writeln!(
                output,
                "observed.syscall.{key:08x}={count},{}",
                syscall_name(*key)
            )
            .unwrap();
        }
        for blocker in VRF_BLOCKERS {
            writeln!(output, "vrf.blocker={blocker}").unwrap();
        }
        output
    }
}

/// Fingerprint selected runtime/program policy fields that are not all public inputs
/// of the current relation. This is an out-of-band host pin, not guest enforcement.
pub fn runtime_profile_sha256(witness: &ReplayWitnessV1) -> [u8; 32] {
    let runtime = &witness.runtime;
    let mut hash = Sha256::new();
    hash.update(b"northstar-readiness-runtime-profile-v1");
    hash.update(runtime.agave_revision);
    hash.update(runtime.northstar_revision);
    hash.update(runtime.feature_set_hash);
    hash.update(runtime.lamports_per_signature.to_le_bytes());
    hash.update([runtime.sbpf_version]);
    hash.update(runtime.vm_config_hash);
    hash.update(runtime.syscall_registry_hash);
    hash.update(runtime.program_id);
    hash.update(runtime.programdata_id);
    hash.update(runtime.entry_pc.to_le_bytes());
    hash.update(program_elf_hash(witness));
    hash.update(runtime.program_hash);
    hash.finalize().into()
}

pub fn inspect_encoded(
    encoded: &[u8],
    expected: ExpectedBindings<'_>,
) -> Result<ReadinessReport, ReplayError> {
    if encoded.len() > MAX_READINESS_WITNESS_BYTES {
        return Err(ReplayError::Encoding);
    }
    let witness = decode_witness(encoded)?;
    if encode_witness(&witness)? != encoded {
        return Err(ReplayError::Encoding);
    }
    let elf_hash = program_elf_hash(&witness);
    let profile = runtime_profile_sha256(&witness);
    let expected_public = parse_public_inputs(expected.public_inputs);
    let derived = derive_public_inputs(&witness);
    let public_binding = match (&expected_public, &derived) {
        (Ok(expected), Ok(derived)) if expected == derived => Check::Pass,
        (Ok(_), Ok(_)) => Check::Mismatch,
        (Err(error), _) | (_, Err(error)) => Check::Rejected(*error),
    };
    let mut syscall_rows = BTreeMap::new();
    for row in &witness.vm_rows {
        if row.syscall_key != 0 {
            let count = syscall_rows.entry(row.syscall_key).or_insert(0usize);
            *count = count.saturating_add(1);
        }
    }
    Ok(ReadinessReport {
        witness_bytes: encoded.len(),
        witness_sha256: Sha256::digest(encoded).into(),
        transaction_sha256: Sha256::digest(&witness.transaction_bytes).into(),
        program_elf_sha256: elf_hash,
        runtime_profile_sha256: profile,
        checkpoint_transaction_effect_root: witness.checkpoint.checkpoint_transaction_effect_root,
        derived_public_inputs: derived.ok().map(public_inputs_bytes),
        checks: vec![
            ("canonical_encoding", Check::Pass),
            (
                "canonical_session_context",
                validate_session_context(&witness.session_context).into(),
            ),
            (
                "expected_public_inputs_encoding",
                expected_public.map(|_| ()).into(),
            ),
            ("header", validate_header(&witness).into()),
            (
                "transaction_signature_blockhash",
                validate_transaction(&witness).into(),
            ),
            (
                "accounts_fees_result",
                validate_accounts_and_result(&witness).into(),
            ),
            ("trace", validate_trace(&witness, elf_hash).into()),
            (
                "checkpoint_paths",
                checkpoint::verify_checkpoint_binding(&witness)
                    .map(|_| ())
                    .into(),
            ),
            ("expected_public_inputs_binding", public_binding),
            (
                "checkpoint_root_anchor",
                compare_anchor(
                    expected.checkpoint_transaction_effect_root,
                    witness.checkpoint.checkpoint_transaction_effect_root,
                ),
            ),
            (
                "runtime_profile_anchor",
                compare_anchor(expected.runtime_profile_sha256, profile),
            ),
            // Keep the authoritative route separate from the individual diagnostics.
            ("native_relation", replay(&witness).map(|_| ()).into()),
        ],
        pre_accounts: witness.pre_accounts.len(),
        post_accounts: witness.post_accounts.len(),
        rollback_accounts: witness.rollback_accounts.len(),
        readonly_accounts: witness.readonly_accounts.len(),
        instruction_data_bytes: witness.instruction_data.len(),
        vm_rows: witness.vm_rows.len(),
        vm_invocation_tags: witness.event_tags.iter().filter(|tag| **tag == 5).count(),
        executed_units: witness.result.executed_units,
        syscall_rows,
    })
}

fn validate_session_context(context: &[u8]) -> Result<(), ReplayError> {
    if context.len() != 149
        || !context.starts_with(SESSION_CONTEXT_DOMAIN_V1)
        || context.last() != Some(&SETTLEMENT_POLICY_VERSION_V1)
    {
        return Err(ReplayError::Domain);
    }
    Ok(())
}

fn parse_public_inputs(encoded: &[u8]) -> Result<ErStepPublicInputsV1, ReplayError> {
    let public: ErStepPublicInputsV1 =
        borsh::from_slice(encoded).map_err(|_| ReplayError::Encoding)?;
    FullTransactionPublicInputsV1::try_from(public).map_err(|_| ReplayError::Domain)?;
    Ok(public)
}

fn compare_anchor(expected: Option<[u8; 32]>, actual: [u8; 32]) -> Check {
    match expected {
        None => Check::Missing,
        Some(expected) if expected == actual => Check::Pass,
        Some(_) => Check::Mismatch,
    }
}

/// Names are diagnostic only; none of the curve/CPI keys become allowed syscalls.
pub fn syscall_name(key: u32) -> &'static str {
    match key {
        SOL_MEMCPY_KEY => "sol_memcpy_",
        0xaa26_07ca => "sol_curve_validate_point:unsupported",
        0xdd1c_41a6 => "sol_curve_group_op:unsupported",
        0x60a4_0880 => "sol_curve_multiscalar_mul:unsupported",
        0x080c_98b0 => "sol_curve_decompress:unsupported",
        0xf111_a47e => "sol_curve_pairing_map:unsupported",
        0xa22b_9c85 => "sol_invoke_signed_c:unsupported",
        0xd744_9092 => "sol_invoke_signed_rust:unsupported",
        _ => "unknown:unsupported",
    }
}

pub fn hex(bytes: &[u8]) -> String {
    let mut output = String::new();
    for byte in bytes {
        write!(output, "{byte:02x}").unwrap();
    }
    output
}
