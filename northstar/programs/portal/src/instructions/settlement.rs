use {
    crate::{
        BeginSettlement, FinishSettlement, MAX_SETTLEMENT_CHUNK, PortalError, Session,
        SettlementStatus, WriteSettlementChunk, find_delegation_record_pda, find_session_pda,
        state::DelegationRecord,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        ProgramResult,
        account_info::AccountInfo,
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{Sysvar, clock::Clock},
    },
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

fn store_session(session: &AccountInfo, session_state: &Session) -> ProgramResult {
    let mut session_data = session.try_borrow_mut_data()?;
    BorshSerialize::serialize(session_state, &mut &mut session_data[..Session::LEN]).unwrap();
    Ok(())
}

fn require_validator(validator: &AccountInfo, session_state: &Session) -> ProgramResult {
    if !validator.is_signer() || validator.key() != &session_state.validator {
        return Err(PortalError::Unauthorized.into());
    }
    Ok(())
}

pub fn process_begin_settlement(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    BeginSettlement { er_slot, checksum }: BeginSettlement,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: BeginSettlement, er_slot={}", er_slot);

    if accounts.len() < 2 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let validator = &accounts[0];
    let session = &accounts[1];
    let mut session_state = load_session(program_id, session)?;
    require_validator(validator, &session_state)?;

    if session_state.settlement_status == SettlementStatus::InProgress {
        return Err(PortalError::SettlementInProgress.into());
    }

    let current_slot = Clock::get()?.slot;
    let next_settlement_slot = session_state
        .last_settled_l1_slot
        .saturating_add(session_state.settlement_interval_slots);
    if current_slot < next_settlement_slot {
        return Err(PortalError::SettlementTooEarly.into());
    }

    if er_slot <= session_state.last_settled_er_slot {
        return Err(PortalError::SettlementErSlotNotAdvanced.into());
    }

    session_state.settlement_status = SettlementStatus::InProgress;
    session_state.settlement_er_slot = er_slot;
    session_state.settlement_checksum = checksum;
    session_state.settlement_started_l1_slot = current_slot;
    store_session(session, &session_state)?;

    Ok(())
}

pub fn process_write_settlement_chunk(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    WriteSettlementChunk {
        er_slot,
        checksum,
        account_data_offset,
        chunk_len,
        chunk,
    }: WriteSettlementChunk,
) -> ProgramResult {
    if accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let validator = &accounts[0];
    let session = &accounts[1];
    let delegated_account = &accounts[2];
    let delegation_record = &accounts[3];

    let session_state = load_session(program_id, session)?;
    require_validator(validator, &session_state)?;

    if session_state.settlement_status != SettlementStatus::InProgress {
        return Err(PortalError::SettlementNotInProgress.into());
    }
    if er_slot != session_state.settlement_er_slot {
        return Err(PortalError::SettlementErSlotMismatch.into());
    }
    if checksum != session_state.settlement_checksum {
        return Err(PortalError::SettlementChecksumMismatch.into());
    }

    if delegated_account.owner() != program_id {
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    let delegated_key = *delegated_account.key();
    let (expected_record, _) = find_delegation_record_pda(program_id, &delegated_key);
    if delegation_record.key() != &expected_record {
        return Err(PortalError::DelegationRecordAccountMismatch.into());
    }
    if delegation_record.owner() != program_id {
        return Err(PortalError::DelegationRecordStateInvalid.into());
    }

    let delegation_state = DelegationRecord::try_from_slice(&delegation_record.try_borrow_data()?)
        .map_err(|_| PortalError::DelegationRecordDeserializeFailed)?;
    if !delegation_state.is_valid() || delegation_state.grid_id != session_state.grid_id {
        return Err(PortalError::DelegationRecordStateInvalid.into());
    }

    let chunk_len = chunk_len as usize;
    if chunk_len > MAX_SETTLEMENT_CHUNK {
        return Err(PortalError::SettlementChunkTooLarge.into());
    }
    let start = account_data_offset as usize;
    let end = start
        .checked_add(chunk_len)
        .ok_or(PortalError::SettlementChunkOutOfBounds)?;
    if end > delegated_account.data_len() {
        return Err(PortalError::SettlementChunkOutOfBounds.into());
    }

    let mut delegated_data = delegated_account.try_borrow_mut_data()?;
    delegated_data[start..end].copy_from_slice(&chunk[..chunk_len]);

    Ok(())
}

pub fn process_finish_settlement(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    FinishSettlement { er_slot, checksum }: FinishSettlement,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: FinishSettlement, er_slot={}", er_slot);

    if accounts.len() < 2 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let validator = &accounts[0];
    let session = &accounts[1];
    let mut session_state = load_session(program_id, session)?;
    require_validator(validator, &session_state)?;

    if session_state.settlement_status != SettlementStatus::InProgress {
        return Err(PortalError::SettlementNotInProgress.into());
    }
    if er_slot != session_state.settlement_er_slot {
        return Err(PortalError::SettlementErSlotMismatch.into());
    }
    if checksum != session_state.settlement_checksum {
        return Err(PortalError::SettlementChecksumMismatch.into());
    }

    session_state.last_settled_l1_slot = Clock::get()?.slot;
    session_state.last_settled_er_slot = er_slot;
    session_state.settlement_status = SettlementStatus::Idle;
    session_state.settlement_er_slot = 0;
    session_state.settlement_checksum = [0; 32];
    session_state.settlement_started_l1_slot = 0;
    store_session(session, &session_state)?;

    Ok(())
}

pub fn process_abort_settlement(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    pinocchio_log::log!("Instruction: AbortSettlement");

    if accounts.len() < 2 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority_or_validator = &accounts[0];
    let session = &accounts[1];
    let mut session_state = load_session(program_id, session)?;

    if !authority_or_validator.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }
    if session_state.settlement_status != SettlementStatus::InProgress {
        return Err(PortalError::SettlementNotInProgress.into());
    }

    let is_validator = authority_or_validator.key() == &session_state.validator;
    let is_timed_out_authority = authority_or_validator.key() == &session_state.authority
        && Clock::get()?.slot
            > session_state
                .settlement_started_l1_slot
                .saturating_add(session_state.settlement_interval_slots);
    if !is_validator && !is_timed_out_authority {
        return Err(PortalError::SettlementUnauthorizedAbort.into());
    }

    session_state.settlement_status = SettlementStatus::Idle;
    session_state.settlement_er_slot = 0;
    session_state.settlement_checksum = [0; 32];
    session_state.settlement_started_l1_slot = 0;
    store_session(session, &session_state)?;

    Ok(())
}
