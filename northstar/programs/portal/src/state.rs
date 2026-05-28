use {
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::pubkey::Pubkey,
};

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Session {
    pub discriminator: u8,
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
    pub created_at: u64,
    pub nonce: u128,
    pub authority: Pubkey,
    pub validator: Pubkey,
    pub settlement_interval_slots: u64,
    pub last_settled_l1_slot: u64,
    pub last_settled_er_slot: u64,
    pub settlement_status: SettlementStatus,
    pub settlement_er_slot: u64,
    pub settlement_checksum: [u8; 32],
    pub settlement_accumulator: [u8; 32],
    pub settlement_started_l1_slot: u64,
    pub bump: u8,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum SettlementStatus {
    Idle = 0,
    InProgress = 1,
}

impl Session {
    pub const LEN: usize = 219;
    pub const SEED_PREFIX: &[u8] = b"session";
    pub const DISCRIMINATOR: u8 = 1;

    #[inline]
    pub fn is_expired(&self, current_slot: u64) -> bool {
        current_slot > self.created_at.saturating_add(self.ttl_slots)
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct FeeVault {
    pub discriminator: u8,
    pub authority: [u8; 32],
    pub bump: u8,
}

impl FeeVault {
    pub const LEN: usize = 34; // 1 + 32 + 1
    pub const SEED_PREFIX: &[u8] = b"fee_vault";
    pub const DISCRIMINATOR: u8 = 2;

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DelegationRecord {
    pub discriminator: u8,
    pub owner_program: Pubkey,
    pub grid_id: u64,
    pub bump: u8,
}

impl DelegationRecord {
    pub const LEN: usize = 42;
    pub const SEED_PREFIX: &[u8] = b"delegation";
    pub const DISCRIMINATOR: u8 = 3;

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DepositReceipt {
    pub discriminator: u8,
    pub session: Pubkey,
    pub recipient: Pubkey,
    pub balance: u64,
    pub bump: u8,
}

impl DepositReceipt {
    pub const LEN: usize = 74; // 1 + 32 + 32 + 8 + 1
    pub const SEED_PREFIX: &[u8] = b"deposit_receipt";
    pub const DISCRIMINATOR: u8 = 4;

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_len() {
        let session = Session {
            discriminator: Session::DISCRIMINATOR,
            grid_id: 123,
            ttl_slots: 1000,
            fee_cap: 5000,
            nonce: 999,
            created_at: 100,
            authority: [1; 32],
            validator: [2; 32],
            settlement_interval_slots: 42,
            last_settled_l1_slot: 100,
            last_settled_er_slot: 0,
            settlement_status: SettlementStatus::Idle,
            settlement_er_slot: 0,
            settlement_checksum: [0; 32],
            settlement_accumulator: [0; 32],
            settlement_started_l1_slot: 0,
            bump: 255,
        };
        let serialized = borsh::to_vec(&session).unwrap();
        assert_eq!(serialized.len(), Session::LEN);
    }

    #[test]
    fn test_fee_vault_len() {
        let vault = FeeVault {
            discriminator: FeeVault::DISCRIMINATOR,
            authority: [0xAB; 32],
            bump: 128,
        };
        let serialized = borsh::to_vec(&vault).unwrap();
        assert_eq!(serialized.len(), FeeVault::LEN);
    }

    #[test]
    fn test_deposit_receipt_len() {
        let receipt = DepositReceipt {
            discriminator: DepositReceipt::DISCRIMINATOR,
            session: [0x11; 32],
            recipient: [0x22; 32],
            balance: 1_000_000_000,
            bump: 77,
        };
        let serialized = borsh::to_vec(&receipt).unwrap();
        assert_eq!(serialized.len(), DepositReceipt::LEN);
    }

    #[test]
    fn test_delegation_record_len() {
        let record = DelegationRecord {
            discriminator: DelegationRecord::DISCRIMINATOR,
            owner_program: [0xDE; 32],
            grid_id: 456,
            bump: 77,
        };
        let serialized = borsh::to_vec(&record).unwrap();
        assert_eq!(serialized.len(), DelegationRecord::LEN);
    }

    #[test]
    fn test_session_is_expired() {
        let session = Session {
            discriminator: Session::DISCRIMINATOR,
            grid_id: 1,
            ttl_slots: 100,
            fee_cap: 1000,
            nonce: 0,
            created_at: 50,
            authority: [1; 32],
            validator: [2; 32],
            settlement_interval_slots: 10,
            last_settled_l1_slot: 50,
            last_settled_er_slot: 0,
            settlement_status: SettlementStatus::Idle,
            settlement_er_slot: 0,
            settlement_checksum: [0; 32],
            settlement_accumulator: [0; 32],
            settlement_started_l1_slot: 0,
            bump: 1,
        };

        assert!(!session.is_expired(100));
        assert!(!session.is_expired(149));
        assert!(session.is_expired(151));
    }
}
