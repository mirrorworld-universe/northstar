//! Versioned development-only input/bindings for actual bounded interpreter replay.
//! This module contains no interpreter and never treats a supplied output as execution.

use {
    crate::checkpoint::StateAccountValueV1,
    borsh::{
        io::{self, Read, Write},
        BorshDeserialize, BorshSerialize,
    },
    std::ops::Deref,
};

mod binding;
mod ids;
mod wire;
pub use {
    binding::*,
    ids::*,
    wire::{admit_signed_transaction, CandidateInstructionV1, CandidateSignedTransactionV1},
};

pub const CANDIDATE_MAGIC: [u8; 8] = *b"NSVRF001";
pub const CANDIDATE_VERSION: u16 = 1;
pub const CANDIDATE_PROOF_KIND: u8 = 3;
pub const CANDIDATE_PROOF_VERSION: u8 = 1;
pub const MAX_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ACCOUNT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ACCOUNTS: usize = 64;
pub const MAX_STATE_ACCOUNTS: usize = 1024;
pub const MAX_FEATURES: usize = 1024;
pub const MAX_TRANSACTION_BYTES: usize = 1232;
pub const MAX_COMPUTE_UNITS: u64 = 1_400_000;
pub const UPGRADEABLE_PROGRAMDATA_METADATA_BYTES: usize = 45;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedVec<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    pub fn new(values: Vec<T>) -> Result<Self, CandidateError> {
        if values.len() > MAX {
            return Err(CandidateError::ResourceLimit);
        }
        Ok(Self(values))
    }
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}
impl<T, const MAX: usize> Default for BoundedVec<T, MAX> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl<T, const MAX: usize> Deref for BoundedVec<T, MAX> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T: BorshSerialize, const MAX: usize> BorshSerialize for BoundedVec<T, MAX> {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.0.serialize(writer)
    }
}
impl<T: BorshDeserialize, const MAX: usize> BorshDeserialize for BoundedVec<T, MAX> {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let encoded_length = u32::deserialize_reader(reader)?;
        let length = encoded_length as usize;
        if length > MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "candidate vector bound",
            ));
        }
        if let Some(values) = T::vec_from_reader(encoded_length, reader)? {
            return Ok(Self(values));
        }
        // Do not reserve from an untrusted count before reading its elements.
        let mut values = Vec::new();
        for _ in 0..length {
            values.push(T::deserialize_reader(reader)?);
        }
        Ok(Self(values))
    }
}

