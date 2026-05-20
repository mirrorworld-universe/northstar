use borsh::{BorshDeserialize, BorshSerialize};

#[cfg_attr(feature = "idl", derive(shank::ShankInstruction))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
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
    Delegate { grid_id: u64 },

    #[cfg_attr(feature = "idl", account(0, name = "authority", sig, mut))]
    #[cfg_attr(feature = "idl", account(1, name = "delegated_account", mut))]
    #[cfg_attr(feature = "idl", account(2, name = "owner_program"))]
    #[cfg_attr(feature = "idl", account(3, name = "delegation_record", mut))]
    #[cfg_attr(feature = "idl", account(4, name = "system_program"))]
    Undelegate,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct OpenSession {
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
}
