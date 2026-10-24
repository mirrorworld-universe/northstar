//! Host-only signed-wire observations for a draft profile. Never an admission path.

use {
    crate::{parse_legacy_transaction, read_short_vec, take, take_array, ReplayError},
    ed25519_dalek::{Signature, VerifyingKey},
    sha2::{Digest, Sha256},
    std::fmt::Write,
};

pub const PROFILE_ID: &str = "northstar-vrf-coordinator-proof-profile-v1-draft";
pub const LIBRARY_REVISION: &str = "d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd";
pub const BASELINE_REVISION: &str = "34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52";
/// A diagnostic resource limit, not a change to transaction admission.
pub const MAX_DIAGNOSTIC_TRANSACTION_BYTES: usize = 1232;
pub const COORDINATOR_ID: [u8; 32] =
    solana_pubkey::pubkey!("FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N").to_bytes();
pub const REFERENCE_VERIFY_ID: [u8; 32] =
    solana_pubkey::pubkey!("FUBz1nvSSoHnMqFoK2CA3jF5ckpYPdF3ZT4SoLzDNzh1").to_bytes();
pub const TEST_CONSUMER_ID: [u8; 32] =
    solana_pubkey::pubkey!("HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt").to_bytes();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticError {
    ResourceLimit,
    Encoding,
    VersionedMessageUnsupported,
    Signature { index: usize },
}

