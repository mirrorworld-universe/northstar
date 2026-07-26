use {
    crate::{MAX_SETTLEMENT_CHUNK, MAX_SETTLEMENT_LAMPORT_ACCOUNTS, MAX_STEP_PROOF_CHUNK},
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::pubkey::Pubkey,
};

#[cfg_attr(feature = "idl", derive(shank::ShankInstruction))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
#[allow(clippy::large_enum_variant)]
pub enum PortalInstruction {
    #[cfg_attr(feature = "idl", account(0, name = "payer", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "fee_vault", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "system_program"))]
    OpenSession(OpenSession),

    #[cfg_attr(feature = "idl", account(0, name = "closer", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "fee_vault", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "system_program"))]
    #[cfg_attr(feature = "idl", account(4, name = "checkpoint_cursor", mut))]
    CloseSession,

    #[cfg_attr(feature = "idl", account(0, name = "depositor", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "deposit_receipt", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "recipient"))]
    #[cfg_attr(feature = "idl", account(4, name = "system_program"))]
    DepositFee { lamports: u64 },

    #[cfg_attr(feature = "idl", account(0, name = "payer", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "system_program"))]
    #[cfg_attr(feature = "idl", account(2, name = "delegated_account", sig, mut))]
    #[cfg_attr(feature = "idl", account(3, name = "owner_program"))]
    #[cfg_attr(feature = "idl", account(4, name = "delegation_record", mut))]
    #[cfg_attr(feature = "idl", account(5, name = "buffer"))]
    #[cfg_attr(feature = "idl", account(6, name = "session"))]
    Delegate { grid_id: u64 },