pub type Hash32 = [u8; 32];
pub type AccountBytes = BoundedVec<u8, MAX_ACCOUNT_BYTES>;

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateAccountStateV1 {
    pub lamports: u64,
    pub owner: Hash32,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data: AccountBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateAccountInputV1 {
    pub key: Hash32,
    pub state: Option<CandidateAccountStateV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateProgramV1 {
    pub program_id: Hash32,
    pub programdata_id: Hash32,
    pub loader_id: Hash32,
    pub sbpf_version: u8,
    pub elf_sha256: Hash32,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateFeatureV1 {
    pub id: Hash32,
    pub activation_slot: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateProfileV1 {
    pub version: u16,
    pub library_revision: [u8; 20],
    pub runtime_revision: [u8; 20],
    pub agave_revision: [u8; 20],
    pub program_identities_sha256: Hash32,
    pub active_features: BoundedVec<CandidateFeatureV1, MAX_FEATURES>,
    pub inactive_features: BoundedVec<Hash32, MAX_FEATURES>,
    pub vm_config_sha256: Hash32,
    pub syscall_registry_sha256: Hash32,
    pub compute_budget_sha256: Hash32,
    /// Fixed by the guest-reviewed profile. Portal must require the same count.
    pub checkpoint_steps: u32,
    pub lamports_per_signature: u64,
    pub max_compute_units: u64,
    pub heap_bytes: u32,
    pub max_processing_age: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateRecentBlockhashV1 {
    pub blockhash: Hash32,
    pub age: u64,
    pub lamports_per_signature: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateRuntimeV1 {
    pub slot: u64,
    pub observation_slot: u64,
    pub recent_blockhashes: BoundedVec<CandidateRecentBlockhashV1, 2048>,
    pub compute_unit_limit: u64,
    pub heap_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateExecutionInputV1 {
    pub session_context: BoundedVec<u8, 149>,
    pub er_slot: u64,
    pub step_index: u32,
    pub transaction_bytes: BoundedVec<u8, MAX_TRANSACTION_BYTES>,
    pub profile: CandidateProfileV1,
    pub runtime: CandidateRuntimeV1,
    /// Strict key order, including explicit absence. Privileges derive from wire.
    pub accounts: BoundedVec<CandidateAccountInputV1, MAX_ACCOUNTS>,
    /// Strict program-ID order; all three pinned programs and authenticated loader bytes.
    pub programs: BoundedVec<CandidateProgramV1, 3>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum CandidateExecutionStatusV1 {
    Success,
    InstructionFailure {
        instruction_index: u16,
        /// Candidate v1 code 1 means InstructionError::Custom. This is not a
        /// legacy serialized InstructionError discriminant; other kinds reject.
        error_kind: u32,
        custom_error: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateReturnDataV1 {
    pub program_id: Hash32,
    pub data: BoundedVec<u8, 1024>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateExecutionOutputV1 {
    pub status: CandidateExecutionStatusV1,
    /// Complete key map, same order/keys as input. None represents canonical deletion.
    pub accounts: BoundedVec<CandidateAccountInputV1, MAX_ACCOUNTS>,
    pub executed_units: u64,
    pub loaded_accounts_data_size: u64,
    pub transaction_fee: u64,
    pub prioritization_fee: u64,
    pub return_data: Option<CandidateReturnDataV1>,
    pub log_commitment: Hash32,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateMerklePathV1 {
    pub leaf_index: u32,
    pub leaf_count: u32,
    pub siblings: BoundedVec<Hash32, 4>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateCheckpointV1 {
    /// Complete canonical ER state sets; input absence is checked against these sets.
    pub pre_state: BoundedVec<StateAccountValueV1, MAX_STATE_ACCOUNTS>,
    pub post_state: BoundedVec<StateAccountValueV1, MAX_STATE_ACCOUNTS>,
    pub transaction_effect_path: CandidateMerklePathV1,
    pub transaction_effect_root: Hash32,
    pub readonly_context_root: Hash32,
    pub readonly_context_path: CandidateMerklePathV1,
    pub settlement_effect_root: Hash32,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CandidateEnvelopeV1 {
    pub magic: [u8; 8],
    pub version: u16,
    pub proof_kind: u8,
    pub proof_version: u8,
    pub input: CandidateExecutionInputV1,
    pub expected_output: CandidateExecutionOutputV1,
    pub checkpoint: CandidateCheckpointV1,
}

/// Must be constructed from guest-reviewed constants, not deserialized from witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidatePolicyV1 {
    pub profile_sha256: Hash32,
    pub programs: [CandidateProgramV1; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateUnsupported {
    VersionedMessage,
    TransactionShape,
    FeePolicy,
    AccountLifecycle,
    Program,
    Runtime,
    CrossLayerSettlement,
    Engine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateError {
    Encoding,
    ResourceLimit,
    Domain,
    Wire,
    Signature,
    Profile,
    Runtime,
    Accounts,
    Program,
    Outcome,
    Checkpoint,
    Unsupported(CandidateUnsupported),
}

/// The approved guest wires an actual interpreter implementation here. Implementations
/// supplied by a host caller are not execution proofs and cannot select a guest key.
pub trait CandidateReplayEngine {
    fn execute(
        &mut self,
        input: &CandidateExecutionInputV1,
    ) -> Result<CandidateExecutionOutputV1, CandidateError>;
}
