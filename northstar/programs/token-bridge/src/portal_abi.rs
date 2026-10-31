#[cfg(feature = "checkpoint-continuity-v2")]
use {
    borsh::BorshDeserialize,
    northstar_portal::{SessionBridgeV2, SessionV2},
    pinocchio::error::ProgramError,
    solana_sha256_hasher::hashv,
};
use {
    northstar_portal::{Checkpoint, CheckpointStatus, Session},
    pinocchio::Address,
};

#[cfg(feature = "checkpoint-continuity-v2")]
pub(crate) fn registration_v2(
    bridge_program: &Address,
    portal: &Address,
    address: &Address,
    data: &[u8],
) -> Result<SessionBridgeV2, ProgramError> {
    let frame =
        SessionBridgeV2::try_from_slice(data).map_err(|_| ProgramError::InvalidAccountData)?;
    let (expected, bump) = northstar_portal::find_session_bridge_v2_pda(
        portal,
        &frame.session_context,
        &frame.base.mint,
    );
    #[cfg(target_os = "solana")]
    let vault =
        Address::find_program_address(&[b"token_vault", address.as_ref()], bridge_program).0;
    #[cfg(not(target_os = "solana"))]
    let vault = Address::new_from_array(
        crate::find_token_vault_pda(
            &solana_pubkey::Pubkey::new_from_array(bridge_program.to_bytes()),
            &solana_pubkey::Pubkey::new_from_array(address.to_bytes()),
        )
        .0
        .to_bytes(),
    );
    if !frame.is_valid()
        || frame.base.bridge_program != *bridge_program
        || frame.base.token_program != pinocchio_token::ID
        || frame.base.session != northstar_portal::find_session_pda(portal).0
        || *address != expected
        || frame.base.bump != bump
        || frame.base.vault != vault
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(frame)
}

#[cfg(feature = "checkpoint-continuity-v2")]
pub(crate) fn session_v2(
    registration: &SessionBridgeV2,
    portal: &Address,
    address: &Address,
    data: &[u8],
) -> Result<SessionV2, ProgramError> {
    let frame = SessionV2::try_from_slice(data).map_err(|_| ProgramError::InvalidAccountData)?;
    let (expected, bump) = northstar_portal::find_session_pda(portal);
    if !frame.is_valid()
        || *address != expected
        || *address != registration.base.session
        || frame.base.bump != bump
        || frame
            .identity(portal, address)
            .commitment(|parts| hashv(parts).to_bytes())
            .map_err(|_| ProgramError::InvalidAccountData)?
            != registration.session_context
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(frame)
}

pub(crate) fn is_valid_settlement_session(session: &Session, validator: &Address) -> bool {
    session.is_valid() && session.validator == *validator
}

