use {
    super::initialize_pda_account,
    crate::{
        find_challenge_pda, find_checkpoint_cursor_pda, find_checkpoint_pda, find_da_proof_pda,
        find_session_pda, find_step_proof_pda, BisectChallenge, CancelCheckpoint, Challenge,
        ChallengeStatus, ChallengeTurn, Checkpoint, CheckpointBondStatus, CheckpointCursor,
        CheckpointStatus, CommitCheckpoint, CreateStepProof, DataAvailabilityProof,
        DataAvailabilityStatus, OpenChallenge, PortalError, ProposeCheckpoint, ResolveChallenge,
        RespondChallenge, SealStepProof, Session, StepProofAccount, StepProofVerifierMode,
        TimeoutChallenge, WriteStepProof, CHALLENGE_TURN_WINDOW_SLOTS,
        CHECKPOINT_PROPOSER_BOND_LAMPORTS, MAX_CHALLENGE_WINDOW_SLOTS, MAX_STEP_PROOF_BYTES,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        cpi::{Seed, Signer},
        error::ProgramError,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
    pinocchio_system::instructions::Transfer,
    solana_sha256_hasher::hashv,
};

fn load_session(program_id: &Pubkey, session: &AccountInfo) -> Result<Session, ProgramError> {
    let (expected_session_key, _) = find_session_pda(program_id);
    if session.address() != &expected_session_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !session.owned_by(program_id) {
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }
    let session_state = Session::try_from_slice(&session.try_borrow()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !session_state.is_valid() {
        return Err(PortalError::SessionStateInvalid.into());
    }
    Ok(session_state)
}

fn store_checkpoint(checkpoint: &mut AccountInfo, checkpoint_state: &Checkpoint) -> ProgramResult {
    let mut checkpoint_data = checkpoint.try_borrow_mut()?;
    BorshSerialize::serialize(checkpoint_state, &mut &mut checkpoint_data[..]).unwrap();
    Ok(())
}

fn store_cursor(cursor: &mut AccountInfo, cursor_state: &CheckpointCursor) -> ProgramResult {
    let mut cursor_data = cursor.try_borrow_mut()?;
    BorshSerialize::serialize(cursor_state, &mut &mut cursor_data[..]).unwrap();
    Ok(())
}

fn store_challenge(challenge: &mut AccountInfo, challenge_state: &Challenge) -> ProgramResult {
    let mut data = challenge.try_borrow_mut()?;
    BorshSerialize::serialize(challenge_state, &mut &mut data[..]).unwrap();
    Ok(())
}

fn load_challenge(
    program_id: &Pubkey,
    checkpoint_key: &Pubkey,
    challenge: &AccountInfo,
) -> Result<Challenge, ProgramError> {
    let (expected_key, _) = find_challenge_pda(program_id, checkpoint_key);
    if challenge.address() != &expected_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !challenge.owned_by(program_id) {
        return Err(PortalError::ChallengeStateInvalid.into());
    }
    let state = Challenge::try_from_slice(&challenge.try_borrow()?)
        .map_err(|_| PortalError::ChallengeDeserializeFailed)?;
    if !state.is_valid() || state.checkpoint != *checkpoint_key {
        return Err(PortalError::ChallengeStateInvalid.into());
    }
    Ok(state)
}

fn store_da_proof(account: &mut AccountInfo, state: &DataAvailabilityProof) -> ProgramResult {
    let mut data = account.try_borrow_mut()?;
    BorshSerialize::serialize(state, &mut &mut data[..]).unwrap();
    Ok(())
}

fn load_da_proof(
    program_id: &Pubkey,
    challenge_key: &Pubkey,
    checkpoint_key: &Pubkey,
    account: &AccountInfo,
) -> Result<DataAvailabilityProof, ProgramError> {
    let (expected_key, _) = find_da_proof_pda(program_id, challenge_key);
    if account.address() != &expected_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !account.owned_by(program_id) {
        return Err(PortalError::DataAvailabilityStateInvalid.into());
    }
    let state = DataAvailabilityProof::try_from_slice(&account.try_borrow()?)
        .map_err(|_| PortalError::DataAvailabilityDeserializeFailed)?;
    if !state.is_valid() || state.challenge != *challenge_key || state.checkpoint != *checkpoint_key
    {
        return Err(PortalError::DataAvailabilityStateInvalid.into());
    }
    Ok(state)
}

fn store_step_proof(proof: &mut AccountInfo, proof_state: &StepProofAccount) -> ProgramResult {
    let mut proof_data = proof.try_borrow_mut()?;
    BorshSerialize::serialize(proof_state, &mut &mut proof_data[..]).unwrap();
    Ok(())
}

fn load_step_proof(
    program_id: &Pubkey,
    checkpoint_key: &Pubkey,
    proof: &AccountInfo,
) -> Result<StepProofAccount, ProgramError> {
    let (expected_proof_key, _) = find_step_proof_pda(program_id, checkpoint_key);
    if proof.address() != &expected_proof_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !proof.owned_by(program_id) {
        return Err(PortalError::StepProofStateInvalid.into());
    }
    let proof_state = StepProofAccount::try_from_slice(&proof.try_borrow()?)
        .map_err(|_| PortalError::StepProofDeserializeFailed)?;
    if !proof_state.is_valid() || proof_state.checkpoint != *checkpoint_key {
        return Err(PortalError::StepProofStateInvalid.into());
    }
    Ok(proof_state)
}

fn resolve_checkpoint_bond(
    checkpoint: &mut AccountInfo,
    recipient: &mut AccountInfo,
    checkpoint_state: &mut Checkpoint,
    next_status: CheckpointBondStatus,
) -> ProgramResult {
    if checkpoint_state.bond_status != CheckpointBondStatus::Locked {
        return Err(PortalError::CheckpointBondAlreadyResolved.into());
    }

    let bond_lamports = checkpoint_state.bond_lamports;
    if bond_lamports == 0 {
        return Err(PortalError::CheckpointBondInsufficient.into());
    }

    let rent_lamports = Rent::get()?.try_minimum_balance(crate::account_size(checkpoint_state))?;
    let minimum_locked_lamports = rent_lamports
        .checked_add(bond_lamports)
        .ok_or(PortalError::ArithmeticOverflow)?;
    if checkpoint.lamports() < minimum_locked_lamports {
        return Err(PortalError::CheckpointBondInsufficient.into());
    }

    let recipient_lamports = recipient
        .lamports()
        .checked_add(bond_lamports)
        .ok_or(PortalError::ArithmeticOverflow)?;
    recipient.set_lamports(recipient_lamports);
    let checkpoint_lamports = checkpoint
        .lamports()
        .checked_sub(bond_lamports)
        .ok_or(PortalError::ArithmeticOverflow)?;
    checkpoint.set_lamports(checkpoint_lamports);
    checkpoint_state.bond_status = next_status;

    Ok(())
}

fn release_checkpoint_bond(
    checkpoint: &mut AccountInfo,
    proposer: &mut AccountInfo,
    checkpoint_state: &mut Checkpoint,
) -> ProgramResult {
    if proposer.address() != &checkpoint_state.proposer {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    resolve_checkpoint_bond(
        checkpoint,
        proposer,
        checkpoint_state,
        CheckpointBondStatus::Released,
    )
}

#[allow(dead_code)]
pub(crate) fn slash_checkpoint_bond(
    checkpoint: &mut AccountInfo,
    recipient: &mut AccountInfo,
    checkpoint_state: &mut Checkpoint,
) -> ProgramResult {
    resolve_checkpoint_bond(
        checkpoint,
        recipient,
        checkpoint_state,
        CheckpointBondStatus::Slashed,
    )
}

fn load_checkpoint(
    program_id: &Pubkey,
    session_key: &Pubkey,
    er_slot: u64,
    checkpoint: &AccountInfo,
) -> Result<Checkpoint, ProgramError> {
    let (expected_checkpoint_key, _) = find_checkpoint_pda(program_id, session_key, er_slot);
    if checkpoint.address() != &expected_checkpoint_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !checkpoint.owned_by(program_id) {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let checkpoint_state = Checkpoint::try_from_slice(&checkpoint.try_borrow()?)
        .map_err(|_| PortalError::CheckpointDeserializeFailed)?;
    if !checkpoint_state.is_valid()
        || checkpoint_state.session != *session_key
        || checkpoint_state.er_slot != er_slot
    {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    Ok(checkpoint_state)
}

fn load_cursor(
    program_id: &Pubkey,
    session_key: &Pubkey,
    cursor: &AccountInfo,
) -> Result<CheckpointCursor, ProgramError> {
    let (expected_cursor_key, _) = find_checkpoint_cursor_pda(program_id, session_key);
    if cursor.address() != &expected_cursor_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !cursor.owned_by(program_id) {
        return Err(PortalError::CheckpointCursorStateInvalid.into());
    }
    let cursor_state = CheckpointCursor::try_from_slice(&cursor.try_borrow()?)
        .map_err(|_| PortalError::CheckpointCursorDeserializeFailed)?;
    if !cursor_state.is_valid() || cursor_state.session != *session_key {
        return Err(PortalError::CheckpointCursorStateInvalid.into());
    }
    Ok(cursor_state)
}

fn create_cursor(
    program_id: &Pubkey,
    payer: &AccountInfo,
    session_key: &Pubkey,
    cursor: &mut AccountInfo,
) -> Result<CheckpointCursor, ProgramError> {
    let (expected_cursor_key, cursor_bump) = find_checkpoint_cursor_pda(program_id, session_key);
    if cursor.address() != &expected_cursor_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let cursor_state = CheckpointCursor {
        discriminator: CheckpointCursor::DISCRIMINATOR,
        session: *session_key,
        latest_finalized_checkpoint: [0; 32].into(),
        latest_finalized_er_slot: 0,
        latest_finalized_state_root: [0; 32],
        active_checkpoint: [0; 32].into(),
        active_er_slot: 0,
        bump: cursor_bump,
    };
    let rent = Rent::get()?;
    let cursor_size = crate::account_size(&cursor_state);
    let cursor_lamports = rent.try_minimum_balance(cursor_size)?;
    let cursor_bump_bytes = [cursor_bump];
    let cursor_seeds = &[
        Seed::from(CheckpointCursor::SEED_PREFIX),
        Seed::from(session_key.as_ref()),
        Seed::from(cursor_bump_bytes.as_ref()),
    ];
    let cursor_signer = Signer::from(cursor_seeds);

    initialize_pda_account(
        payer,
        cursor,
        cursor_lamports,
        cursor_size as u64,
        program_id,
        cursor_signer,
    )?;

    store_cursor(cursor, &cursor_state)?;
    Ok(cursor_state)
}

fn load_or_create_cursor(
    program_id: &Pubkey,
    payer: &AccountInfo,
    session_key: &Pubkey,
    cursor: &mut AccountInfo,
) -> Result<CheckpointCursor, ProgramError> {
    if cursor.is_data_empty() {
        create_cursor(program_id, payer, session_key, cursor)
    } else {
        load_cursor(program_id, session_key, cursor)
    }
}

fn require_advancing_checkpoint(
    session_state: &Session,
    cursor_state: &CheckpointCursor,
    er_slot: u64,
) -> ProgramResult {
    if er_slot <= session_state.last_settled_er_slot
        || er_slot <= cursor_state.latest_finalized_er_slot
    {
        return Err(PortalError::CheckpointErSlotNotAdvanced.into());
    }
    Ok(())
}

fn require_no_active_checkpoint(cursor_state: &CheckpointCursor) -> ProgramResult {
    if cursor_state.active_checkpoint != [0; 32].into() {
        return Err(PortalError::CheckpointActiveExists.into());
    }
    Ok(())
}

fn require_active_checkpoint(
    cursor_state: &CheckpointCursor,
    checkpoint_key: &Pubkey,
    er_slot: u64,
) -> ProgramResult {
    if cursor_state.active_checkpoint != *checkpoint_key || cursor_state.active_er_slot != er_slot {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    Ok(())
}

fn clear_active_checkpoint(cursor_state: &mut CheckpointCursor) {
    cursor_state.active_checkpoint = [0; 32].into();
    cursor_state.active_er_slot = 0;
}

fn next_turn_deadline(current_slot: u64, hard_deadline: u64) -> Result<u64, ProgramError> {
    Ok(core::cmp::min(
        current_slot
            .checked_add(CHALLENGE_TURN_WINDOW_SLOTS)
            .ok_or(PortalError::ArithmeticOverflow)?,
        hard_deadline,
    ))
}

fn require_live_turn(challenge_state: &Challenge, current_slot: u64) -> ProgramResult {
    if challenge_state.status != ChallengeStatus::Active {
        return Err(PortalError::ChallengeStateInvalid.into());
    }
    if current_slot >= challenge_state.turn_deadline_l1_slot
        || current_slot >= challenge_state.hard_deadline_l1_slot
    {
        return Err(PortalError::CheckpointChallengeWindowClosed.into());
    }
    Ok(())
}

fn step_proof_public_input_hash(
    program_id: &Pubkey,
    session_key: &Pubkey,
    checkpoint_key: &Pubkey,
    checkpoint_state: &Checkpoint,
    challenge_state: &Challenge,
    proof_state: &StepProofAccount,
) -> [u8; 32] {
    hashv(&[
        b"northstar-er-step-v1",
        program_id.as_ref(),
        session_key.as_ref(),
        checkpoint_key.as_ref(),
        &[proof_state.proof_kind],
        &[proof_state.proof_version],
        &checkpoint_state.er_slot.to_le_bytes(),
        &challenge_state.start_step.to_le_bytes(),
        &challenge_state.start_state_root,
        &challenge_state.end_state_root,
        &proof_state.tx_effect_root,
        &proof_state.readonly_l1_root,
        &proof_state.settlement_effect_root,
    ])
    .to_bytes()
}

#[p_instruction(
    id = 14,
    accounts = [
        proposer(signer, mut),
        session(state = Session),
        checkpoint(mut, state = Checkpoint),
        checkpoint_cursor(mut, state = CheckpointCursor),
        system_program
    ],
    data = [
        er_slot: u64,
        step_count: u64,
        previous_state_root: Hash32,
        new_state_root: Hash32,
        trace_root: Hash32,
        tx_effect_root: Hash32,
        readonly_l1_root: Hash32,
        da_commitment: Hash32,
        effect_commitment: Hash32,
        challenge_window_slots: u64
    ]
)]
pub fn process_propose_checkpoint(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    ProposeCheckpoint {
        er_slot,
        step_count,
        previous_state_root,
        new_state_root,
        trace_root,
        tx_effect_root,
        readonly_l1_root,
        da_commitment,
        effect_commitment,
        challenge_window_slots,
    }: ProposeCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: ProposeCheckpoint, er_slot={}", er_slot);

    if challenge_window_slots == 0 || step_count == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if challenge_window_slots > MAX_CHALLENGE_WINDOW_SLOTS {
        return Err(PortalError::CheckpointChallengeWindowTooLong.into());
    }

    let [proposer, session, checkpoint, cursor, _system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !proposer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let session_state = load_session(program_id, session)?;
    if proposer.address() != &session_state.validator {
        return Err(PortalError::Unauthorized.into());
    }

    let current_slot = Clock::get()?.slot;
    if session_state.is_expired(current_slot) {
        return Err(PortalError::SessionExpired.into());
    }

    let session_key = session.address();
    let mut cursor_state = load_or_create_cursor(program_id, proposer, session_key, cursor)?;
    require_no_active_checkpoint(&cursor_state)?;
    require_advancing_checkpoint(&session_state, &cursor_state, er_slot)?;
    if previous_state_root != cursor_state.latest_finalized_state_root {
        return Err(PortalError::CheckpointPreviousRootMismatch.into());
    }

    let (expected_checkpoint_key, checkpoint_bump) =
        find_checkpoint_pda(program_id, session_key, er_slot);
    if checkpoint.address() != &expected_checkpoint_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let checkpoint_state = Checkpoint {
        discriminator: Checkpoint::DISCRIMINATOR,
        session: *session_key,
        er_slot,
        step_count,
        previous_state_root,
        new_state_root,
        trace_root,
        tx_effect_root,
        readonly_l1_root,
        da_commitment,
        effect_commitment,
        proposer: *proposer.address(),
        proposed_at_l1_slot: current_slot,
        challenge_deadline_l1_slot: current_slot
            .checked_add(challenge_window_slots)
            .ok_or(PortalError::ArithmeticOverflow)?,
        status: CheckpointStatus::Pending,
        bond_lamports: CHECKPOINT_PROPOSER_BOND_LAMPORTS,
        bond_status: CheckpointBondStatus::Locked,
        challenger: [0; 32].into(),
        challenged_at_l1_slot: 0,
        challenge_resolved: false,
        bump: checkpoint_bump,
    };
    let rent = Rent::get()?;
    let checkpoint_size = crate::account_size(&checkpoint_state);
    let checkpoint_rent_lamports = rent.try_minimum_balance(checkpoint_size)?;
    if checkpoint.is_data_empty() {
        let checkpoint_lamports = checkpoint_rent_lamports
            .checked_add(CHECKPOINT_PROPOSER_BOND_LAMPORTS)
            .ok_or(PortalError::ArithmeticOverflow)?;
        let er_slot_bytes = er_slot.to_le_bytes();
        let checkpoint_bump_bytes = [checkpoint_bump];
        let checkpoint_seeds = &[
            Seed::from(Checkpoint::SEED_PREFIX),
            Seed::from(session_key.as_ref()),
            Seed::from(er_slot_bytes.as_ref()),
            Seed::from(checkpoint_bump_bytes.as_ref()),
        ];
        let checkpoint_signer = Signer::from(checkpoint_seeds);

        initialize_pda_account(
            proposer,
            checkpoint,
            checkpoint_lamports,
            checkpoint_size as u64,
            program_id,
            checkpoint_signer,
        )?;
    } else {
        let previous_checkpoint = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
        let reusable_terminal = matches!(
            previous_checkpoint.status,
            CheckpointStatus::Cancelled | CheckpointStatus::Invalid
        ) || (previous_checkpoint.status == CheckpointStatus::Settled
            && cursor_state.latest_finalized_checkpoint == [0; 32].into()
            && cursor_state.latest_finalized_er_slot == 0
            && cursor_state.latest_finalized_state_root == [0; 32]);
        if !reusable_terminal {
            return Err(PortalError::CheckpointStateInvalid.into());
        }
        if checkpoint.lamports() < checkpoint_rent_lamports {
            return Err(PortalError::CheckpointBondInsufficient.into());
        }
        Transfer {
            from: proposer,
            to: checkpoint,
            lamports: CHECKPOINT_PROPOSER_BOND_LAMPORTS,
        }
        .invoke()?;
    }

    store_checkpoint(checkpoint, &checkpoint_state)?;

    cursor_state.active_checkpoint = *checkpoint.address();
    cursor_state.active_er_slot = er_slot;
    store_cursor(cursor, &cursor_state)?;

    Ok(())
}

#[p_instruction(
    id = 15,
    accounts = [
        committer(signer),
        session(state = Session),
        checkpoint(mut, state = Checkpoint),
        checkpoint_cursor(mut, state = CheckpointCursor),
        proposer(mut)
    ],
    data = [er_slot: u64]
)]
pub fn process_commit_checkpoint(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    CommitCheckpoint { er_slot }: CommitCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CommitCheckpoint, er_slot={}", er_slot);

    let [committer, session, checkpoint, cursor, proposer, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !committer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let session_state = load_session(program_id, session)?;
    let session_key = session.address();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    let mut cursor_state = load_cursor(program_id, session_key, cursor)?;

    let current_slot = Clock::get()?.slot;
    match checkpoint_state.status {
        CheckpointStatus::Pending => {}
        CheckpointStatus::Challenged => return Err(PortalError::CheckpointChallenged.into()),
        _ => return Err(PortalError::CheckpointStateInvalid.into()),
    }

    if current_slot < checkpoint_state.challenge_deadline_l1_slot {
        return Err(PortalError::CheckpointCommitTooEarly.into());
    }

    require_advancing_checkpoint(&session_state, &cursor_state, er_slot)?;
    require_active_checkpoint(&cursor_state, checkpoint.address(), er_slot)?;
    if checkpoint_state.previous_state_root != cursor_state.latest_finalized_state_root {
        return Err(PortalError::CheckpointPreviousRootMismatch.into());
    }

    checkpoint_state.status = CheckpointStatus::Committed;
    release_checkpoint_bond(checkpoint, proposer, &mut checkpoint_state)?;
    store_checkpoint(checkpoint, &checkpoint_state)?;

    cursor_state.latest_finalized_checkpoint = *checkpoint.address();
    cursor_state.latest_finalized_er_slot = er_slot;
    cursor_state.latest_finalized_state_root = checkpoint_state.new_state_root;
    store_cursor(cursor, &cursor_state)?;

    Ok(())
}

#[p_instruction(
    id = 16,
    accounts = [
        proposer(signer, mut),
        session(state = Session),
        checkpoint(mut, state = Checkpoint),
        checkpoint_cursor(mut, state = CheckpointCursor)
    ],
    data = [er_slot: u64]
)]
pub fn process_cancel_checkpoint(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    CancelCheckpoint { er_slot }: CancelCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CancelCheckpoint, er_slot={}", er_slot);

    let [proposer, session, checkpoint, cursor, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !proposer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let session_key = session.address();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    let mut cursor_state = load_cursor(program_id, session_key, cursor)?;

    if checkpoint_state.status == CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointChallenged.into());
    }
    if checkpoint_state.status != CheckpointStatus::Pending {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    if proposer.address() != &checkpoint_state.proposer {
        return Err(PortalError::CheckpointUnauthorizedCancel.into());
    }
    if Clock::get()?.slot >= checkpoint_state.challenge_deadline_l1_slot {
        return Err(PortalError::CheckpointCancelWindowClosed.into());
    }
    require_active_checkpoint(&cursor_state, checkpoint.address(), er_slot)?;

    checkpoint_state.status = CheckpointStatus::Cancelled;
    release_checkpoint_bond(checkpoint, proposer, &mut checkpoint_state)?;
    store_checkpoint(checkpoint, &checkpoint_state)?;
    clear_active_checkpoint(&mut cursor_state);
    store_cursor(cursor, &cursor_state)?;

    Ok(())
}

#[p_instruction(
    id = 17,
    accounts = [
        challenger(signer, mut),
        session(state = Session),
        checkpoint(mut, state = Checkpoint),
        challenge(mut, state = Challenge),
        da_proof(mut, state = DataAvailabilityProof),
        system_program
    ],
    data = [er_slot: u64]
)]
pub fn process_open_challenge(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    OpenChallenge { er_slot }: OpenChallenge,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: OpenChallenge, er_slot={}", er_slot);

    let [challenger, session, checkpoint, challenge, da_proof, _system_program, ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !challenger.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let session_state = load_session(program_id, session)?;
    let session_key = session.address();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Pending || checkpoint_state.challenge_resolved {
        return Err(PortalError::CheckpointStateInvalid.into());
    }

    let current_slot = Clock::get()?.slot;
    if current_slot >= checkpoint_state.challenge_deadline_l1_slot {
        return Err(PortalError::CheckpointChallengeWindowClosed.into());
    }

    let (expected_challenge_key, challenge_bump) =
        find_challenge_pda(program_id, checkpoint.address());
    if challenge.address() != &expected_challenge_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let challenge_state = Challenge {
        discriminator: Challenge::DISCRIMINATOR,
        checkpoint: *checkpoint.address(),
        challenger: *challenger.address(),
        respondent: session_state.validator,
        opened_at_l1_slot: current_slot,
        hard_deadline_l1_slot: checkpoint_state.challenge_deadline_l1_slot,
        turn_deadline_l1_slot: next_turn_deadline(
            current_slot,
            checkpoint_state.challenge_deadline_l1_slot,
        )?,
        start_step: 0,
        end_step: checkpoint_state.step_count,
        midpoint_step: 0,
        start_state_root: checkpoint_state.previous_state_root,
        end_state_root: checkpoint_state.new_state_root,
        midpoint_state_root: [0; 32],
        status: ChallengeStatus::Active,
        turn: ChallengeTurn::Respondent,
        rounds: 0,
        bump: challenge_bump,
    };
    let challenge_size = crate::account_size(&challenge_state);
    if challenge.is_data_empty() {
        let challenge_bump_bytes = [challenge_bump];
        let challenge_seeds = &[
            Seed::from(Challenge::SEED_PREFIX),
            Seed::from(checkpoint.address().as_ref()),
            Seed::from(challenge_bump_bytes.as_ref()),
        ];
        initialize_pda_account(
            challenger,
            challenge,
            Rent::get()?.try_minimum_balance(challenge_size)?,
            challenge_size as u64,
            program_id,
            Signer::from(challenge_seeds),
        )?;
    } else if load_challenge(program_id, checkpoint.address(), challenge)?.status
        == ChallengeStatus::Active
    {
        return Err(PortalError::CheckpointChallenged.into());
    }

    let (expected_da_key, da_bump) = find_da_proof_pda(program_id, challenge.address());
    if da_proof.address() != &expected_da_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let da_state = DataAvailabilityProof {
        discriminator: DataAvailabilityProof::DISCRIMINATOR,
        challenge: *challenge.address(),
        checkpoint: *checkpoint.address(),
        commitment: checkpoint_state.da_commitment,
        payload_root: [0; 32],
        inclusion_proof_hash: [0; 32],
        revealed_at_l1_slot: 0,
        status: DataAvailabilityStatus::Missing,
        bump: da_bump,
    };
    let da_size = crate::account_size(&da_state);
    if da_proof.is_data_empty() {
        let da_bump_bytes = [da_bump];
        let da_seeds = &[
            Seed::from(DataAvailabilityProof::SEED_PREFIX),
            Seed::from(challenge.address().as_ref()),
            Seed::from(da_bump_bytes.as_ref()),
        ];
        initialize_pda_account(
            challenger,
            da_proof,
            Rent::get()?.try_minimum_balance(da_size)?,
            da_size as u64,
            program_id,
            Signer::from(da_seeds),
        )?;
    } else {
        load_da_proof(
            program_id,
            challenge.address(),
            checkpoint.address(),
            da_proof,
        )?;
    }

    store_challenge(challenge, &challenge_state)?;

    store_da_proof(da_proof, &da_state)?;

    checkpoint_state.status = CheckpointStatus::Challenged;
    checkpoint_state.challenger = *challenger.address();
    checkpoint_state.challenged_at_l1_slot = current_slot;
    store_checkpoint(checkpoint, &checkpoint_state)
}

#[p_instruction(
    id = 23,
    accounts = [
        validator(signer),
        session(state = Session),
        checkpoint(state = Checkpoint),
        challenge(mut, state = Challenge),
        da_proof(mut, state = DataAvailabilityProof)
    ],
    data = [
        er_slot: u64,
        claimed_step: u64,
        claimed_state_root: Hash32,
        da_payload_root: Hash32,
        da_inclusion_proof_hash: Hash32
    ]
)]
pub fn process_respond_challenge(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    RespondChallenge {
        er_slot,
        claimed_step,
        claimed_state_root,
        da_payload_root,
        da_inclusion_proof_hash,
    }: RespondChallenge,
) -> ProgramResult {
    let [validator, session, checkpoint, challenge, da_proof, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !validator.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let session_state = load_session(program_id, session)?;
    if validator.address() != &session_state.validator {
        return Err(PortalError::Unauthorized.into());
    }
    let checkpoint_state = load_checkpoint(program_id, session.address(), er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let mut challenge_state = load_challenge(program_id, checkpoint.address(), challenge)?;
    let current_slot = Clock::get()?.slot;
    require_live_turn(&challenge_state, current_slot)?;
    if challenge_state.turn != ChallengeTurn::Respondent
        || validator.address() != &challenge_state.respondent
    {
        return Err(PortalError::ChallengeTurnInvalid.into());
    }

    let mut da_state = load_da_proof(
        program_id,
        challenge.address(),
        checkpoint.address(),
        da_proof,
    )?;
    if da_state.status == DataAvailabilityStatus::Missing {
        if da_payload_root != da_state.commitment || da_inclusion_proof_hash == [0; 32] {
            return Err(PortalError::DataAvailabilityCommitmentMismatch.into());
        }
        da_state.payload_root = da_payload_root;
        da_state.inclusion_proof_hash = da_inclusion_proof_hash;
        da_state.revealed_at_l1_slot = current_slot;
        da_state.status = DataAvailabilityStatus::Revealed;
        store_da_proof(da_proof, &da_state)?;
    } else if da_state.status != DataAvailabilityStatus::Revealed {
        return Err(PortalError::DataAvailabilityStateInvalid.into());
    } else if da_payload_root != da_state.payload_root
        || da_inclusion_proof_hash != da_state.inclusion_proof_hash
    {
        return Err(PortalError::DataAvailabilityCommitmentMismatch.into());
    }

    let width = challenge_state
        .end_step
        .checked_sub(challenge_state.start_step)
        .ok_or(PortalError::ChallengeResponseInvalid)?;
    if width == 0 {
        return Err(PortalError::ChallengeResponseInvalid.into());
    }
    if width == 1 {
        if claimed_step != challenge_state.start_step
            || claimed_state_root != challenge_state.end_state_root
        {
            return Err(PortalError::ChallengeResponseInvalid.into());
        }
        challenge_state.turn = ChallengeTurn::Prove;
    } else {
        let midpoint = challenge_state
            .start_step
            .checked_add(width / 2)
            .ok_or(PortalError::ArithmeticOverflow)?;
        if claimed_step != midpoint {
            return Err(PortalError::ChallengeResponseInvalid.into());
        }
        challenge_state.midpoint_step = midpoint;
        challenge_state.midpoint_state_root = claimed_state_root;
        challenge_state.turn = ChallengeTurn::Challenger;
    }
    challenge_state.turn_deadline_l1_slot =
        next_turn_deadline(current_slot, challenge_state.hard_deadline_l1_slot)?;
    store_challenge(challenge, &challenge_state)
}

#[p_instruction(
    id = 24,
    accounts = [
        challenger(signer),
        session(state = Session),
        checkpoint(state = Checkpoint),
        challenge(mut, state = Challenge)
    ],
    data = [er_slot: u64, dispute_upper: bool]
)]
pub fn process_bisect_challenge(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    BisectChallenge {
        er_slot,
        dispute_upper,
    }: BisectChallenge,
) -> ProgramResult {
    let [challenger, session, checkpoint, challenge, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !challenger.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let checkpoint_state = load_checkpoint(program_id, session.address(), er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let mut state = load_challenge(program_id, checkpoint.address(), challenge)?;
    let current_slot = Clock::get()?.slot;
    require_live_turn(&state, current_slot)?;
    if state.turn != ChallengeTurn::Challenger || challenger.address() != &state.challenger {
        return Err(PortalError::ChallengeTurnInvalid.into());
    }
    if state.midpoint_step <= state.start_step || state.midpoint_step >= state.end_step {
        return Err(PortalError::ChallengeResponseInvalid.into());
    }

    if dispute_upper {
        state.start_step = state.midpoint_step;
        state.start_state_root = state.midpoint_state_root;
    } else {
        state.end_step = state.midpoint_step;
        state.end_state_root = state.midpoint_state_root;
    }
    state.midpoint_step = 0;
    state.midpoint_state_root = [0; 32];
    state.turn = ChallengeTurn::Respondent;
    state.rounds = state
        .rounds
        .checked_add(1)
        .ok_or(PortalError::ArithmeticOverflow)?;
    state.turn_deadline_l1_slot = next_turn_deadline(current_slot, state.hard_deadline_l1_slot)?;
    store_challenge(challenge, &state)
}

#[p_instruction(
    id = 25,
    accounts = [
        caller(signer),
        session(state = Session),
        checkpoint(mut, state = Checkpoint),
        challenge(mut, state = Challenge),
        da_proof(mut, state = DataAvailabilityProof),
        bond_recipient(mut),
        checkpoint_cursor(mut, state = CheckpointCursor)
    ],
    data = [er_slot: u64]
)]
pub fn process_timeout_challenge(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    TimeoutChallenge { er_slot }: TimeoutChallenge,
) -> ProgramResult {
    let [caller, session, checkpoint, challenge, da_proof, bond_recipient, cursor, ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !caller.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let mut checkpoint_state = load_checkpoint(program_id, session.address(), er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let mut challenge_state = load_challenge(program_id, checkpoint.address(), challenge)?;
    if challenge_state.status != ChallengeStatus::Active {
        return Err(PortalError::ChallengeStateInvalid.into());
    }
    let current_slot = Clock::get()?.slot;
    if current_slot < challenge_state.turn_deadline_l1_slot
        && current_slot < challenge_state.hard_deadline_l1_slot
    {
        return Err(PortalError::ChallengeDeadlineNotReached.into());
    }
    if bond_recipient.address() != &challenge_state.challenger {
        return Err(PortalError::Unauthorized.into());
    }
    let mut cursor_state = load_cursor(program_id, session.address(), cursor)?;
    require_active_checkpoint(&cursor_state, checkpoint.address(), er_slot)?;
    let mut da_state = load_da_proof(
        program_id,
        challenge.address(),
        checkpoint.address(),
        da_proof,
    )?;

    if challenge_state.turn == ChallengeTurn::Respondent {
        if da_state.status == DataAvailabilityStatus::Missing {
            da_state.status = DataAvailabilityStatus::Defaulted;
            store_da_proof(da_proof, &da_state)?;
        }
        slash_checkpoint_bond(checkpoint, bond_recipient, &mut checkpoint_state)?;
        checkpoint_state.status = CheckpointStatus::Invalid;
        checkpoint_state.challenge_resolved = true;
        challenge_state.status = ChallengeStatus::ChallengerWon;
        clear_active_checkpoint(&mut cursor_state);
        store_cursor(cursor, &cursor_state)?;
    } else {
        checkpoint_state.status = CheckpointStatus::Pending;
        checkpoint_state.challenge_resolved = true;
        challenge_state.status = ChallengeStatus::ValidatorWon;
    }
    store_checkpoint(checkpoint, &checkpoint_state)?;
    store_challenge(challenge, &challenge_state)
}

#[p_instruction(
    id = 18,
    accounts = [
        authority(signer, mut),
        session(state = Session),
        checkpoint(state = Checkpoint),
        challenge(state = Challenge),
        step_proof(mut, state = StepProofAccount),
        system_program
    ],
    data = [
        er_slot: u64,
        proof_kind: u8,
        proof_version: u8,
        step_index: u64,
        tx_effect_root: Hash32,
        readonly_l1_root: Hash32,
        settlement_effect_root: Hash32
    ]
)]
pub fn process_create_step_proof(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    CreateStepProof {
        er_slot,
        proof_kind,
        proof_version,
        step_index,
        tx_effect_root,
        readonly_l1_root,
        settlement_effect_root,
    }: CreateStepProof,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CreateStepProof, er_slot={}", er_slot);

    let [authority, session, checkpoint, challenge, proof, _system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let session_key = session.address();
    let checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let challenge_state = load_challenge(program_id, checkpoint.address(), challenge)?;
    require_live_turn(&challenge_state, Clock::get()?.slot)?;
    if challenge_state.turn != ChallengeTurn::Prove
        || challenge_state.end_step != challenge_state.start_step.saturating_add(1)
        || step_index != challenge_state.start_step
    {
        return Err(PortalError::ChallengeTurnInvalid.into());
    }
    if proof_kind == 0 || proof_version == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if authority.address() != &challenge_state.challenger
        || readonly_l1_root != checkpoint_state.readonly_l1_root
        || settlement_effect_root != checkpoint_state.effect_commitment
    {
        return Err(PortalError::Unauthorized.into());
    }

    let (expected_proof_key, proof_bump) = find_step_proof_pda(program_id, checkpoint.address());
    if proof.address() != &expected_proof_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let mut proof_state = StepProofAccount {
        discriminator: StepProofAccount::DISCRIMINATOR,
        checkpoint: *checkpoint.address(),
        challenge: *challenge.address(),
        authority: *authority.address(),
        proof_kind,
        proof_version,
        step_index,
        tx_effect_root,
        readonly_l1_root,
        settlement_effect_root,
        public_input_hash: [0; 32],
        written_len: 0,
        sealed: false,
        proof_hash: [0; 32],
        bump: proof_bump,
        data: [0; MAX_STEP_PROOF_BYTES],
    };
    proof_state.public_input_hash = step_proof_public_input_hash(
        program_id,
        session_key,
        checkpoint.address(),
        &checkpoint_state,
        &challenge_state,
        &proof_state,
    );
    let rent = Rent::get()?;
    let proof_size = crate::account_size(&proof_state);
    let proof_lamports = rent.try_minimum_balance(proof_size)?;
    if proof.is_data_empty() {
        let proof_bump_bytes = [proof_bump];
        let proof_seeds = &[
            Seed::from(StepProofAccount::SEED_PREFIX),
            Seed::from(checkpoint.address().as_ref()),
            Seed::from(proof_bump_bytes.as_ref()),
        ];
        initialize_pda_account(
            authority,
            proof,
            proof_lamports,
            proof_size as u64,
            program_id,
            Signer::from(proof_seeds),
        )?;
    } else if !proof.owned_by(program_id) || proof.lamports() < proof_lamports {
        return Err(PortalError::StepProofStateInvalid.into());
    }

    store_step_proof(proof, &proof_state)
}

#[p_instruction(
    id = 19,
    accounts = [
        authority(signer),
        session(state = Session),
        checkpoint(state = Checkpoint),
        challenge(state = Challenge),
        step_proof(mut, state = StepProofAccount)
    ],
    data = [
        er_slot: u64,
        offset: u32,
        chunk_len: u16,
        chunk: [u8; 128]
    ]
)]
pub fn process_write_step_proof(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    WriteStepProof {
        er_slot,
        offset,
        chunk_len,
        chunk,
    }: WriteStepProof,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: WriteStepProof, er_slot={}", er_slot);

    let [authority, session, checkpoint, challenge, proof, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let checkpoint_state = load_checkpoint(program_id, session.address(), er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let challenge_state = load_challenge(program_id, checkpoint.address(), challenge)?;
    require_live_turn(&challenge_state, Clock::get()?.slot)?;
    if challenge_state.turn != ChallengeTurn::Prove {
        return Err(PortalError::ChallengeTurnInvalid.into());
    }

    let mut proof_state = load_step_proof(program_id, checkpoint.address(), proof)?;
    if proof_state.challenge != *challenge.address() {
        return Err(PortalError::StepProofCheckpointMismatch.into());
    }
    if proof_state.authority != *authority.address() {
        return Err(PortalError::Unauthorized.into());
    }
    if proof_state.sealed {
        return Err(PortalError::StepProofAlreadySealed.into());
    }

    let chunk_len = chunk_len as usize;
    let offset = offset as usize;
    let end = offset
        .checked_add(chunk_len)
        .ok_or(PortalError::StepProofChunkOutOfBounds)?;
    if end > MAX_STEP_PROOF_BYTES || chunk_len > chunk.len() {
        return Err(PortalError::StepProofChunkOutOfBounds.into());
    }
    proof_state.data[offset..end].copy_from_slice(&chunk[..chunk_len]);
    proof_state.written_len = proof_state.written_len.max(end as u32);
    store_step_proof(proof, &proof_state)?;

    Ok(())
}

#[p_instruction(
    id = 20,
    accounts = [
        authority(signer),
        session(state = Session),
        checkpoint(state = Checkpoint),
        challenge(state = Challenge),
        step_proof(mut, state = StepProofAccount)
    ],
    data = [er_slot: u64, proof_len: u32]
)]
pub fn process_seal_step_proof(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    SealStepProof { er_slot, proof_len }: SealStepProof,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: SealStepProof, er_slot={}", er_slot);

    let [authority, session, checkpoint, challenge, proof, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let checkpoint_state = load_checkpoint(program_id, session.address(), er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let challenge_state = load_challenge(program_id, checkpoint.address(), challenge)?;
    require_live_turn(&challenge_state, Clock::get()?.slot)?;
    if challenge_state.turn != ChallengeTurn::Prove {
        return Err(PortalError::ChallengeTurnInvalid.into());
    }

    let mut proof_state = load_step_proof(program_id, checkpoint.address(), proof)?;
    if proof_state.challenge != *challenge.address() {
        return Err(PortalError::StepProofCheckpointMismatch.into());
    }
    if proof_state.authority != *authority.address() {
        return Err(PortalError::Unauthorized.into());
    }
    if proof_state.sealed {
        return Err(PortalError::StepProofAlreadySealed.into());
    }
    if proof_len == 0 || proof_len > proof_state.written_len {
        return Err(PortalError::StepProofChunkOutOfBounds.into());
    }

    proof_state.written_len = proof_len;
    proof_state.sealed = true;
    proof_state.proof_hash = hashv(&[&proof_state.data[..proof_len as usize]]).to_bytes();
    store_step_proof(proof, &proof_state)?;

    Ok(())
}

#[allow(dead_code)]
enum StepProofVerification {
    Valid,
    Invalid,
    Unavailable,
}

fn verify_step_proof(
    verifier_mode: StepProofVerifierMode,
    proof_state: &StepProofAccount,
) -> StepProofVerification {
    match verifier_mode {
        StepProofVerifierMode::Production => StepProofVerification::Unavailable,
        StepProofVerifierMode::TestOnly => verify_step_proof_test_only(proof_state),
    }
}

fn verify_step_proof_test_only(proof_state: &StepProofAccount) -> StepProofVerification {
    // Dummy verifier is for program-test only. Solana/SBF builds need the explicit
    // build-script escape hatch, otherwise `test-verifier` fails compilation.
    #[cfg(feature = "test-verifier")]
    {
        match proof_state.data.first().copied() {
            Some(2) => StepProofVerification::Valid,
            Some(1) => StepProofVerification::Invalid,
            _ => StepProofVerification::Unavailable,
        }
    }
    #[cfg(not(feature = "test-verifier"))]
    {
        let _ = proof_state;
        StepProofVerification::Unavailable
    }
}

#[p_instruction(
    id = 21,
    accounts = [
        submitter(signer),
        session(state = Session),
        checkpoint(mut, state = Checkpoint),
        challenge(mut, state = Challenge),
        da_proof(state = DataAvailabilityProof),
        step_proof(state = StepProofAccount),
        bond_recipient(mut),
        checkpoint_cursor(mut, state = CheckpointCursor)
    ],
    data = [er_slot: u64, verifier_mode: StepProofVerifierMode]
)]
pub fn process_resolve_challenge(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    ResolveChallenge {
        er_slot,
        verifier_mode,
    }: ResolveChallenge,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: ResolveChallenge, er_slot={}", er_slot);

    let [submitter, session, checkpoint, challenge, da_proof, proof, bond_recipient, cursor, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !submitter.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let session_key = session.address();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let mut challenge_state = load_challenge(program_id, checkpoint.address(), challenge)?;
    require_live_turn(&challenge_state, Clock::get()?.slot)?;
    if challenge_state.turn != ChallengeTurn::Prove
        || challenge_state.end_step != challenge_state.start_step.saturating_add(1)
    {
        return Err(PortalError::ChallengeTurnInvalid.into());
    }
    let da_state = load_da_proof(
        program_id,
        challenge.address(),
        checkpoint.address(),
        da_proof,
    )?;
    if da_state.status != DataAvailabilityStatus::Revealed {
        return Err(PortalError::DataAvailabilityStateInvalid.into());
    }
    let mut cursor_state = load_cursor(program_id, session_key, cursor)?;
    require_active_checkpoint(&cursor_state, checkpoint.address(), er_slot)?;

    let proof_state = load_step_proof(program_id, checkpoint.address(), proof)?;
    if proof_state.checkpoint != *checkpoint.address()
        || proof_state.challenge != *challenge.address()
    {
        return Err(PortalError::StepProofCheckpointMismatch.into());
    }
    if proof_state.authority != challenge_state.challenger {
        return Err(PortalError::Unauthorized.into());
    }
    if !proof_state.sealed {
        return Err(PortalError::StepProofNotSealed.into());
    }
    let proof_len = proof_state.written_len as usize;
    if proof_len == 0 || proof_len > MAX_STEP_PROOF_BYTES {
        return Err(PortalError::StepProofChunkOutOfBounds.into());
    }
    let proof_hash = hashv(&[&proof_state.data[..proof_len]]).to_bytes();
    if proof_hash != proof_state.proof_hash {
        return Err(PortalError::StepProofHashMismatch.into());
    }
    if proof_state.public_input_hash
        != step_proof_public_input_hash(
            program_id,
            session_key,
            checkpoint.address(),
            &checkpoint_state,
            &challenge_state,
            &proof_state,
        )
    {
        return Err(PortalError::StepProofPublicInputMismatch.into());
    }

    match verify_step_proof(verifier_mode, &proof_state) {
        StepProofVerification::Unavailable => Err(PortalError::StepProofVerifierUnavailable.into()),
        StepProofVerification::Invalid => {
            if bond_recipient.address() != &challenge_state.challenger {
                return Err(PortalError::Unauthorized.into());
            }
            slash_checkpoint_bond(checkpoint, bond_recipient, &mut checkpoint_state)?;
            checkpoint_state.status = CheckpointStatus::Invalid;
            checkpoint_state.challenge_resolved = true;
            challenge_state.status = ChallengeStatus::ChallengerWon;
            store_checkpoint(checkpoint, &checkpoint_state)?;
            store_challenge(challenge, &challenge_state)?;
            clear_active_checkpoint(&mut cursor_state);
            store_cursor(cursor, &cursor_state)
        }
        StepProofVerification::Valid => {
            checkpoint_state.status = CheckpointStatus::Pending;
            checkpoint_state.challenge_resolved = true;
            challenge_state.status = ChallengeStatus::ValidatorWon;
            store_checkpoint(checkpoint, &checkpoint_state)?;
            store_challenge(challenge, &challenge_state)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_proof_public_input_hash_v1_is_stable() {
        let checkpoint = Checkpoint {
            discriminator: Checkpoint::DISCRIMINATOR,
            session: [2; 32].into(),
            er_slot: 5,
            step_count: 10,
            previous_state_root: [0; 32],
            new_state_root: [0; 32],
            trace_root: [0; 32],
            tx_effect_root: [0; 32],
            readonly_l1_root: [7; 32],
            da_commitment: [0; 32],
            effect_commitment: [8; 32],
            proposer: [0; 32].into(),
            proposed_at_l1_slot: 0,
            challenge_deadline_l1_slot: 0,
            status: CheckpointStatus::Challenged,
            bond_lamports: 1,
            bond_status: CheckpointBondStatus::Locked,
            challenger: [0; 32].into(),
            challenged_at_l1_slot: 0,
            challenge_resolved: false,
            bump: 0,
        };
        let challenge = Challenge {
            discriminator: Challenge::DISCRIMINATOR,
            checkpoint: [3; 32].into(),
            challenger: [0; 32].into(),
            respondent: [0; 32].into(),
            opened_at_l1_slot: 0,
            hard_deadline_l1_slot: 0,
            turn_deadline_l1_slot: 0,
            start_step: 7,
            end_step: 8,
            midpoint_step: 0,
            start_state_root: [4; 32],
            end_state_root: [5; 32],
            midpoint_state_root: [0; 32],
            status: ChallengeStatus::Active,
            turn: ChallengeTurn::Prove,
            rounds: 0,
            bump: 0,
        };
        let proof = StepProofAccount {
            discriminator: StepProofAccount::DISCRIMINATOR,
            checkpoint: [3; 32].into(),
            challenge: [0; 32].into(),
            authority: [0; 32].into(),
            proof_kind: 1,
            proof_version: 1,
            step_index: 7,
            tx_effect_root: [6; 32],
            readonly_l1_root: [7; 32],
            settlement_effect_root: [8; 32],
            public_input_hash: [0; 32],
            written_len: 0,
            sealed: false,
            proof_hash: [0; 32],
            bump: 0,
            data: [0; MAX_STEP_PROOF_BYTES],
        };

        assert_eq!(
            step_proof_public_input_hash(
                &Pubkey::from([1; 32]),
                &Pubkey::from([2; 32]),
                &Pubkey::from([3; 32]),
                &checkpoint,
                &challenge,
                &proof,
            ),
            [
                0xd5, 0xf5, 0x13, 0x29, 0xde, 0x7a, 0x7f, 0x56, 0x66, 0x15, 0x20, 0xef, 0x99, 0xc7,
                0x18, 0x91, 0x96, 0x87, 0x7b, 0xbc, 0xa4, 0x12, 0xea, 0x03, 0xdf, 0x79, 0x4a, 0x17,
                0x92, 0x3c, 0x05, 0x44,
            ]
        );
    }
}
