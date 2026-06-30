use {
    crate::{
        find_checkpoint_cursor_pda, find_checkpoint_pda, find_session_pda, CancelCheckpoint,
        Checkpoint, CheckpointBondStatus, CheckpointCursor, CheckpointStatus, CommitCheckpoint,
        PortalError, ProposeCheckpoint, Session, CHECKPOINT_PROPOSER_BOND_LAMPORTS,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        account_info::AccountInfo,
        instruction::{Seed, Signer},
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        ProgramResult,
    },
    pinocchio_system::instructions::CreateAccount,
};

fn load_session(program_id: &Pubkey, session: &AccountInfo) -> Result<Session, ProgramError> {
    let (expected_session_key, _) = find_session_pda(program_id);
    if session.key() != &expected_session_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if session.owner() != program_id {
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }
    let session_state = Session::try_from_slice(&session.try_borrow_data()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !session_state.is_valid() {
        return Err(PortalError::SessionStateInvalid.into());
    }
    Ok(session_state)
}

fn store_checkpoint(checkpoint: &AccountInfo, checkpoint_state: &Checkpoint) -> ProgramResult {
    let mut checkpoint_data = checkpoint.try_borrow_mut_data()?;
    BorshSerialize::serialize(
        checkpoint_state,
        &mut &mut checkpoint_data[..Checkpoint::LEN],
    )
    .unwrap();
    Ok(())
}

fn store_cursor(cursor: &AccountInfo, cursor_state: &CheckpointCursor) -> ProgramResult {
    let mut cursor_data = cursor.try_borrow_mut_data()?;
    BorshSerialize::serialize(cursor_state, &mut &mut cursor_data[..CheckpointCursor::LEN])
        .unwrap();
    Ok(())
}

fn resolve_checkpoint_bond(
    checkpoint: &AccountInfo,
    recipient: &AccountInfo,
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

    let rent_lamports = Rent::get()?.minimum_balance(Checkpoint::LEN);
    let minimum_locked_lamports = rent_lamports
        .checked_add(bond_lamports)
        .ok_or(PortalError::ArithmeticOverflow)?;
    if checkpoint.lamports() < minimum_locked_lamports {
        return Err(PortalError::CheckpointBondInsufficient.into());
    }

    {
        let mut recipient_lamports = recipient.try_borrow_mut_lamports()?;
        *recipient_lamports = recipient_lamports
            .checked_add(bond_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
    }
    *checkpoint.try_borrow_mut_lamports()? = checkpoint
        .lamports()
        .checked_sub(bond_lamports)
        .ok_or(PortalError::ArithmeticOverflow)?;
    checkpoint_state.bond_status = next_status;

    Ok(())
}

fn release_checkpoint_bond(
    checkpoint: &AccountInfo,
    proposer: &AccountInfo,
    checkpoint_state: &mut Checkpoint,
) -> ProgramResult {
    if proposer.key() != &checkpoint_state.proposer {
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
    checkpoint: &AccountInfo,
    recipient: &AccountInfo,
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
    if checkpoint.key() != &expected_checkpoint_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if checkpoint.owner() != program_id {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let checkpoint_state = Checkpoint::try_from_slice(&checkpoint.try_borrow_data()?)
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
    if cursor.key() != &expected_cursor_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if cursor.owner() != program_id {
        return Err(PortalError::CheckpointCursorStateInvalid.into());
    }
    let cursor_state = CheckpointCursor::try_from_slice(&cursor.try_borrow_data()?)
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
    cursor: &AccountInfo,
) -> Result<CheckpointCursor, ProgramError> {
    let (expected_cursor_key, cursor_bump) = find_checkpoint_cursor_pda(program_id, session_key);
    if cursor.key() != &expected_cursor_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let rent = Rent::get()?;
    let cursor_lamports = rent.minimum_balance(CheckpointCursor::LEN);
    let cursor_bump_bytes = [cursor_bump];
    let cursor_seeds = &[
        Seed::from(CheckpointCursor::SEED_PREFIX),
        Seed::from(session_key.as_ref()),
        Seed::from(cursor_bump_bytes.as_ref()),
    ];
    let cursor_signer = Signer::from(cursor_seeds);

    CreateAccount {
        from: payer,
        to: cursor,
        lamports: cursor_lamports,
        space: CheckpointCursor::LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[cursor_signer])?;

    let cursor_state = CheckpointCursor {
        discriminator: CheckpointCursor::DISCRIMINATOR,
        session: *session_key,
        latest_finalized_checkpoint: [0; 32],
        latest_finalized_er_slot: 0,
        bump: cursor_bump,
    };
    store_cursor(cursor, &cursor_state)?;
    Ok(cursor_state)
}

fn load_or_create_cursor(
    program_id: &Pubkey,
    payer: &AccountInfo,
    session_key: &Pubkey,
    cursor: &AccountInfo,
) -> Result<CheckpointCursor, ProgramError> {
    if cursor.data_is_empty() {
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

pub fn process_propose_checkpoint(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    ProposeCheckpoint {
        er_slot,
        previous_state_root,
        new_state_root,
        effect_commitment,
        challenge_window_slots,
    }: ProposeCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: ProposeCheckpoint, er_slot={}", er_slot);

    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    if challenge_window_slots == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let proposer = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];
    let cursor = &accounts[3];
    let _system_program = &accounts[4];

    if !proposer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let session_state = load_session(program_id, session)?;
    if proposer.key() != &session_state.validator {
        return Err(PortalError::Unauthorized.into());
    }

    let current_slot = Clock::get()?.slot;
    if session_state.is_expired(current_slot) {
        return Err(PortalError::SessionExpired.into());
    }

    let session_key = session.key();
    let cursor_state = load_or_create_cursor(program_id, proposer, session_key, cursor)?;
    require_advancing_checkpoint(&session_state, &cursor_state, er_slot)?;

    if !checkpoint.data_is_empty() {
        return Err(PortalError::CheckpointStateInvalid.into());
    }

    let (expected_checkpoint_key, checkpoint_bump) =
        find_checkpoint_pda(program_id, session_key, er_slot);
    if checkpoint.key() != &expected_checkpoint_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let rent = Rent::get()?;
    let checkpoint_lamports = rent
        .minimum_balance(Checkpoint::LEN)
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

    CreateAccount {
        from: proposer,
        to: checkpoint,
        lamports: checkpoint_lamports,
        space: Checkpoint::LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[checkpoint_signer])?;

    let checkpoint_state = Checkpoint {
        discriminator: Checkpoint::DISCRIMINATOR,
        session: *session_key,
        er_slot,
        previous_state_root,
        new_state_root,
        effect_commitment,
        proposer: *proposer.key(),
        proposed_at_l1_slot: current_slot,
        challenge_deadline_l1_slot: current_slot
            .checked_add(challenge_window_slots)
            .ok_or(PortalError::ArithmeticOverflow)?,
        status: CheckpointStatus::Pending,
        bond_lamports: CHECKPOINT_PROPOSER_BOND_LAMPORTS,
        bond_status: CheckpointBondStatus::Locked,
        bump: checkpoint_bump,
    };
    store_checkpoint(checkpoint, &checkpoint_state)?;

    Ok(())
}

pub fn process_commit_checkpoint(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    CommitCheckpoint { er_slot }: CommitCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CommitCheckpoint, er_slot={}", er_slot);

    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let committer = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];
    let cursor = &accounts[3];
    let proposer = &accounts[4];

    if !committer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let session_state = load_session(program_id, session)?;
    let session_key = session.key();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    let mut cursor_state = load_cursor(program_id, session_key, cursor)?;

    if checkpoint_state.status != CheckpointStatus::Pending {
        return Err(PortalError::CheckpointStateInvalid.into());
    }

    let current_slot = Clock::get()?.slot;
    if current_slot < checkpoint_state.challenge_deadline_l1_slot {
        return Err(PortalError::CheckpointCommitTooEarly.into());
    }

    require_advancing_checkpoint(&session_state, &cursor_state, er_slot)?;

    checkpoint_state.status = CheckpointStatus::Committed;
    release_checkpoint_bond(checkpoint, proposer, &mut checkpoint_state)?;
    store_checkpoint(checkpoint, &checkpoint_state)?;

    cursor_state.latest_finalized_checkpoint = *checkpoint.key();
    cursor_state.latest_finalized_er_slot = er_slot;
    store_cursor(cursor, &cursor_state)?;

    Ok(())
}

pub fn process_cancel_checkpoint(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    CancelCheckpoint { er_slot }: CancelCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CancelCheckpoint, er_slot={}", er_slot);

    if accounts.len() < 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let proposer = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];

    if !proposer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let session_key = session.key();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;

    if checkpoint_state.status != CheckpointStatus::Pending {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    if proposer.key() != &checkpoint_state.proposer {
        return Err(PortalError::CheckpointUnauthorizedCancel.into());
    }

    checkpoint_state.status = CheckpointStatus::Cancelled;
    release_checkpoint_bond(checkpoint, proposer, &mut checkpoint_state)?;
    store_checkpoint(checkpoint, &checkpoint_state)?;

    Ok(())
}
