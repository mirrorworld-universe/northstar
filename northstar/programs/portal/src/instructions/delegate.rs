use {
    crate::{
        error::PortalError,
        pda::{find_delegation_record_pda, find_session_pda},
        state::{DelegationRecord, Session, SettlementStatus},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        ProgramResult,
        account_info::AccountInfo,
        instruction::{Seed, Signer},
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{Sysvar, rent::Rent},
    },
    pinocchio_system::instructions::CreateAccount,
};

/// Delegate one or more accounts into a NorthStar Ephemeral Rollup session.
///
/// Caller must pre-stage each `delegated_account` (Portal-owned, post-
/// `system::Assign`) and matching `buffer` (`owner_program`-owned, data_len
/// matching `delegated_account`, holding bytes to install into it). Portal
/// copies `buffer → delegated_account` after creating each `DelegationRecord`.
/// For the keypair-wallet flow both have 0-length data and the copy is a no-op.
///
/// Accounts:
/// 0. `[signer, writable]` payer
/// 1. `[]` system_program
/// 2. `[]` session
///
/// Then one 4-account group per delegation:
/// - `[signer, writable]` delegated_account
/// - `[]` owner_program (stored in `DelegationRecord.owner_program`)
/// - `[writable]` delegation_record PDA (`["delegation", delegated_account]` under Portal)
/// - `[]` buffer
pub fn process_delegate(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    grid_id: u64,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: Delegate, grid_id={}", grid_id);

    const PREFIX_ACCOUNTS: usize = 3;
    const ACCOUNTS_PER_DELEGATION: usize = 4;

    if accounts.len() < PREFIX_ACCOUNTS + ACCOUNTS_PER_DELEGATION {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let payer = &accounts[0];
    let system_program = &accounts[1];
    let session = &accounts[2];
    let delegation_accounts = &accounts[PREFIX_ACCOUNTS..];

    if !delegation_accounts
        .len()
        .is_multiple_of(ACCOUNTS_PER_DELEGATION)
    {
        return Err(ProgramError::InvalidInstructionData);
    }

    let session_state = validate_session(program_id, session, grid_id)?;

    for group in delegation_accounts.chunks_exact(ACCOUNTS_PER_DELEGATION) {
        process_delegate_account(
            program_id,
            payer,
            &group[0],
            &group[1],
            &group[2],
            system_program,
            &group[3],
            &session_state,
        )?;
    }

    pinocchio_log::log!("Delegate success");

    Ok(())
}

fn validate_session(
    program_id: &Pubkey,
    session: &AccountInfo,
    grid_id: u64,
) -> Result<Session, ProgramError> {
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
    if session_state.settlement_status == SettlementStatus::InProgress {
        return Err(PortalError::SettlementInProgress.into());
    }
    if session_state.grid_id != grid_id {
        return Err(PortalError::Unauthorized.into());
    }
    Ok(session_state)
}

fn process_delegate_account(
    program_id: &Pubkey,
    payer: &AccountInfo,
    delegated_account: &AccountInfo,
    owner_program: &AccountInfo,
    delegation_record: &AccountInfo,
    _system_program: &AccountInfo,
    buffer: &AccountInfo,
    _session_state: &Session,
) -> ProgramResult {
    if !payer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    if !delegated_account.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    if delegated_account.owner() != program_id {
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    let delegated_key = *delegated_account.key();
    let (expected_delegation_key, bump) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.key() != &expected_delegation_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    if delegation_record.lamports() > 0 {
        return Err(PortalError::DelegationRecordAlreadyInitialized.into());
    }

    if buffer.owner() != owner_program.key() {
        return Err(PortalError::DelegateBufferOwnerMismatch.into());
    }
    if buffer.data_len() != delegated_account.data_len() {
        return Err(PortalError::DelegateBufferSizeMismatch.into());
    }

    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(DelegationRecord::LEN);

    let bump_bytes = [bump];
    let seeds = &[
        Seed::from(DelegationRecord::SEED_PREFIX),
        Seed::from(delegated_key.as_ref()),
        Seed::from(bump_bytes.as_ref()),
    ];
    let signer = Signer::from(seeds);

    CreateAccount {
        from: payer,
        to: delegation_record,
        lamports,
        space: DelegationRecord::LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[signer])?;

    let delegation_state = DelegationRecord {
        discriminator: DelegationRecord::DISCRIMINATOR,
        owner_program: *owner_program.key(),
        grid_id: _session_state.grid_id,
        bump,
    };
    let mut delegation_data = delegation_record.try_borrow_mut_data()?;
    BorshSerialize::serialize(
        &delegation_state,
        &mut &mut delegation_data[..DelegationRecord::LEN],
    )
    .unwrap();
    drop(delegation_data);

    let buffer_data = buffer.try_borrow_data()?;
    let mut delegated_data = delegated_account.try_borrow_mut_data()?;
    delegated_data.copy_from_slice(&buffer_data);

    Ok(())
}
