use {
    crate::Hash32,
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::pubkey::Pubkey,
};

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
#[allow(clippy::large_enum_variant)]
pub enum PortalInstruction {
    OpenSession(OpenSession),
    CloseSession,
    DepositFee {
        lamports: u64,
    },
    Delegate {
        grid_id: u64,
    },
    Undelegate,
    BeginSettlement(BeginSettlement),
    WriteSettlementChunk(WriteSettlementChunk),
    FinishSettlement(FinishSettlement),
    AbortSettlement,
    SettleDepositReceipt(SettleDepositReceipt),
    UndelegateHandoff,
    SettleAccountOwner(SettleAccountOwner),
    SettleAccountLamports(SettleAccountLamports),
    StartWithdrawal {
        lamports: u64,
    },
    ProposeCheckpoint(ProposeCheckpoint),
    CommitCheckpoint(CommitCheckpoint),
    CancelCheckpoint(CancelCheckpoint),
    OpenChallenge(OpenChallenge),
    CreateStepProof(CreateStepProof),
    WriteStepProof(WriteStepProof),
    SealStepProof(SealStepProof),
    ResolveChallenge(ResolveChallenge),
    RegisterSessionBridge(RegisterSessionBridge),
    RespondChallenge(RespondChallenge),
    BisectChallenge(BisectChallenge),
    TimeoutChallenge(TimeoutChallenge),
    #[cfg(feature = "zk-verifier-prototype")]
    VerifyErStepProofV1(VerifyErStepProofV1),
}
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct OpenSession {
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
    pub validator: Pubkey,
    pub settlement_interval_slots: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct BeginSettlement {
    pub er_slot: u64,
    pub checksum: Hash32,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct WriteSettlementChunk {
    pub er_slot: u64,
    pub checksum: Hash32,
    pub account_data_offset: u32,
    pub chunk_len: u16,
    // IDL schemas require literal array lengths.
    pub chunk: [u8; 700],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct FinishSettlement {
    pub er_slot: u64,
    pub checksum: Hash32,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SettleDepositReceipt {
    pub er_slot: u64,
    pub checksum: Hash32,
    pub balance: u64,
    pub withdrawn: u64,
    pub payout_lamports: u64,
    pub l1_recipient: Pubkey,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SettleAccountOwner {
    pub er_slot: u64,
    pub checksum: Hash32,
    pub owner: Pubkey,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SettleAccountLamports {
    pub er_slot: u64,
    pub checksum: Hash32,
    pub account_count: u8,
    // IDL schemas require literal array lengths.
    pub lamports: [u64; 7],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ProposeCheckpoint {
    pub er_slot: u64,
    pub step_count: u64,
    pub previous_state_root: Hash32,
    pub new_state_root: Hash32,
    pub trace_root: Hash32,
    pub tx_effect_root: Hash32,
    pub readonly_l1_root: Hash32,
    pub da_commitment: Hash32,
    pub effect_commitment: Hash32,
    pub challenge_window_slots: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct CommitCheckpoint {
    pub er_slot: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct CancelCheckpoint {
    pub er_slot: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct OpenChallenge {
    pub er_slot: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct CreateStepProof {
    pub er_slot: u64,
    pub proof_kind: u8,
    pub proof_version: u8,
    pub step_index: u64,
    pub tx_effect_root: Hash32,
    pub readonly_l1_root: Hash32,
    pub settlement_effect_root: Hash32,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct WriteStepProof {
    pub er_slot: u64,
    pub offset: u32,
    pub chunk_len: u16,
    // IDL schemas require literal array lengths.
    pub chunk: [u8; 128],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SealStepProof {
    pub er_slot: u64,
    pub proof_len: u32,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum StepProofVerifierMode {
    Production = 0,
    TestOnly = 1,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ResolveChallenge {
    pub er_slot: u64,
    pub verifier_mode: StepProofVerifierMode,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct RegisterSessionBridge {
    pub mint: Pubkey,
    pub bridge_program: Pubkey,
    pub vault: Pubkey,
    pub token_program: Pubkey,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct RespondChallenge {
    pub er_slot: u64,
    pub claimed_step: u64,
    pub claimed_state_root: Hash32,
    pub da_payload_root: Hash32,
    pub da_inclusion_proof_hash: Hash32,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct BisectChallenge {
    pub er_slot: u64,
    pub dispute_upper: bool,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct TimeoutChallenge {
    pub er_slot: u64,
}

#[cfg(feature = "zk-verifier-prototype")]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct VerifyErStepProofV1 {
    pub proof: [u8; 256],
    pub public_inputs: [u8; 256],
}