pub(crate) fn is_valid_settlement_checkpoint(
    checkpoint: &Checkpoint,
    session: &Address,
    er_slot: u64,
    checksum: &[u8; 32],
) -> bool {
    checkpoint.is_valid()
        && checkpoint.session == *session
        && checkpoint.er_slot == er_slot
        && checkpoint.effect_commitment == *checksum
        && checkpoint.status == CheckpointStatus::Settled
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        northstar_portal::{CheckpointBondStatus, SettlementStatus},
    };

    #[cfg(feature = "checkpoint-continuity-v2")]
    fn v2_fixture() -> (Address, Address, Address, SessionBridgeV2, SessionV2) {
        let bridge = Address::from([6; 32]);
        let portal = Address::from([7; 32]);
        let (session, bump) = northstar_portal::find_session_pda(&portal);
        let frame = SessionV2 {
            discriminator: SessionV2::DISCRIMINATOR,
            base: Session {
                discriminator: Session::DISCRIMINATOR,
                grid_id: 1,
                ttl_slots: 2_000,
                fee_cap: 0,
                created_at: 10,
                nonce: 11,
                authority: Address::from([8; 32]),
                validator: Address::from([9; 32]),
                settlement_interval_slots: 10,
                last_settled_l1_slot: 0,
                last_settled_er_slot: 0,
                settlement_status: SettlementStatus::Idle,
                settlement_er_slot: 0,
                settlement_checksum: [0; 32],
                settlement_accumulator: [0; 32],
                settlement_started_l1_slot: 0,
                bump,
            },
            l1_genesis: [1; 32],
            profile: [2; 32],
            ingress_count: 0,
            ingress_root: [0; 32],
            consumed_ingress_count: 0,
            consumed_ingress_root: [0; 32],
        };
        let context = frame
            .identity(&portal, &session)
            .commitment(|parts| hashv(parts).to_bytes())
            .unwrap();
        let mint = Address::from([3; 32]);
        let (address, bump) =
            northstar_portal::find_session_bridge_v2_pda(&portal, &context, &mint);
        let vault = crate::find_token_vault_pda(
            &solana_pubkey::Pubkey::new_from_array(bridge.to_bytes()),
            &solana_pubkey::Pubkey::new_from_array(address.to_bytes()),
        )
        .0;
        let registration = SessionBridgeV2 {
            discriminator: SessionBridgeV2::DISCRIMINATOR,
            base: northstar_portal::SessionBridge {
                discriminator: northstar_portal::SessionBridge::DISCRIMINATOR,
                session,
                mint,
                bridge_program: bridge,
                vault: Address::from(vault.to_bytes()),
                token_program: pinocchio_token::ID,
                bump,
            },
            session_context: context,
        };
        (bridge, portal, address, registration, frame)
    }

    #[cfg(feature = "checkpoint-continuity-v2")]
    #[test]
    fn invariant_v2_registry_requires_canonical_program_pda_and_vault() {
        let (bridge, portal, address, registration, _) = v2_fixture();
        let raw = borsh::to_vec(&registration).unwrap();
        assert_eq!(
            registration_v2(&bridge, &portal, &address, &raw).unwrap(),
            registration
        );
        for mutation in 0..4 {
            let mut changed = registration;
            match mutation {
                0 => changed.base.bridge_program = Address::from([10; 32]),
                1 => changed.base.vault = Address::from([10; 32]),
                2 => changed.base.bump = changed.base.bump.wrapping_add(1),
                _ => changed.session_context = [0; 32],
            }
            assert!(registration_v2(
                &bridge,
                &portal,
                &address,
                &borsh::to_vec(&changed).unwrap()
            )
            .is_err());
        }
    }

    #[cfg(feature = "checkpoint-continuity-v2")]
    #[test]
    fn invariant_v2_bridge_rejects_changed_session_incarnations() {
        let (_, portal, _, registration, frame) = v2_fixture();
        assert_eq!(
            session_v2(
                &registration,
                &portal,
                &registration.base.session,
                &borsh::to_vec(&frame).unwrap()
            )
            .unwrap(),
            frame
        );
        for mutation in 0..5 {
            let mut changed = frame;
            match mutation {
                0 => changed.base.nonce += 1,
                1 => changed.profile = [10; 32],
                2 => changed.l1_genesis = [10; 32],
                3 => changed.base.validator = Address::from([10; 32]),
                _ => changed.base.grid_id += 1,
            }
            assert!(session_v2(
                &registration,
                &portal,
                &registration.base.session,
                &borsh::to_vec(&changed).unwrap()
            )
            .is_err());
        }
    }

    #[test]
    fn settlement_session_requires_portal_state_and_validator() {
        let validator = Address::from([8; 32]);
        let session = Session {
            discriminator: Session::DISCRIMINATOR,
            grid_id: 1,
            ttl_slots: 2_000,
            fee_cap: 1_000_000,
            created_at: 10,
            nonce: 11,
            authority: [7; 32].into(),
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
        assert!(!is_valid_settlement_session(
            &session,
            &Address::from([9; 32])
        ));
    }

    #[test]
    fn settlement_checkpoint_requires_matching_finalizable_state() {
        let session = Address::from([7; 32]);
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
            proposer: [9; 32].into(),
            proposed_at_l1_slot: 10,
            challenge_deadline_l1_slot: 20,
            status: CheckpointStatus::Committed,
            bond_lamports: 1_000_000,
            bond_status: CheckpointBondStatus::Locked,
            challenger: [0; 32].into(),
            challenged_at_l1_slot: 0,
            challenge_resolved: false,
            bump: 255,
        };

        assert!(!is_valid_settlement_checkpoint(
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
