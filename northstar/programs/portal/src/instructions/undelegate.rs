use {
    crate::{
        error::PortalError,
        pda::{find_delegation_record_pda, find_session_pda},
        state::{DelegationRecord, Session, SettlementStatus},
    },
    borsh::BorshDeserialize,
    pinocchio::{
        account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
    },
};

/// Undelegate an account, returning ownership to `owner_program`.
///
/// Solana's runtime allows owner reassign only when existing data bytes are all zero.
/// Plain undelegation therefore rejects non-empty delegated account data instead of
/// silently clearing settled bytes.
///
/// `UndelegateHandoff` is an explicit primitive for owner-program CPI wrappers:
/// the owner program must copy the Portal-owned data before CPI, invoke this
/// instruction to zero data and assign ownership back, then restore the copied
/// bytes after CPI returns and it owns the account again.
///
/// Accounts:
/// 0. `[signer, writable]` authority (receives the delegation_record's lamport refund)
/// 1. `[writable]` delegated_account
/// 2. `[]` owner_program (must equal `delegation_record.owner_program`)
/// 3. `[writable]` delegation_record PDA (closed)
/// 4. `[]` system_program
/// 5. `[]` session
pub fn process_undelegate(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    pinocchio_log::log!("Instruction: Undelegate");
    process_undelegate_inner(program_id, accounts, false)
}

pub fn process_undelegate_handoff(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    pinocchio_log::log!("Instruction: UndelegateHandoff");
    process_undelegate_inner(program_id, accounts, true)
}

fn process_undelegate_inner(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    allow_non_empty_handoff: bool,
) -> ProgramResult {
    if accounts.len() < 6 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority = &accounts[0];
    let delegated_account = &accounts[1];
    let owner_program = &accounts[2];
    let delegation_record = &accounts[3];
    let _system_program = &accounts[4];
    let session = &accounts[5];

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

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

    let delegated_key = *delegated_account.key();
    let (expected_delegation_key, _) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.key() != &expected_delegation_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let delegation_state = DelegationRecord::try_from_slice(&delegation_record.try_borrow_data()?)
        .map_err(|_| PortalError::DelegationRecordDeserializeFailed)?;

    if !delegation_state.is_valid() {
        return Err(PortalError::DelegationRecordStateInvalid.into());
    }

    if delegation_state.owner_program != *owner_program.key() {
        return Err(PortalError::Unauthorized.into());
    }

    if delegated_account.owner() != program_id {
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    let mut delegated_data = delegated_account.try_borrow_mut_data()?;
    let has_non_empty_data = delegated_data.iter().any(|byte| *byte != 0);
    if has_non_empty_data && !allow_non_empty_handoff {
        return Err(PortalError::DelegatedAccountDataNotEmpty.into());
    }
    if has_non_empty_data {
        delegated_data.fill(0);
    }
    drop(delegated_data);

    unsafe { delegated_account.assign(owner_program.key()) };

    let delegation_record_lamports = delegation_record.lamports();

    if delegation_record_lamports > 0 {
        let mut authority_lamports = authority.try_borrow_mut_lamports()?;
        *authority_lamports = authority_lamports
            .checked_add(delegation_record_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
        *delegation_record.try_borrow_mut_lamports()? = 0;
    }

    delegation_record.try_borrow_mut_data()?.fill(0);

    pinocchio_log::log!("Undelegate success");

    Ok(())
}
