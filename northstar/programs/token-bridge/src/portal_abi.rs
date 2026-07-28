const SESSION_LEN: usize = 219;
const SESSION_VALIDATOR_OFFSET: usize = 81;
const CHECKPOINT_LEN: usize = 373;
const CHECKPOINT_EFFECT_COMMITMENT_OFFSET: usize = 241;
const CHECKPOINT_STATUS_OFFSET: usize = 321;

pub(crate) fn is_valid_settlement_session(data: &[u8], validator: &[u8; 32]) -> bool {
    data.len() == SESSION_LEN
        && data[0] == 1
        && data[SESSION_VALIDATOR_OFFSET..SESSION_VALIDATOR_OFFSET + 32] == validator[..]
}

// Keep this zero-copy parser aligned with the Portal checkpoint account ABI.
pub(crate) fn is_valid_settlement_checkpoint(
    data: &[u8],
    session: &[u8; 32],
    er_slot: u64,
    checksum: &[u8; 32],
) -> bool {
    data.len() == CHECKPOINT_LEN
        && data[0] == 5
        && data[1..33] == session[..]
        && u64::from_le_bytes(data[33..41].try_into().unwrap()) == er_slot
        && data[CHECKPOINT_EFFECT_COMMITMENT_OFFSET..CHECKPOINT_EFFECT_COMMITMENT_OFFSET + 32]
            == checksum[..]
        && matches!(data[CHECKPOINT_STATUS_OFFSET], 1 | 3)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        northstar_portal::{
            Checkpoint, CheckpointBondStatus, CheckpointStatus, Session, SettlementStatus,
        },
    };

    #[test]
    fn settlement_session_validation_matches_portal_abi() {
        let validator = [8; 32];
        let session = Session {
            discriminator: 1,
            grid_id: 1,
            ttl_slots: 2_000,
            fee_cap: 1_000_000,
            created_at: 10,
            nonce: 11,
            authority: [7; 32],
            validator,
            settlement_interval_slots: 10,
            last_settled_l1_slot: 0,
            last_settled_er_slot: 0,
            settlement_status: SettlementStatus::Idle,
            settlement_er_slot: 0,
            settlement_checksum: [0; 32],
            settlement_accumulator: [0; 32],
            settlement_started_l1_slot: 0,
            bump: 255,
        };
        let data = borsh::to_vec(&session).unwrap();

        assert_eq!(data.len(), Session::LEN);
        assert!(is_valid_settlement_session(&data, &validator));
    }

    #[test]
    fn settlement_checkpoint_validation_matches_portal_abi() {
        let session = [7; 32];
        let checksum = [8; 32];
        let er_slot = 9;
        let mut checkpoint = Checkpoint {
            discriminator: Checkpoint::DISCRIMINATOR,
            session,
            er_slot,
            step_count: 1,
            previous_state_root: [1; 32],
            new_state_root: [2; 32],
            trace_root: [3; 32],
            tx_effect_root: [4; 32],
            readonly_l1_root: [5; 32],
            da_commitment: [6; 32],
            effect_commitment: checksum,
            proposer: [9; 32],
            proposed_at_l1_slot: 10,
            challenge_deadline_l1_slot: 20,
            status: CheckpointStatus::Committed,
            bond_lamports: 1_000_000,
            bond_status: CheckpointBondStatus::Locked,
            challenger: [0; 32],
            challenged_at_l1_slot: 0,
            challenge_resolved: false,
            bump: 255,
        };
        let data = borsh::to_vec(&checkpoint).unwrap();

        assert_eq!(data.len(), Checkpoint::LEN);
        assert!(is_valid_settlement_checkpoint(
            &data, &session, er_slot, &checksum
        ));

        checkpoint.status = CheckpointStatus::Pending;
        let pending = borsh::to_vec(&checkpoint).unwrap();
        assert!(!is_valid_settlement_checkpoint(
            &pending, &session, er_slot, &checksum
        ));

        checkpoint.status = CheckpointStatus::Settled;
        let settled = borsh::to_vec(&checkpoint).unwrap();
        assert!(is_valid_settlement_checkpoint(
            &settled, &session, er_slot, &checksum
        ));
    }
}
