use {
    crate::{
        error::PortalError,
        pda::{find_delegation_record_pda, find_session_pda},
        state::{DelegationRecord, Session, SettlementStatus},
    },
    borsh::BorshDeserialize,
    pinocchio::{
        error::ProgramError, AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
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
#[p_instruction(
    id = 4,
    accounts = [
        authority(signer, mut),
        delegated_account(mut),
        owner_program,
        delegation_record(mut, state = DelegationRecord),
        system_program,
        session(state = Session)
    ]
)]
pub fn process_undelegate(program_id: &Pubkey, accounts: &mut [AccountInfo]) -> ProgramResult {
    pinocchio_log::log!("Instruction: Undelegate");
    process_undelegate_inner(program_id, accounts, false)
}

#[p_instruction(
    id = 10,
    accounts = [
        authority(signer, mut),
        delegated_account(mut),
        owner_program,
        delegation_record(mut, state = DelegationRecord),
        system_program,
        session(state = Session)
    ]
)]
pub fn process_undelegate_handoff(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
) -> ProgramResult {
    pinocchio_log::log!("Instruction: UndelegateHandoff");
    process_undelegate_inner(program_id, accounts, true)
}

fn process_undelegate_inner(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    allow_non_empty_handoff: bool,
) -> ProgramResult {
    let [authority, delegated_account, owner_program, delegation_record, _system_program, session, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

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
    if session_state.settlement_status == SettlementStatus::InProgress {
        return Err(PortalError::SettlementInProgress.into());
    }

    let delegated_key = *delegated_account.address();
    let (expected_delegation_key, _) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.address() != &expected_delegation_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let delegation_state = DelegationRecord::try_from_slice(&delegation_record.try_borrow()?)
        .map_err(|_| PortalError::DelegationRecordDeserializeFailed)?;

    if !delegation_state.is_valid() {
        return Err(PortalError::DelegationRecordStateInvalid.into());
    }

    if delegation_state.owner_program != *owner_program.address() {
        return Err(PortalError::Unauthorized.into());
    }

    if !delegated_account.owned_by(program_id) {
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    let mut delegated_data = delegated_account.try_borrow_mut()?;
    let has_non_empty_data = delegated_data.iter().any(|byte| *byte != 0);
    if has_non_empty_data && !allow_non_empty_handoff {
        return Err(PortalError::DelegatedAccountDataNotEmpty.into());
    }
    if has_non_empty_data {
        delegated_data.fill(0);
    }
    drop(delegated_data);

    unsafe { delegated_account.assign(owner_program.address()) };

    let delegation_record_lamports = delegation_record.lamports();

    if delegation_record_lamports > 0 {
        let authority_lamports = authority
            .lamports()
            .checked_add(delegation_record_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
        authority.set_lamports(authority_lamports);
        delegation_record.set_lamports(0);
    }

    delegation_record.try_borrow_mut()?.fill(0);

    pinocchio_log::log!("Undelegate success");

    Ok(())
}
