use {
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::pubkey::Pubkey,
};

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub enum PortalInstruction {
    OpenSession(OpenSession),
    CloseSession { grid_id: u64 },
    DepositFee { lamports: u64 },
    Delegate { grid_id: u64 },
    Undelegate,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct OpenSession {
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
    pub owner: Pubkey,
}
