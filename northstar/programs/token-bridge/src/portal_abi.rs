use northstar_portal::{Checkpoint, CheckpointStatus, Session};

pub(crate) fn is_valid_settlement_session(session: &Session, validator: &[u8; 32]) -> bool {
    session.is_valid() && session.validator == *validator
}

pub(crate) fn is_valid_settlement_checkpoint(
    checkpoint: &Checkpoint,
    session: &[u8; 32],
    er_slot: u64,
    checksum: &[u8; 32],
) -> bool {
    checkpoint.is_valid()
        && checkpoint.session == *session
        && checkpoint.er_slot == er_slot
        && checkpoint.effect_commitment == *checksum
        && matches!(
            checkpoint.status,
            CheckpointStatus::Committed | CheckpointStatus::Settled
        )
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        northstar_portal::{CheckpointBondStatus, SettlementStatus},
    };

    #[test]
    fn settlement_session_requires_portal_state_and_validator() {
        let validator = [8; 32];
        let session = Session {
            discriminator: Session::DISCRIMINATOR,
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

        assert!(is_valid_settlement_session(&session, &validator));
        assert!(!is_valid_settlement_session(&session, &[9; 32]));
    }

    #[test]
    fn settlement_checkpoint_requires_matching_finalizable_state() {
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

        assert!(is_valid_settlement_checkpoint(
            &checkpoint,
            &session,
            er_slot,
            &checksum
        ));

        checkpoint.status = CheckpointStatus::Pending;
        assert!(!is_valid_settlement_checkpoint(
            &checkpoint,
            &session,
            er_slot,
            &checksum
        ));

        checkpoint.status = CheckpointStatus::Settled;
        assert!(is_valid_settlement_checkpoint(
            &checkpoint,
            &session,
            er_slot,
            &checksum
        ));
    }
}
