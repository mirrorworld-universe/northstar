use {
    crate::{
        BeginSettlement, FinishSettlement, MAX_SETTLEMENT_CHUNK, MAX_SETTLEMENT_LAMPORT_ACCOUNTS,
        PortalError, Session, SettleAccountLamports, SettleAccountOwner, SettlementStatus,
        WriteSettlementChunk, find_delegation_record_pda, find_session_pda,
        state::DelegationRecord,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        ProgramResult,
        account_info::AccountInfo,
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{Sysvar, clock::Clock, rent::Rent},
    },
    solana_sha256_hasher::hashv,
};

const SETTLEMENT_CHECKSUM_DOMAIN: &[u8] = b"northstar-settlement-v0";

// Sonic: Must stay byte-identical to northstar/src/settlement.rs helpers.
pub(crate) fn initial_settlement_checksum(er_slot: u64) -> [u8; 32] {
    hashv(&[SETTLEMENT_CHECKSUM_DOMAIN, &er_slot.to_le_bytes()]).to_bytes()
}

pub(crate) fn accumulate_data_chunk_checksum(
    accumulator: [u8; 32],
    account: &Pubkey,
    account_data_offset: u32,
    data: &[u8],
) -> [u8; 32] {
    hashv(&[
        &accumulator,
        b"data",
        account,
        &account_data_offset.to_le_bytes(),
        &(data.len() as u32).to_le_bytes(),
        data,
    ])
    .to_bytes()
}

pub(crate) fn accumulate_owner_checksum(
    accumulator: [u8; 32],
    account: &Pubkey,
    owner: &Pubkey,
) -> [u8; 32] {
    hashv(&[&accumulator, b"owner", account, owner]).to_bytes()
}

pub(crate) fn accumulate_lamports_checksum(
    accumulator: [u8; 32],
    account: &Pubkey,
    lamports: u64,
) -> [u8; 32] {
    hashv(&[&accumulator, b"lamports", account, &lamports.to_le_bytes()]).to_bytes()
}

pub(crate) fn accumulate_receipt_checksum(
    accumulator: [u8; 32],
    recipient: &Pubkey,
    balance: u64,
    withdrawn: u64,
) -> [u8; 32] {
    hashv(&[
        &accumulator,
        b"receipt",
        recipient,
        &balance.to_le_bytes(),
        &withdrawn.to_le_bytes(),
    ])
    .to_bytes()
}

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

fn require_active_settlement(
    session_state: &Session,
    er_slot: u64,
    checksum: [u8; 32],
) -> ProgramResult {
    if session_state.settlement_status != SettlementStatus::InProgress {
        return Err(PortalError::SettlementNotInProgress.into());
    }
    if er_slot != session_state.settlement_er_slot {
        return Err(PortalError::SettlementErSlotMismatch.into());
    }
    if checksum != session_state.settlement_checksum {
        return Err(PortalError::SettlementChecksumMismatch.into());
    }
    Ok(())
}

fn load_delegation_record(
    program_id: &Pubkey,
    session_state: &Session,
    delegated_account: &AccountInfo,
    delegation_record: &AccountInfo,
) -> Result<DelegationRecord, ProgramError> {
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

    Ok(delegation_state)
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
    session_state.settlement_accumulator = initial_settlement_checksum(er_slot);
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

    let mut session_state = load_session(program_id, session)?;
    require_validator(validator, &session_state)?;

    require_active_settlement(&session_state, er_slot, checksum)?;
    load_delegation_record(
        program_id,
        &session_state,
        delegated_account,
        delegation_record,
    )?;

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

    let chunk_data = &chunk[..chunk_len];
    let mut delegated_data = delegated_account.try_borrow_mut_data()?;
    if delegated_data[start..end] == *chunk_data {
        return Ok(());
    }
    delegated_data[start..end].copy_from_slice(chunk_data);
    drop(delegated_data);

    session_state.settlement_accumulator = accumulate_data_chunk_checksum(
        session_state.settlement_accumulator,
        delegated_account.key(),
        account_data_offset,
        chunk_data,
    );
    store_session(session, &session_state)?;

    Ok(())
}

