use {
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::pubkey::Pubkey,
};

pub fn account_size<T: BorshSerialize + ?Sized>(account: &T) -> usize {
    borsh::object_length(account).expect("account serialization length overflow")
}

/// Fixed ER account that holds withdrawn SOL until L1 settlement.
pub const WITHDRAWAL_SINK: Pubkey = [
    0x05, 0x7d, 0x77, 0xa2, 0x13, 0x37, 0xb6, 0x2d, 0xb7, 0x7d, 0xba, 0x7e, 0x26, 0xf8, 0xe1, 0x47,
    0x06, 0x35, 0xbd, 0x36, 0x53, 0x76, 0xa6, 0x7d, 0x7f, 0xf5, 0xa1, 0x82, 0x3e, 0xe0, 0x8f, 0xb8,
];

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

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Checkpoint {
    pub discriminator: u8,
    pub session: Pubkey,
    pub er_slot: u64,
    pub step_count: u64,
    pub previous_state_root: [u8; 32],
    pub new_state_root: [u8; 32],
    pub trace_root: [u8; 32],
    pub tx_effect_root: [u8; 32],
    pub readonly_l1_root: [u8; 32],
    pub da_commitment: [u8; 32],
    pub effect_commitment: [u8; 32],
    pub proposer: Pubkey,
    pub proposed_at_l1_slot: u64,
    pub challenge_deadline_l1_slot: u64,
    pub status: CheckpointStatus,
    pub bond_lamports: u64,
    pub bond_status: CheckpointBondStatus,
    pub challenger: Pubkey,
    pub challenged_at_l1_slot: u64,
    pub challenge_resolved: bool,
    pub bump: u8,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum CheckpointStatus {
    Pending = 0,
    Committed = 1,
    Cancelled = 2,
    Settled = 3,
    Challenged = 4,
    Invalid = 5,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum CheckpointBondStatus {
    Locked = 0,
    Released = 1,
    Slashed = 2,
}

impl Checkpoint {
    pub const SEED_PREFIX: &[u8] = b"checkpoint";
    pub const DISCRIMINATOR: u8 = 5;

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CheckpointCursor {
    pub discriminator: u8,
    pub session: Pubkey,
    pub latest_finalized_checkpoint: Pubkey,
    pub latest_finalized_er_slot: u64,
    pub latest_finalized_state_root: [u8; 32],
    pub active_checkpoint: Pubkey,
    pub active_er_slot: u64,
    pub bump: u8,
}

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Challenge {
    pub discriminator: u8,
    pub checkpoint: Pubkey,
    pub challenger: Pubkey,
    pub respondent: Pubkey,
    pub opened_at_l1_slot: u64,
    pub hard_deadline_l1_slot: u64,
    pub turn_deadline_l1_slot: u64,
    pub start_step: u64,
    pub end_step: u64,
    pub midpoint_step: u64,
    pub start_state_root: [u8; 32],
    pub end_state_root: [u8; 32],
    pub midpoint_state_root: [u8; 32],
    pub status: ChallengeStatus,
    pub turn: ChallengeTurn,
    pub rounds: u32,
    pub bump: u8,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum ChallengeStatus {
    Active = 0,
    ChallengerWon = 1,
    ValidatorWon = 2,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum ChallengeTurn {
    Respondent = 0,
    Challenger = 1,
    Prove = 2,
}

impl Challenge {
    pub const SEED_PREFIX: &[u8] = b"challenge";
    pub const DISCRIMINATOR: u8 = 9;

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DataAvailabilityProof {
    pub discriminator: u8,
    pub challenge: Pubkey,
    pub checkpoint: Pubkey,
    pub commitment: [u8; 32],
    pub payload_root: [u8; 32],
    pub inclusion_proof_hash: [u8; 32],
    pub revealed_at_l1_slot: u64,
    pub status: DataAvailabilityStatus,
    pub bump: u8,
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum DataAvailabilityStatus {
    Missing = 0,
    Revealed = 1,
    Defaulted = 2,
}

impl DataAvailabilityProof {
    pub const SEED_PREFIX: &[u8] = b"da_proof";
    pub const DISCRIMINATOR: u8 = 10;

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct StepProofAccount {
    pub discriminator: u8,
    pub checkpoint: Pubkey,
    pub challenge: Pubkey,
    pub authority: Pubkey,
    pub proof_kind: u8,
    pub proof_version: u8,
    pub step_index: u64,
    pub tx_effect_root: [u8; 32],
    pub readonly_l1_root: [u8; 32],
    pub settlement_effect_root: [u8; 32],
    pub public_input_hash: [u8; 32],
    pub written_len: u32,
    pub sealed: bool,
    pub proof_hash: [u8; 32],
    pub bump: u8,
    pub data: [u8; crate::MAX_STEP_PROOF_BYTES],
}

impl StepProofAccount {
    pub const SEED_PREFIX: &[u8] = b"step_proof";
    pub const DISCRIMINATOR: u8 = 7;

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

impl CheckpointCursor {
    pub const SEED_PREFIX: &[u8] = b"checkpoint_cursor";
    pub const DISCRIMINATOR: u8 = 6;

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

impl Session {
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
    /// Cumulative lamports requested for L1 payout via ER withdrawal
    /// transactions. Settlement pays only the delta above this value.
    pub withdrawn: u64,
    pub bump: u8,
}

impl DepositReceipt {
    pub const SEED_PREFIX: &[u8] = b"deposit_receipt";
    pub const DISCRIMINATOR: u8 = 4;

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[cfg_attr(feature = "idl", derive(shank::ShankAccount))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SessionBridge {
    pub discriminator: u8,
    pub session: Pubkey,
    pub mint: Pubkey,
    pub bridge_program: Pubkey,
    pub vault: Pubkey,
    pub token_program: Pubkey,
    pub bump: u8,
}

impl SessionBridge {
    pub const SEED_PREFIX: &[u8] = b"session_bridge";
    pub const DISCRIMINATOR: u8 = 8;

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
        assert_eq!(serialized.len(), account_size(&session));
    }

    #[test]
    fn test_fee_vault_len() {
        let vault = FeeVault {
            discriminator: FeeVault::DISCRIMINATOR,
            authority: [0xAB; 32],
            bump: 128,
        };
        let serialized = borsh::to_vec(&vault).unwrap();
        assert_eq!(serialized.len(), account_size(&vault));
    }

    #[test]
    fn test_deposit_receipt_len() {
        let receipt = DepositReceipt {
            discriminator: DepositReceipt::DISCRIMINATOR,
            session: [0x11; 32],
            recipient: [0x22; 32],
            balance: 1_000_000_000,
            withdrawn: 0,
            bump: 77,
        };
        let serialized = borsh::to_vec(&receipt).unwrap();
        assert_eq!(serialized.len(), account_size(&receipt));
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
        assert_eq!(serialized.len(), account_size(&record));
    }

    #[test]
    fn test_checkpoint_len() {
        let checkpoint = Checkpoint {
            discriminator: Checkpoint::DISCRIMINATOR,
            session: [0x10; 32],
            er_slot: 10,
            step_count: 4,
            previous_state_root: [0x11; 32],
            new_state_root: [0x12; 32],
            trace_root: [0x13; 32],
            tx_effect_root: [0x14; 32],
            readonly_l1_root: [0x15; 32],
            da_commitment: [0x16; 32],
            effect_commitment: [0x17; 32],
            proposer: [0x18; 32],
            proposed_at_l1_slot: 100,
            challenge_deadline_l1_slot: 110,
            status: CheckpointStatus::Pending,
            bond_lamports: 1_000_000,
            bond_status: CheckpointBondStatus::Locked,
            challenger: [0x19; 32],
            challenged_at_l1_slot: 105,
            challenge_resolved: true,
            bump: 99,
        };
        let serialized = borsh::to_vec(&checkpoint).unwrap();
        assert_eq!(serialized.len(), account_size(&checkpoint));
    }

    #[test]
    fn test_checkpoint_cursor_len() {
        let cursor = CheckpointCursor {
            discriminator: CheckpointCursor::DISCRIMINATOR,
            session: [0x10; 32],
            latest_finalized_checkpoint: [0x11; 32],
            latest_finalized_er_slot: 10,
            latest_finalized_state_root: [0x12; 32],
            active_checkpoint: [0x13; 32],
            active_er_slot: 11,
            bump: 99,
        };
        let serialized = borsh::to_vec(&cursor).unwrap();
        assert_eq!(serialized.len(), account_size(&cursor));
    }

    #[test]
    fn test_challenge_and_da_proof_len() {
        let challenge = Challenge {
            discriminator: Challenge::DISCRIMINATOR,
            checkpoint: [1; 32],
            challenger: [2; 32],
            respondent: [3; 32],
            opened_at_l1_slot: 1,
            hard_deadline_l1_slot: 10,
            turn_deadline_l1_slot: 5,
            start_step: 0,
            end_step: 4,
            midpoint_step: 2,
            start_state_root: [4; 32],
            end_state_root: [5; 32],
            midpoint_state_root: [6; 32],
            status: ChallengeStatus::Active,
            turn: ChallengeTurn::Respondent,
            rounds: 0,
            bump: 7,
        };
        assert_eq!(
            borsh::to_vec(&challenge).unwrap().len(),
            account_size(&challenge)
        );

        let da_proof = DataAvailabilityProof {
            discriminator: DataAvailabilityProof::DISCRIMINATOR,
            challenge: [1; 32],
            checkpoint: [2; 32],
            commitment: [3; 32],
            payload_root: [4; 32],
            inclusion_proof_hash: [5; 32],
            revealed_at_l1_slot: 6,
            status: DataAvailabilityStatus::Revealed,
            bump: 7,
        };
        assert_eq!(
            borsh::to_vec(&da_proof).unwrap().len(),
            account_size(&da_proof)
        );
    }

    #[test]
    fn test_step_proof_account_len() {
        let proof = StepProofAccount {
            discriminator: StepProofAccount::DISCRIMINATOR,
            checkpoint: [0x31; 32],
            challenge: [0x32; 32],
            authority: [0x33; 32],
            proof_kind: 1,
            proof_version: 1,
            step_index: 3,
            tx_effect_root: [0x34; 32],
            readonly_l1_root: [0x35; 32],
            settlement_effect_root: [0x36; 32],
            public_input_hash: [0x37; 32],
            written_len: 3,
            sealed: true,
            proof_hash: [0x35; 32],
            bump: 77,
            data: [0xAB; crate::MAX_STEP_PROOF_BYTES],
        };
        let serialized = borsh::to_vec(&proof).unwrap();
        assert_eq!(serialized.len(), account_size(&proof));
    }

    #[test]
    fn test_session_bridge_len() {
        let bridge = SessionBridge {
            discriminator: SessionBridge::DISCRIMINATOR,
            session: [0x11; 32],
            mint: [0x22; 32],
            bridge_program: [0x33; 32],
            vault: [0x44; 32],
            token_program: [0x55; 32],
            bump: 88,
        };
        let serialized = borsh::to_vec(&bridge).unwrap();
        assert_eq!(serialized.len(), account_size(&bridge));
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
