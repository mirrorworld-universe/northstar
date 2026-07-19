use borsh::{BorshDeserialize, BorshSerialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TokenVault {
    pub discriminator: u8,
    pub session_bridge: [u8; 32],
    pub mint: [u8; 32],
    pub vault_token_account: [u8; 32],
    pub token_program: [u8; 32],
    pub deposited: u64,
    pub withdrawn: u64,
    pub bump: u8,
}

impl TokenVault {
    pub const DISCRIMINATOR: u8 = 1;
    pub const LEN: usize = 146;
    pub const SEED_PREFIX: &'static [u8] = b"token_vault";

    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ErTokenAccount {
    pub discriminator: u8,
    pub session_bridge: [u8; 32],
    pub owner: [u8; 32],
    pub mint: [u8; 32],
    pub amount: u64,
    pub bump: u8,
}

impl ErTokenAccount {
    pub const DISCRIMINATOR: u8 = 2;
    pub const LEN: usize = 106;
    pub const SEED_PREFIX: &'static [u8] = b"er_token";

    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TokenDepositReceipt {
    pub discriminator: u8,
    pub session_bridge: [u8; 32],
    pub er_token_account: [u8; 32],
    pub balance: u64,
    pub withdrawn: u64,
    pub bump: u8,
}

impl TokenDepositReceipt {
    pub const DISCRIMINATOR: u8 = 3;
    pub const LEN: usize = 82;
    pub const SEED_PREFIX: &'static [u8] = b"token_deposit";

    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

pub struct BridgeBuffer;

impl BridgeBuffer {
    pub const SEED_PREFIX: &'static [u8] = b"northstar-token-buffer";
}