pub fn process_settle_account_owner(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    SettleAccountOwner {
        er_slot,
        checksum,
        owner,
    }: SettleAccountOwner,
) -> ProgramResult {
    if accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let validator = &accounts[0];
    let session = &accounts[1];
    let delegated_account = &accounts[2];
    let delegation_record = &accounts[3];

    let mut session_state = load_session(program_id, session)?;
    require_validator(validator, &session_state)?;
    require_active_settlement(&session_state, er_slot, checksum)?;

    let mut delegation_state = load_delegation_record(
        program_id,
        &session_state,
        delegated_account,
        delegation_record,
    )?;
    if delegation_state.owner_program == owner {
        return Ok(());
    }

    delegation_state.owner_program = owner;
    let mut delegation_data = delegation_record.try_borrow_mut_data()?;
    BorshSerialize::serialize(
        &delegation_state,
        &mut &mut delegation_data[..DelegationRecord::LEN],
    )
    .unwrap();
    drop(delegation_data);

    session_state.settlement_accumulator = accumulate_owner_checksum(
        session_state.settlement_accumulator,
        delegated_account.key(),
        &owner,
    );
    store_session(session, &session_state)?;

    Ok(())
}

pub fn process_settle_account_lamports(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    SettleAccountLamports {
        er_slot,
        checksum,
        account_count,
        lamports,
    }: SettleAccountLamports,
) -> ProgramResult {
    let account_count = account_count as usize;
    if account_count == 0 || account_count > MAX_SETTLEMENT_LAMPORT_ACCOUNTS {
        return Err(ProgramError::InvalidInstructionData);
    }
    if accounts.len() < 2 + account_count * 2 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let validator = &accounts[0];
    let session = &accounts[1];

    let mut session_state = load_session(program_id, session)?;
    require_validator(validator, &session_state)?;
    require_active_settlement(&session_state, er_slot, checksum)?;

    let settlement_accounts = &accounts[2..2 + account_count * 2];
    for index in 0..account_count {
        let account = &settlement_accounts[index * 2];
        let record = &settlement_accounts[index * 2 + 1];
        load_delegation_record(program_id, &session_state, account, record)?;

        for other_index in 0..index {
            let other = &settlement_accounts[other_index * 2];
            if account.key() == other.key() {
                return Err(ProgramError::InvalidInstructionData);
            }
        }
    }

    let rent = Rent::get()?;
    let mut current_total = 0u128;
    let mut target_total = 0u128;
    let mut already_settled = true;
    for index in 0..account_count {
        let account = &settlement_accounts[index * 2];
        let target_lamports = lamports[index];
        current_total = current_total
            .checked_add(account.lamports() as u128)
            .ok_or(PortalError::ArithmeticOverflow)?;
        target_total = target_total
            .checked_add(target_lamports as u128)
            .ok_or(PortalError::ArithmeticOverflow)?;
        if account.data_len() > 0 && target_lamports < rent.minimum_balance(account.data_len()) {
            return Err(PortalError::SettlementLamportsBelowRentExempt.into());
        }
        already_settled &= account.lamports() == target_lamports;
    }

    if already_settled {
        return Ok(());
    }
    if current_total != target_total {
        return Err(PortalError::SettlementLamportsNotConserved.into());
    }

    for index in 0..account_count {
        let account = &settlement_accounts[index * 2];
        let target_lamports = lamports[index];
        *account.try_borrow_mut_lamports()? = target_lamports;
        session_state.settlement_accumulator = accumulate_lamports_checksum(
            session_state.settlement_accumulator,
            account.key(),
            target_lamports,
        );
    }
    store_session(session, &session_state)?;

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
    if checksum != session_state.settlement_checksum
        || checksum != session_state.settlement_accumulator
    {
        return Err(PortalError::SettlementChecksumMismatch.into());
    }

    session_state.last_settled_l1_slot = Clock::get()?.slot;
    session_state.last_settled_er_slot = er_slot;
    session_state.settlement_status = SettlementStatus::Idle;
    session_state.settlement_er_slot = 0;
    session_state.settlement_checksum = [0; 32];
    session_state.settlement_accumulator = [0; 32];
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
    session_state.settlement_accumulator = [0; 32];
    session_state.settlement_started_l1_slot = 0;
    store_session(session, &session_state)?;

    Ok(())
}
