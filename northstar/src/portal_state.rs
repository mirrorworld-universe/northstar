//! Portal program state types for parsing on-chain accounts.
//!
//! The portal program (`northstar-portal`) is `no_std` + pinocchio, using
//! `pinocchio::pubkey::Pubkey`. This module provides validator-side mirrors
//! of the portal's account layouts using `solana_pubkey::Pubkey` and borsh
//! deserialization.

use {borsh::BorshDeserialize, solana_pubkey::Pubkey};

/// Discriminator for Session accounts
pub const SESSION_DISCRIMINATOR: u8 = 1;

/// Discriminator for FeeVault accounts
pub const FEE_VAULT_DISCRIMINATOR: u8 = 2;

/// Discriminator for DelegationRecord accounts
pub const DELEGATION_RECORD_DISCRIMINATOR: u8 = 3;

/// Session account representing an ephemeral rollup session.
///
/// Layout (82 bytes):
/// - discriminator: u8 (offset 0, value 1)
/// - owner: [u8; 32] (offset 1)
/// - grid_id: u64 (offset 33)
/// - ttl_slots: u64 (offset 41)
/// - fee_cap: u64 (offset 49)
/// - created_at: u64 (offset 57)
/// - nonce: u128 (offset 65)
/// - bump: u8 (offset 81)
#[derive(Debug, Clone, BorshDeserialize)]
pub struct Session {
    pub discriminator: u8,
    pub owner: Pubkey,
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
    pub created_at: u64,
    pub nonce: u128,
    pub bump: u8,
}

impl Session {
    /// Fixed size of a Session account
    pub const LEN: usize = 82;

    /// Returns true if the discriminator matches the expected value.
    pub fn is_valid(&self) -> bool {
        self.discriminator == SESSION_DISCRIMINATOR
    }

    /// Returns true if the session has expired given the current slot.
    pub fn is_expired(&self, current_slot: u64) -> bool {
        current_slot > self.created_at.saturating_add(self.ttl_slots)
    }
}

/// FeeVault account for holding fees.
///
/// Layout (42 bytes):
/// - discriminator: u8 (offset 0, value 2)
/// - authority: [u8; 32] (offset 1)
/// - balance: u64 (offset 33)
/// - bump: u8 (offset 41)
#[derive(Debug, Clone, BorshDeserialize)]
pub struct FeeVault {
    pub discriminator: u8,
    pub authority: Pubkey,
    pub balance: u64,
    pub bump: u8,
}

impl FeeVault {
    /// Fixed size of a FeeVault account
    pub const LEN: usize = 42;

    /// Returns true if the discriminator matches the expected value.
    pub fn is_valid(&self) -> bool {
        self.discriminator == FEE_VAULT_DISCRIMINATOR
    }
}

/// DelegationRecord account tracking delegated accounts.
///
/// Layout (42 bytes):
/// - discriminator: u8 (offset 0, value 3)
/// - owner_program: [u8; 32] (offset 1)
/// - grid_id: u64 (offset 33)
/// - bump: u8 (offset 41)
///
/// Note: The `delegated_account` pubkey is NOT stored on-chain.
/// It is derived during scanning by matching the PDA address
/// (PDA seeds are `["delegation", delegated_account]`).
#[derive(Debug, Clone, BorshDeserialize)]
pub struct DelegationRecord {
    pub discriminator: u8,
    pub owner_program: Pubkey,
    pub grid_id: u64,
    pub bump: u8,
}

impl DelegationRecord {
    /// Fixed size of a DelegationRecord account
    pub const LEN: usize = 42;

    /// Returns true if the discriminator matches the expected value.
    pub fn is_valid(&self) -> bool {
        self.discriminator == DELEGATION_RECORD_DISCRIMINATOR
    }
}

/// Enum representing any portal program account type.
#[derive(Debug, Clone)]
pub enum PortalAccount {
    Session(Session),
    FeeVault(FeeVault),
    DelegationRecord(DelegationRecord),
}

