use borsh::{BorshDeserialize, BorshSerialize};

pub const SESSION_DISCRIMINATOR: u8 = 1;
pub const FEE_VAULT_DISCRIMINATOR: u8 = 2;
pub const DELEGATION_RECORD_DISCRIMINATOR: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Session {
    pub discriminator: u8,
    pub owner: [u8; 32],
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
    pub created_at: u64,
    pub nonce: u128,
    pub bump: u8,
}

impl Session {
    pub const LEN: usize = 82;
    pub const SEED_PREFIX: &[u8] = b"session";

    #[inline]
    pub fn is_expired(&self, current_slot: u64) -> bool {
        current_slot > self.created_at.saturating_add(self.ttl_slots)
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == SESSION_DISCRIMINATOR
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct FeeVault {
    pub discriminator: u8,
    pub authority: [u8; 32],
    pub balance: u64,
    pub bump: u8,
}

impl FeeVault {
    pub const LEN: usize = 42;
    pub const SEED_PREFIX: &[u8] = b"fee_vault";

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == FEE_VAULT_DISCRIMINATOR
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DelegationRecord {
    pub discriminator: u8,
    pub owner_program: [u8; 32],
    pub grid_id: u64,
    pub bump: u8,
}

impl DelegationRecord {
    pub const LEN: usize = 42;
    pub const SEED_PREFIX: &[u8] = b"delegation";

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == DELEGATION_RECORD_DISCRIMINATOR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_len() {
        let session = Session {
            discriminator: SESSION_DISCRIMINATOR,
            owner: [0x42; 32],
            grid_id: 123,
            ttl_slots: 1000,
            fee_cap: 5000,
            nonce: 999,
            created_at: 100,
            bump: 255,
        };
        let serialized = borsh::to_vec(&session).unwrap();
        assert_eq!(serialized.len(), Session::LEN);
    }

    #[test]
    fn test_fee_vault_len() {
        let vault = FeeVault {
            discriminator: FEE_VAULT_DISCRIMINATOR,
            authority: [0xAB; 32],
            balance: 1_000_000,
            bump: 128,
        };
        let serialized = borsh::to_vec(&vault).unwrap();
        assert_eq!(serialized.len(), FeeVault::LEN);
    }

    #[test]
    fn test_delegation_record_len() {
        let record = DelegationRecord {
            discriminator: DELEGATION_RECORD_DISCRIMINATOR,
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
            discriminator: SESSION_DISCRIMINATOR,
            owner: [0; 32],
            grid_id: 1,
            ttl_slots: 100,
            fee_cap: 1000,
            nonce: 0,
            created_at: 50,
            bump: 1,
        };

        assert!(!session.is_expired(100));
        assert!(!session.is_expired(149));
        assert!(session.is_expired(151));
    }
}