    #[cfg_attr(feature = "idl", account(0, name = "authority", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "delegated_account", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "owner_program"))]
    #[cfg_attr(feature = "idl", account(3, name = "delegation_record", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "system_program"))]
    #[cfg_attr(feature = "idl", account(5, name = "session"))]
    Undelegate,

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint"))]
    BeginSettlement(BeginSettlement),

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "delegated_account", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "delegation_record"))]
    WriteSettlementChunk(WriteSettlementChunk),

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "checkpoint_cursor", mut))]
    FinishSettlement(FinishSettlement),

    #[cfg_attr(feature = "idl", account(0, name = "authority_or_validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    AbortSettlement,

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "deposit_receipt", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "er_source"))]
    #[cfg_attr(feature = "idl", account(4, name = "l1_recipient", mut))]
    SettleDepositReceipt(SettleDepositReceipt),

    #[cfg_attr(feature = "idl", account(0, name = "authority", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "delegated_account", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "owner_program"))]
    #[cfg_attr(feature = "idl", account(3, name = "delegation_record", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "system_program"))]
    #[cfg_attr(feature = "idl", account(5, name = "session"))]
    UndelegateHandoff,

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "delegated_account", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "delegation_record", mut))]
    SettleAccountOwner(SettleAccountOwner),

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    SettleAccountLamports(SettleAccountLamports),

    #[cfg_attr(feature = "idl", account(0, name = "source", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "l1_recipient"))]
    #[cfg_attr(feature = "idl", account(2, name = "withdrawal_sink", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "system_program"))]
    #[cfg_attr(feature = "idl", account(4, name = "clock"))]
    StartWithdrawal { lamports: u64 },

    #[cfg_attr(feature = "idl", account(0, name = "proposer", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "checkpoint_cursor", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "system_program"))]
    ProposeCheckpoint(ProposeCheckpoint),

    #[cfg_attr(feature = "idl", account(0, name = "committer", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "checkpoint_cursor", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "proposer", mut))]
    CommitCheckpoint(CommitCheckpoint),

    #[cfg_attr(feature = "idl", account(0, name = "proposer", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "checkpoint_cursor", mut))]
    CancelCheckpoint(CancelCheckpoint),

    #[cfg_attr(feature = "idl", account(0, name = "challenger", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "challenge", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "da_proof", mut))]
    #[cfg_attr(feature = "idl", account(5, name = "system_program"))]
    OpenChallenge(OpenChallenge),

    #[cfg_attr(feature = "idl", account(0, name = "authority", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint"))]
    #[cfg_attr(feature = "idl", account(3, name = "challenge"))]
    #[cfg_attr(feature = "idl", account(4, name = "step_proof", mut))]
    #[cfg_attr(feature = "idl", account(5, name = "system_program"))]
    CreateStepProof(CreateStepProof),

    #[cfg_attr(feature = "idl", account(0, name = "authority", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint"))]
    #[cfg_attr(feature = "idl", account(3, name = "challenge"))]
    #[cfg_attr(feature = "idl", account(4, name = "step_proof", mut))]
    WriteStepProof(WriteStepProof),

    #[cfg_attr(feature = "idl", account(0, name = "authority", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint"))]
    #[cfg_attr(feature = "idl", account(3, name = "challenge"))]
    #[cfg_attr(feature = "idl", account(4, name = "step_proof", mut))]
    SealStepProof(SealStepProof),

    #[cfg_attr(feature = "idl", account(0, name = "submitter", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "challenge", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "da_proof"))]
    #[cfg_attr(feature = "idl", account(5, name = "step_proof"))]
    #[cfg_attr(feature = "idl", account(6, name = "bond_recipient", mut))]
    #[cfg_attr(feature = "idl", account(7, name = "checkpoint_cursor", mut))]
    ResolveChallenge(ResolveChallenge),

    #[cfg_attr(feature = "idl", account(0, name = "authority", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "session_bridge", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "system_program"))]
    RegisterSessionBridge(RegisterSessionBridge),

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint"))]
    #[cfg_attr(feature = "idl", account(3, name = "challenge", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "da_proof", mut))]
    RespondChallenge(RespondChallenge),

    #[cfg_attr(feature = "idl", account(0, name = "challenger", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint"))]
    #[cfg_attr(feature = "idl", account(3, name = "challenge", mut))]
    BisectChallenge(BisectChallenge),

    #[cfg_attr(feature = "idl", account(0, name = "caller", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session"))]
    #[cfg_attr(feature = "idl", account(2, name = "checkpoint", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "challenge", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "da_proof", mut))]
    #[cfg_attr(feature = "idl", account(5, name = "bond_recipient", mut))]
    #[cfg_attr(feature = "idl", account(6, name = "checkpoint_cursor", mut))]
    TimeoutChallenge(TimeoutChallenge),
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct OpenSession {
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
    pub validator: Pubkey,
    pub settlement_interval_slots: u64,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct BeginSettlement {
    pub er_slot: u64,
    pub checksum: [u8; 32],
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct WriteSettlementChunk {
    pub er_slot: u64,
    pub checksum: [u8; 32],
    pub account_data_offset: u32,
    pub chunk_len: u16,
    pub chunk: [u8; MAX_SETTLEMENT_CHUNK],
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct FinishSettlement {
    pub er_slot: u64,
    pub checksum: [u8; 32],
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SettleDepositReceipt {
    pub er_slot: u64,
    pub checksum: [u8; 32],
    pub balance: u64,
    pub withdrawn: u64,
    pub payout_lamports: u64,
    pub l1_recipient: Pubkey,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SettleAccountOwner {
    pub er_slot: u64,
    pub checksum: [u8; 32],
    pub owner: Pubkey,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SettleAccountLamports {
    pub er_slot: u64,
    pub checksum: [u8; 32],
    pub account_count: u8,
    pub lamports: [u64; MAX_SETTLEMENT_LAMPORT_ACCOUNTS],
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ProposeCheckpoint {
    pub er_slot: u64,
    pub step_count: u64,
    pub previous_state_root: [u8; 32],
    pub new_state_root: [u8; 32],
    pub trace_root: [u8; 32],
    pub tx_effect_root: [u8; 32],
    pub readonly_l1_root: [u8; 32],
    pub da_commitment: [u8; 32],
    pub effect_commitment: [u8; 32],
    pub challenge_window_slots: u64,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct CommitCheckpoint {
    pub er_slot: u64,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct CancelCheckpoint {
    pub er_slot: u64,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct OpenChallenge {
    pub er_slot: u64,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct CreateStepProof {
    pub er_slot: u64,
    pub proof_kind: u8,
    pub proof_version: u8,
    pub step_index: u64,
    pub tx_effect_root: [u8; 32],
    pub readonly_l1_root: [u8; 32],
    pub settlement_effect_root: [u8; 32],
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct WriteStepProof {
    pub er_slot: u64,
    pub offset: u32,
    pub chunk_len: u16,
    pub chunk: [u8; MAX_STEP_PROOF_CHUNK],
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SealStepProof {
    pub er_slot: u64,
    pub proof_len: u32,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum StepProofVerifierMode {
    Production = 0,
    TestOnly = 1,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ResolveChallenge {
    pub er_slot: u64,
    pub verifier_mode: StepProofVerifierMode,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct RegisterSessionBridge {
    pub mint: Pubkey,
    pub bridge_program: Pubkey,
    pub vault: Pubkey,
    pub token_program: Pubkey,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct RespondChallenge {
    pub er_slot: u64,
    pub claimed_step: u64,
    pub claimed_state_root: [u8; 32],
    pub da_payload_root: [u8; 32],
    pub da_inclusion_proof_hash: [u8; 32],
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct BisectChallenge {
    pub er_slot: u64,
    pub dispute_upper: bool,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct TimeoutChallenge {
    pub er_slot: u64,
}
