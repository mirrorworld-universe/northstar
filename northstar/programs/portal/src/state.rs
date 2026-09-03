use {
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::Address as Pubkey,
    pinocchio_idl_macros::p_state,
};

pub fn account_size<T: BorshSerialize + ?Sized>(account: &T) -> usize {
    borsh::object_length(account).expect("account serialization length overflow")
}

pub type Hash32 = [u8; 32];

/// Fixed ER account that holds withdrawn SOL until L1 settlement.
pub const WITHDRAWAL_SINK: Pubkey = Pubkey::new_from_array([
    0x05, 0x7d, 0x77, 0xa2, 0x13, 0x37, 0xb6, 0x2d, 0xb7, 0x7d, 0xba, 0x7e, 0x26, 0xf8, 0xe1, 0x47,
    0x06, 0x35, 0xbd, 0x36, 0x53, 0x76, 0xa6, 0x7d, 0x7f, 0xf5, 0xa1, 0x82, 0x3e, 0xe0, 0x8f, 0xb8,
]);

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Session {
    pub discriminator: [u8; 8],
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
    pub settlement_checksum: Hash32,
    pub settlement_accumulator: Hash32,
    pub settlement_started_l1_slot: u64,
    pub bump: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum SettlementStatus {
    Idle = 0,
    InProgress = 1,
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Checkpoint {
    pub discriminator: [u8; 8],
    pub session: Pubkey,
    pub er_slot: u64,
    pub step_count: u64,
    pub previous_state_root: Hash32,
    pub new_state_root: Hash32,
    pub trace_root: Hash32,
    pub tx_effect_root: Hash32,
    pub readonly_l1_root: Hash32,
    pub da_commitment: Hash32,
    pub effect_commitment: Hash32,
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
    pub const DISCRIMINATOR: [u8; 8] = [199, 62, 186, 186, 98, 119, 211, 139];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CheckpointCursor {
    pub discriminator: [u8; 8],
    pub session: Pubkey,
    pub latest_finalized_checkpoint: Pubkey,
    pub latest_finalized_er_slot: u64,
    pub latest_finalized_state_root: Hash32,
    pub active_checkpoint: Pubkey,
    pub active_er_slot: u64,
    pub bump: u8,
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Challenge {
    pub discriminator: [u8; 8],
    pub checkpoint: Pubkey,
    pub challenger: Pubkey,
    pub respondent: Pubkey,
    pub opened_at_l1_slot: u64,
    pub hard_deadline_l1_slot: u64,
    pub turn_deadline_l1_slot: u64,
    pub start_step: u64,
    pub end_step: u64,
    pub midpoint_step: u64,
    pub start_state_root: Hash32,
    pub end_state_root: Hash32,
    pub midpoint_state_root: Hash32,
    pub status: ChallengeStatus,
    pub turn: ChallengeTurn,
    pub rounds: u32,
    pub bump: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum ChallengeStatus {
    Active = 0,
    ChallengerWon = 1,
    ValidatorWon = 2,
}

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
    pub const DISCRIMINATOR: [u8; 8] = [119, 250, 161, 121, 119, 81, 22, 208];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DataAvailabilityProof {
    pub discriminator: [u8; 8],
    pub challenge: Pubkey,
    pub checkpoint: Pubkey,
    pub commitment: Hash32,
    pub payload_root: Hash32,
    pub inclusion_proof_hash: Hash32,
    pub revealed_at_l1_slot: u64,
    pub status: DataAvailabilityStatus,
    pub bump: u8,
}

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
    pub const DISCRIMINATOR: [u8; 8] = [24, 139, 155, 40, 135, 132, 79, 129];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct StepProofAccount {
    pub discriminator: [u8; 8],
    pub checkpoint: Pubkey,
    pub challenge: Pubkey,
    pub authority: Pubkey,
    pub proof_kind: u8,
    pub proof_version: u8,
    pub step_index: u64,
    pub tx_effect_root: Hash32,
    pub readonly_l1_root: Hash32,
    pub settlement_effect_root: Hash32,
    pub public_input_hash: Hash32,
    pub written_len: u32,
    pub sealed: bool,
    pub proof_hash: Hash32,
    pub bump: u8,
    // IDL schemas require literal array lengths.
    pub data: [u8; 256],
}

impl StepProofAccount {
    pub const SEED_PREFIX: &[u8] = b"step_proof";
    pub const DISCRIMINATOR: [u8; 8] = [62, 34, 242, 0, 245, 185, 56, 19];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

impl CheckpointCursor {
    pub const SEED_PREFIX: &[u8] = b"checkpoint_cursor";
    pub const DISCRIMINATOR: [u8; 8] = [251, 223, 216, 188, 51, 244, 129, 196];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

impl Session {
    pub const SEED_PREFIX: &[u8] = b"session";
    pub const DISCRIMINATOR: [u8; 8] = [243, 81, 72, 115, 214, 188, 72, 144];

    #[inline]
    pub fn is_expired(&self, current_slot: u64) -> bool {
        current_slot > self.created_at.saturating_add(self.ttl_slots)
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct FeeVault {
    pub discriminator: [u8; 8],
    pub authority: [u8; 32],
    pub bump: u8,
}

impl FeeVault {
    pub const SEED_PREFIX: &[u8] = b"fee_vault";
    pub const DISCRIMINATOR: [u8; 8] = [192, 178, 69, 232, 58, 149, 157, 132];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DelegationRecord {
    pub discriminator: [u8; 8],
    pub owner_program: Pubkey,
    pub grid_id: u64,
    pub bump: u8,
}

impl DelegationRecord {
    pub const SEED_PREFIX: &[u8] = b"delegation";
    pub const DISCRIMINATOR: [u8; 8] = [203, 185, 161, 226, 129, 251, 132, 155];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct UndelegationRequest {
    pub discriminator: [u8; 8],
    pub session: Pubkey,
    pub delegated_account: Pubkey,
    pub owner_program: Pubkey,
    pub authority: Pubkey,
    pub requested_at_l1_slot: u64,
    pub approved: bool,
    pub bump: u8,
}

impl UndelegationRequest {
    pub const SEED_PREFIX: &[u8] = b"undelegation_request";
    pub const DISCRIMINATOR: [u8; 8] = [141, 66, 225, 119, 110, 101, 37, 38];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DepositReceipt {
    pub discriminator: [u8; 8],
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
    pub const DISCRIMINATOR: [u8; 8] = [64, 175, 24, 183, 138, 109, 70, 78];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SessionBridge {
    pub discriminator: [u8; 8],
    pub session: Pubkey,
    pub mint: Pubkey,
    pub bridge_program: Pubkey,
    pub vault: Pubkey,
    pub token_program: Pubkey,
    pub bump: u8,
}

impl SessionBridge {
    pub const SEED_PREFIX: &[u8] = b"session_bridge";
    pub const DISCRIMINATOR: [u8; 8] = [145, 47, 8, 254, 118, 119, 114, 215];

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[p_state]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TokenWithdrawalAuthorization {
    pub discriminator: [u8; 8],
    pub checkpoint: Pubkey,
    pub tuple_hash: Hash32,
    pub consumed: bool,
    pub bump: u8,
}

impl TokenWithdrawalAuthorization {
    pub const SEED_PREFIX: &[u8] = b"token_withdrawal_auth";
    pub const DISCRIMINATOR: [u8; 8] = [201, 36, 84, 147, 113, 63, 92, 11];

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
            authority: [1; 32].into(),
            validator: [2; 32].into(),
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
        assert_eq!(serialized.len(), 226);
    }

    #[test]
    fn test_fee_vault_len() {
        let vault = FeeVault {
            discriminator: FeeVault::DISCRIMINATOR,
            authority: [0xAB; 32],
            bump: 128,
        };
        let serialized = borsh::to_vec(&vault).unwrap();
        assert_eq!(serialized.len(), 41);
    }

    #[test]
    fn test_deposit_receipt_len() {
        let receipt = DepositReceipt {
            discriminator: DepositReceipt::DISCRIMINATOR,
            session: [0x11; 32].into(),
            recipient: [0x22; 32].into(),
            balance: 1_000_000_000,
            withdrawn: 0,
            bump: 77,
        };
        let serialized = borsh::to_vec(&receipt).unwrap();
        assert_eq!(serialized.len(), 89);
    }

    #[test]
    fn test_delegation_record_len() {
        let record = DelegationRecord {
            discriminator: DelegationRecord::DISCRIMINATOR,
            owner_program: [0xDE; 32].into(),
            grid_id: 456,
            bump: 77,
        };
        let serialized = borsh::to_vec(&record).unwrap();
        assert_eq!(serialized.len(), 49);
    }

    #[test]
    fn test_checkpoint_len() {
        let checkpoint = Checkpoint {
            discriminator: Checkpoint::DISCRIMINATOR,
            session: [0x10; 32].into(),
            er_slot: 10,
            step_count: 4,
            previous_state_root: [0x11; 32],
            new_state_root: [0x12; 32],
            trace_root: [0x13; 32],
            tx_effect_root: [0x14; 32],
            readonly_l1_root: [0x15; 32],
            da_commitment: [0x16; 32],
            effect_commitment: [0x17; 32],
            proposer: [0x18; 32].into(),
            proposed_at_l1_slot: 100,
            challenge_deadline_l1_slot: 110,
            status: CheckpointStatus::Pending,
            bond_lamports: 1_000_000,
            bond_status: CheckpointBondStatus::Locked,
            challenger: [0x19; 32].into(),
            challenged_at_l1_slot: 105,
            challenge_resolved: true,
            bump: 99,
        };
        let serialized = borsh::to_vec(&checkpoint).unwrap();
        assert_eq!(serialized.len(), 380);
    }

    #[test]
    fn test_checkpoint_cursor_len() {
        let cursor = CheckpointCursor {
            discriminator: CheckpointCursor::DISCRIMINATOR,
            session: [0x10; 32].into(),
            latest_finalized_checkpoint: [0x11; 32].into(),
            latest_finalized_er_slot: 10,
            latest_finalized_state_root: [0x12; 32],
            active_checkpoint: [0x13; 32].into(),
            active_er_slot: 11,
            bump: 99,
        };
        let serialized = borsh::to_vec(&cursor).unwrap();
        assert_eq!(serialized.len(), 153);
    }

    #[test]
    fn test_challenge_and_da_proof_len() {
        let challenge = Challenge {
            discriminator: Challenge::DISCRIMINATOR,
            checkpoint: [1; 32].into(),
            challenger: [2; 32].into(),
            respondent: [3; 32].into(),
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
        assert_eq!(borsh::to_vec(&challenge).unwrap().len(), 255);

        let da_proof = DataAvailabilityProof {
            discriminator: DataAvailabilityProof::DISCRIMINATOR,
            challenge: [1; 32].into(),
            checkpoint: [2; 32].into(),
            commitment: [3; 32],
            payload_root: [4; 32],
            inclusion_proof_hash: [5; 32],
            revealed_at_l1_slot: 6,
            status: DataAvailabilityStatus::Revealed,
            bump: 7,
        };
        assert_eq!(borsh::to_vec(&da_proof).unwrap().len(), 178);
    }

    #[test]
    fn test_step_proof_account_len() {
        let proof = StepProofAccount {
            discriminator: StepProofAccount::DISCRIMINATOR,
            checkpoint: [0x31; 32].into(),
            challenge: [0x32; 32].into(),
            authority: [0x33; 32].into(),
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
        assert_eq!(serialized.len(), 536);
    }

    #[test]
    fn test_session_bridge_len() {
        let bridge = SessionBridge {
            discriminator: SessionBridge::DISCRIMINATOR,
            session: [0x11; 32].into(),
            mint: [0x22; 32].into(),
            bridge_program: [0x33; 32].into(),
            vault: [0x44; 32].into(),
            token_program: [0x55; 32].into(),
            bump: 88,
        };
        let serialized = borsh::to_vec(&bridge).unwrap();
        assert_eq!(serialized.len(), 169);
    }

    #[test]
    fn account_discriminators_are_anchor_compatible() {
        assert_eq!(
            Session::DISCRIMINATOR,
            [243, 81, 72, 115, 214, 188, 72, 144]
        );
        assert_eq!(
            FeeVault::DISCRIMINATOR,
            [192, 178, 69, 232, 58, 149, 157, 132]
        );
        assert_eq!(
            DelegationRecord::DISCRIMINATOR,
            [203, 185, 161, 226, 129, 251, 132, 155]
        );
        assert_eq!(
            DepositReceipt::DISCRIMINATOR,
            [64, 175, 24, 183, 138, 109, 70, 78]
        );
        assert_eq!(
            Checkpoint::DISCRIMINATOR,
            [199, 62, 186, 186, 98, 119, 211, 139]
        );
        assert_eq!(
            CheckpointCursor::DISCRIMINATOR,
            [251, 223, 216, 188, 51, 244, 129, 196]
        );
        assert_eq!(
            Challenge::DISCRIMINATOR,
            [119, 250, 161, 121, 119, 81, 22, 208]
        );
        assert_eq!(
            DataAvailabilityProof::DISCRIMINATOR,
            [24, 139, 155, 40, 135, 132, 79, 129]
        );
        assert_eq!(
            StepProofAccount::DISCRIMINATOR,
            [62, 34, 242, 0, 245, 185, 56, 19]
        );
        assert_eq!(
            SessionBridge::DISCRIMINATOR,
            [145, 47, 8, 254, 118, 119, 114, 215]
        );
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
            authority: [1; 32].into(),
            validator: [2; 32].into(),
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
