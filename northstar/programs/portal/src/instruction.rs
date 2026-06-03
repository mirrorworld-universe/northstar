use {
    crate::MAX_SETTLEMENT_CHUNK,
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
    BeginSettlement(BeginSettlement),

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "delegated_account", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "delegation_record"))]
    WriteSettlementChunk(WriteSettlementChunk),

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    FinishSettlement(FinishSettlement),

    #[cfg_attr(feature = "idl", account(0, name = "authority_or_validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    AbortSettlement,

    #[cfg_attr(feature = "idl", account(0, name = "validator", sig))]
    #[cfg_attr(feature = "idl", account(1, name = "session", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "deposit_receipt", mut))]
    #[cfg_attr(feature = "idl", account(3, name = "recipient", mut))]
    SettleDepositReceipt(SettleDepositReceipt),

    #[cfg_attr(feature = "idl", account(0, name = "authority", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "delegated_account", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "owner_program"))]
    #[cfg_attr(feature = "idl", account(3, name = "delegation_record", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "system_program"))]
    #[cfg_attr(feature = "idl", account(5, name = "session"))]
    UndelegateHandoff,
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
}
