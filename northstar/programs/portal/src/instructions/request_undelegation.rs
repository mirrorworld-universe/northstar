use {
    super::initialize_pda_account,
    crate::{
        account_size,
        error::PortalError,
        pda::{find_delegation_record_pda, find_session_pda, find_undelegation_request_pda},
        state::{DelegationRecord, Session, UndelegationRequest},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        cpi::{Seed, Signer},
        error::ProgramError,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
};

#[p_instruction(
    id = 28,
    accounts = [
        payer(signer, mut),
        authority(signer),
        delegated_account(signer),
        owner_program,
        delegation_record(state = DelegationRecord),
        session(state = Session),
        undelegation_request(mut, state = UndelegationRequest),
        system_program
    ]
)]
pub fn process_request_undelegation(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
) -> ProgramResult {
    let [payer, authority, delegated_account, owner_program, delegation_record, session, request, _system_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !payer.is_signer() || !authority.is_signer() || !delegated_account.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }
    let (expected_session, _) = find_session_pda(program_id);
    if session.address() != &expected_session || !session.owned_by(program_id) {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let session_state = Session::try_from_slice(&session.try_borrow()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !session_state.is_valid() {
        return Err(PortalError::SessionStateInvalid.into());
    }
    if session_state.is_expired(Clock::get()?.slot) {
        return Err(PortalError::SessionExpired.into());
    }
    if !delegated_account.owned_by(program_id) {
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    let delegated_key = *delegated_account.address();
    let (expected_record, _) = find_delegation_record_pda(program_id, &delegated_key);
    if delegation_record.address() != &expected_record || !delegation_record.owned_by(program_id) {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let record = DelegationRecord::try_from_slice(&delegation_record.try_borrow()?)
        .map_err(|_| PortalError::DelegationRecordDeserializeFailed)?;
    if !record.is_valid() || record.owner_program != *owner_program.address() {
        return Err(PortalError::DelegationRecordStateInvalid.into());
    }

    let (expected_request, bump) = find_undelegation_request_pda(program_id, &delegated_key);
    if request.address() != &expected_request {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if request.lamports() != 0 || !request.is_data_empty() {
        return Err(PortalError::UndelegationRequestAlreadyInitialized.into());
    }

    let request_state = UndelegationRequest {
        discriminator: UndelegationRequest::DISCRIMINATOR,
        session: *session.address(),
        delegated_account: delegated_key,
        owner_program: *owner_program.address(),
        authority: *authority.address(),
        requested_at_l1_slot: Clock::get()?.slot,
        approved: false,
        bump,
    };
    let request_size = account_size(&request_state);
    let request_rent = Rent::get()?.try_minimum_balance(request_size)?;
    let bump_seed = [bump];
    initialize_pda_account(
        payer,
        request,
        request_rent,
        request_size as u64,
        program_id,
        Signer::from(&[
            Seed::from(UndelegationRequest::SEED_PREFIX),
            Seed::from(delegated_account.address().as_ref()),
            Seed::from(&bump_seed),
        ]),
    )?;
    BorshSerialize::serialize(&request_state, &mut &mut request.try_borrow_mut()?[..])
        .map_err(|_| PortalError::InvalidAccountData)?;

    pinocchio_log::log!("Undelegation request created");
    Ok(())
}

#[p_instruction(
    id = 29,
    accounts = [
        validator(signer),
        session(state = Session),
        undelegation_request(mut, state = UndelegationRequest)
    ]
)]
pub fn process_approve_undelegation(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
) -> ProgramResult {
    let [validator, session, request, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !validator.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }
    let (expected_session, _) = find_session_pda(program_id);
    if session.address() != &expected_session || !session.owned_by(program_id) {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let session_state = Session::try_from_slice(&session.try_borrow()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !session_state.is_valid() || session_state.validator != *validator.address() {
        return Err(PortalError::Unauthorized.into());
    }
    if session_state.settlement_status != crate::SettlementStatus::Idle {
        return Err(PortalError::SettlementInProgress.into());
    }

    let mut request_state = UndelegationRequest::try_from_slice(&request.try_borrow()?)
        .map_err(|_| PortalError::UndelegationRequestDeserializeFailed)?;
    let (expected_request, bump) =
        find_undelegation_request_pda(program_id, &request_state.delegated_account);
    if !request.owned_by(program_id)
        || request.address() != &expected_request
        || !request_state.is_valid()
        || request_state.bump != bump
        || request_state.session != *session.address()
    {
        return Err(PortalError::UndelegationRequestStateInvalid.into());
    }
    request_state.approved = true;
    BorshSerialize::serialize(&request_state, &mut &mut request.try_borrow_mut()?[..])
        .map_err(|_| PortalError::InvalidAccountData)?;
    pinocchio_log::log!("Undelegation request approved");
    Ok(())
}