impl From<ReplayError> for DiagnosticError {
    fn from(_: ReplayError) -> Self {
        Self::Encoding
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionRole {
    CoordinatorRequest,
    CoordinatorFulfill,
    ReferenceVerify,
    TestConsumerRequest,
    TestConsumerCallback,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionObservation {
    pub program: [u8; 32],
    pub accounts: Vec<u8>,
    pub data_bytes: usize,
    pub role: InstructionRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountPrivileges {
    pub signer: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedShapeReport {
    pub transaction_sha256: [u8; 32],
    pub message_sha256: [u8; 32],
    pub signatures_verified: usize,
    pub account_keys: Vec<[u8; 32]>,
    pub account_privileges: Vec<AccountPrivileges>,
    pub recent_blockhash: [u8; 32],
    pub writable_accounts: usize,
    pub instructions: Vec<InstructionObservation>,
    /// The frozen parser's result, not signature, execution, or checkpoint admission.
    pub frozen_wire_gate: Result<(), ReplayError>,
}

impl SignedShapeReport {
    pub fn to_text(&self) -> String {
        let mut output = String::new();
        writeln!(output, "schema={PROFILE_ID}").unwrap();
        writeln!(output, "library_revision={LIBRARY_REVISION}").unwrap();
        writeln!(output, "baseline_revision={BASELINE_REVISION}").unwrap();
        output.push_str("execution_verified=false\nvrf_ready=false\nproof_generated=false\n");
        writeln!(
            output,
            "transaction_sha256={}",
            crate::readiness::hex(&self.transaction_sha256)
        )
        .unwrap();
        writeln!(
            output,
            "message_sha256={}",
            crate::readiness::hex(&self.message_sha256)
        )
        .unwrap();
        writeln!(output, "signatures_verified={}", self.signatures_verified).unwrap();
        writeln!(output, "account_keys={}", self.account_keys.len()).unwrap();
        writeln!(output, "writable_accounts={}", self.writable_accounts).unwrap();
        writeln!(output, "frozen_wire_gate={:?}", self.frozen_wire_gate).unwrap();
        output.push_str("signature_scope=message_only\nbank_freshness_verified=false\n");
        output.push_str("role_scope=program_and_discriminator_only\n");
        output.push_str("arguments_verified=false\nnested_calls_observed=false\n");
        output.push_str("runtime_sanitization_verified=false\n");
        writeln!(
            output,
            "recent_blockhash={}",
            crate::readiness::hex(&self.recent_blockhash)
        )
        .unwrap();
        for (index, (key, flags)) in self
            .account_keys
            .iter()
            .zip(&self.account_privileges)
            .enumerate()
        {
            writeln!(
                output,
                "account.{index}={},signer={},writable={}",
                crate::readiness::hex(key),
                flags.signer,
                flags.writable
            )
            .unwrap();
        }
        for (index, instruction) in self.instructions.iter().enumerate() {
            writeln!(
                output,
                "instruction.{index}={:?},program={},accounts={:?},data_bytes={}",
                instruction.role,
                crate::readiness::hex(&instruction.program),
                instruction.accounts,
                instruction.data_bytes
            )
            .unwrap();
        }
        output
    }
}

/// Inspect canonical legacy bytes and verify every message signature independently.
/// Does not infer callback execution from instruction names or accept a new witness.
pub fn inspect_signed_transaction(bytes: &[u8]) -> Result<SignedShapeReport, DiagnosticError> {
    if bytes.len() > MAX_DIAGNOSTIC_TRANSACTION_BYTES {
        return Err(DiagnosticError::ResourceLimit);
    }
    let mut cursor = 0;
    let signature_count = read_short_vec(bytes, &mut cursor)?;
    if signature_count == 0 || signature_count > 127 {
        return Err(DiagnosticError::Encoding);
    }
    let mut signatures = Vec::new();
    for _ in 0..signature_count {
        signatures.push(take_array::<64>(bytes, &mut cursor)?);
    }
    let message_start = cursor;
    let header: [u8; 3] = take_array(bytes, &mut cursor)?;
    if header[0] & 0x80 != 0 {
        return Err(DiagnosticError::VersionedMessageUnsupported);
    }
    let key_count = read_short_vec(bytes, &mut cursor)?;
    let readonly_signed = usize::from(header[1]);
    let readonly_unsigned = usize::from(header[2]);
    if signature_count != usize::from(header[0]) || key_count > 256 {
        return Err(DiagnosticError::Encoding);
    }
    let unsigned_count = key_count
        .checked_sub(signature_count)
        .ok_or(DiagnosticError::Encoding)?;
    let writable_signed = signature_count
        .checked_sub(readonly_signed)
        .filter(|count| *count != 0)
        .ok_or(DiagnosticError::Encoding)?;
    let writable_unsigned = unsigned_count
        .checked_sub(readonly_unsigned)
        .ok_or(DiagnosticError::Encoding)?;
    let unsigned_writable_end = key_count
        .checked_sub(readonly_unsigned)
        .ok_or(DiagnosticError::Encoding)?;
    let writable_accounts = writable_signed
        .checked_add(writable_unsigned)
        .ok_or(DiagnosticError::Encoding)?;
    let mut account_keys = Vec::new();
    for _ in 0..key_count {
        let key = take_array::<32>(bytes, &mut cursor)?;
        if account_keys.contains(&key) {
            return Err(DiagnosticError::Encoding);
        }
        account_keys.push(key);
    }
    let recent_blockhash = take_array(bytes, &mut cursor)?;
    let instruction_count = read_short_vec(bytes, &mut cursor)?;
    let mut instructions = Vec::new();
    for _ in 0..instruction_count {
        let [program_index] = take_array::<1>(bytes, &mut cursor)?;
        let program = *account_keys
            .get(usize::from(program_index))
            .ok_or(DiagnosticError::Encoding)?;
        let account_count = read_short_vec(bytes, &mut cursor)?;
        let accounts = take(bytes, &mut cursor, account_count)?.to_vec();
        if accounts
            .iter()
            .any(|index| usize::from(*index) >= key_count)
        {
            return Err(DiagnosticError::Encoding);
        }
        let data_len = read_short_vec(bytes, &mut cursor)?;
        let data = take(bytes, &mut cursor, data_len)?;
        instructions.push(InstructionObservation {
            program,
            accounts,
            data_bytes: data.len(),
            role: instruction_role(program, data),
        });
    }
    if cursor != bytes.len() {
        return Err(DiagnosticError::Encoding);
    }
    for (index, signature) in signatures.iter().enumerate() {
        let key = VerifyingKey::from_bytes(&account_keys[index])
            .map_err(|_| DiagnosticError::Signature { index })?;
        key.verify_strict(&bytes[message_start..], &Signature::from_bytes(signature))
            .map_err(|_| DiagnosticError::Signature { index })?;
    }
    Ok(SignedShapeReport {
        transaction_sha256: Sha256::digest(bytes).into(),
        message_sha256: Sha256::digest(&bytes[message_start..]).into(),
        signatures_verified: signature_count,
        account_keys,
        account_privileges: (0..key_count)
            .map(|index| AccountPrivileges {
                signer: index < signature_count,
                writable: if index < signature_count {
                    index < writable_signed
                } else {
                    index < unsigned_writable_end
                },
            })
            .collect(),
        recent_blockhash,
        writable_accounts,
        instructions,
        frozen_wire_gate: parse_legacy_transaction(bytes).map(|_| ()),
    })
}

fn instruction_role(program: [u8; 32], data: &[u8]) -> InstructionRole {
    for (id, name, role) in [
        (
            COORDINATOR_ID,
            "request_random",
            InstructionRole::CoordinatorRequest,
        ),
        (
            COORDINATOR_ID,
            "fulfill",
            InstructionRole::CoordinatorFulfill,
        ),
        (
            REFERENCE_VERIFY_ID,
            "verify",
            InstructionRole::ReferenceVerify,
        ),
        (
            TEST_CONSUMER_ID,
            "make_request",
            InstructionRole::TestConsumerRequest,
        ),
        (
            TEST_CONSUMER_ID,
            "on_random",
            InstructionRole::TestConsumerCallback,
        ),
    ] {
        let discriminator = Sha256::digest(format!("global:{name}").as_bytes());
        if program == id && data.get(..8) == Some(&discriminator[..8]) {
            return role;
        }
    }
    InstructionRole::Other
}