/// Attempt to parse a portal-program-owned account's data into a typed
/// representation.
///
/// Returns `None` if:
/// - The data is empty
/// - The discriminator is unrecognized
/// - Deserialization fails
pub fn try_parse_portal_account(data: &[u8]) -> Option<PortalAccount> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        SESSION_DISCRIMINATOR => borsh::from_slice::<Session>(data)
            .ok()
            .map(PortalAccount::Session),
        FEE_VAULT_DISCRIMINATOR => borsh::from_slice::<FeeVault>(data)
            .ok()
            .map(PortalAccount::FeeVault),
        DELEGATION_RECORD_DISCRIMINATOR => borsh::from_slice::<DelegationRecord>(data)
            .ok()
            .map(PortalAccount::DelegationRecord),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to convert a Pubkey to bytes for test serialization
    fn pubkey_to_bytes(pubkey: &Pubkey) -> [u8; 32] {
        pubkey.to_bytes()
    }

    /// Test that Session deserialization works correctly.
    #[test]
    fn test_session_deserialization() {
        // Construct bytes matching portal program's borsh output
        let mut data = vec![SESSION_DISCRIMINATOR]; // discriminator

        // owner: [u8; 32]
        let owner = Pubkey::new_unique();
        data.extend_from_slice(&pubkey_to_bytes(&owner));

        // grid_id: u64
        data.extend_from_slice(&12345_u64.to_le_bytes());

        // ttl_slots: u64
        data.extend_from_slice(&100_u64.to_le_bytes());

        // fee_cap: u64
        data.extend_from_slice(&5000_u64.to_le_bytes());

        // created_at: u64
        data.extend_from_slice(&50_u64.to_le_bytes());

        // nonce: u128
        data.extend_from_slice(&42_u128.to_le_bytes());

        // bump: u8
        data.push(255);

        assert_eq!(data.len(), Session::LEN);

        let session = Session::try_from_slice(&data).unwrap();
        assert!(session.is_valid());
        assert_eq!(session.owner, owner);
        assert_eq!(session.grid_id, 12345);
        assert_eq!(session.ttl_slots, 100);
        assert_eq!(session.fee_cap, 5000);
        assert_eq!(session.created_at, 50);
        assert_eq!(session.nonce, 42);
        assert_eq!(session.bump, 255);
    }

    /// Test that DelegationRecord deserialization works correctly.
    #[test]
    fn test_delegation_record_deserialization() {
        // Construct bytes matching portal program's borsh output
        let mut data = vec![DELEGATION_RECORD_DISCRIMINATOR]; // discriminator

        // owner_program: [u8; 32]
        let owner_program = Pubkey::new_unique();
        data.extend_from_slice(&pubkey_to_bytes(&owner_program));

        // grid_id: u64
        data.extend_from_slice(&999_u64.to_le_bytes());

        // bump: u8
        data.push(128);

        assert_eq!(data.len(), DelegationRecord::LEN);

        let record = DelegationRecord::try_from_slice(&data).unwrap();
        assert!(record.is_valid());
        assert_eq!(record.owner_program, owner_program);
        assert_eq!(record.grid_id, 999);
        assert_eq!(record.bump, 128);
    }

    /// Test that FeeVault deserialization works correctly.
    #[test]
    fn test_fee_vault_deserialization() {
        // Construct bytes matching portal program's borsh output
        let mut data = vec![FEE_VAULT_DISCRIMINATOR]; // discriminator

        // authority: [u8; 32]
        let authority = Pubkey::new_unique();
        data.extend_from_slice(&pubkey_to_bytes(&authority));

        // balance: u64
        data.extend_from_slice(&1_000_000_u64.to_le_bytes());

        // bump: u8
        data.push(77);

        assert_eq!(data.len(), FeeVault::LEN);

        let vault = FeeVault::try_from_slice(&data).unwrap();
        assert!(vault.is_valid());
        assert_eq!(vault.authority, authority);
        assert_eq!(vault.balance, 1_000_000);
        assert_eq!(vault.bump, 77);
    }

    /// Test the try_parse_portal_account dispatch function.
    #[test]
    fn test_try_parse_portal_account() {
        // Test with Session discriminator
        let mut session_data = vec![SESSION_DISCRIMINATOR];
        session_data.extend(vec![0u8; Session::LEN - 1]);
        let parsed = try_parse_portal_account(&session_data);
        assert!(matches!(parsed, Some(PortalAccount::Session(_))));

        // Test with FeeVault discriminator
        let mut fee_vault_data = vec![FEE_VAULT_DISCRIMINATOR];
        fee_vault_data.extend(vec![0u8; FeeVault::LEN - 1]);
        let parsed = try_parse_portal_account(&fee_vault_data);
        assert!(matches!(parsed, Some(PortalAccount::FeeVault(_))));

        // Test with DelegationRecord discriminator
        let mut delegation_data = vec![DELEGATION_RECORD_DISCRIMINATOR];
        delegation_data.extend(vec![0u8; DelegationRecord::LEN - 1]);
        let parsed = try_parse_portal_account(&delegation_data);
        assert!(matches!(parsed, Some(PortalAccount::DelegationRecord(_))));

        // Test with invalid discriminator
        let invalid_data = vec![99u8, 0, 0, 0];
        let parsed = try_parse_portal_account(&invalid_data);
        assert!(parsed.is_none());

        // Test with empty data
        let parsed = try_parse_portal_account(&[]);
        assert!(parsed.is_none());
    }

    /// Test layout compatibility between portal-side and validator-side structs.
    /// This verifies that borsh serialization produces byte-identical output
    /// on both sides (verified by constructing raw bytes and deserializing).
    #[test]
    fn test_layout_compatibility() {
        // Session: discriminator=1, all zeros for other fields
        // Layout: discriminator(1) + owner(32) + grid_id(8) + ttl_slots(8) +
        //         fee_cap(8) + created_at(8) + nonce(16) + bump(1) = 82 bytes
        let mut session_bytes = vec![SESSION_DISCRIMINATOR];
        session_bytes.extend([0u8; 32]); // owner = zeros (32 bytes)
        session_bytes.extend_from_slice(&0_u64.to_le_bytes()); // grid_id
        session_bytes.extend_from_slice(&0_u64.to_le_bytes()); // ttl_slots
        session_bytes.extend_from_slice(&0_u64.to_le_bytes()); // fee_cap
        session_bytes.extend_from_slice(&0_u64.to_le_bytes()); // created_at
        session_bytes.extend_from_slice(&0_u128.to_le_bytes()); // nonce
        session_bytes.push(0); // bump
        assert_eq!(session_bytes.len(), Session::LEN);

        let session = Session::try_from_slice(&session_bytes).unwrap();
        assert!(session.is_valid());
        assert_eq!(session.owner, Pubkey::default());
        assert_eq!(session.grid_id, 0);

        // FeeVault: discriminator=2, all zeros
        // Layout: discriminator(1) + authority(32) + balance(8) + bump(1) = 42 bytes
        let mut fee_vault_bytes = vec![FEE_VAULT_DISCRIMINATOR];
        fee_vault_bytes.extend([0u8; 32]); // authority = zeros
        fee_vault_bytes.extend_from_slice(&0_u64.to_le_bytes()); // balance
        fee_vault_bytes.push(0); // bump
        assert_eq!(fee_vault_bytes.len(), FeeVault::LEN);

        let vault = FeeVault::try_from_slice(&fee_vault_bytes).unwrap();
        assert!(vault.is_valid());
        assert_eq!(vault.authority, Pubkey::default());
        assert_eq!(vault.balance, 0);

        // DelegationRecord: discriminator=3, all zeros
        // Layout: discriminator(1) + owner_program(32) + grid_id(8) + bump(1) = 42 bytes
        let mut delegation_bytes = vec![DELEGATION_RECORD_DISCRIMINATOR];
        delegation_bytes.extend([0u8; 32]); // owner_program = zeros
        delegation_bytes.extend_from_slice(&0_u64.to_le_bytes()); // grid_id
        delegation_bytes.push(0); // bump
        assert_eq!(delegation_bytes.len(), DelegationRecord::LEN);

        let record = DelegationRecord::try_from_slice(&delegation_bytes).unwrap();
        assert!(record.is_valid());
        assert_eq!(record.owner_program, Pubkey::default());
        assert_eq!(record.grid_id, 0);
    }
}
