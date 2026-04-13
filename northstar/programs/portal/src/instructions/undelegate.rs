use {
    crate::{error::PortalError, pda::find_delegation_record_pda, state::DelegationRecord},
    borsh::BorshDeserialize,
    pinocchio::{AccountView, Address, ProgramResult, error::ProgramError},
};

pub fn process_undelegate(program_id: &Address, accounts: &mut [AccountView]) -> ProgramResult {
    pinocchio_log::log!("Instruction: Undelegate");

    let [
        authority,
        delegated_account,
        owner_program,
        delegation_record,
        _system_program,
        ..,
    ] = accounts
    else {
        pinocchio_log::log!("ERROR: Undelegate failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !authority.is_signer() {
        pinocchio_log::log!("ERROR: Undelegate failed: authority is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    let delegated_key = *delegated_account.address();
    let (expected_delegation_key, _) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.address() != &expected_delegation_key {
        pinocchio_log::log!("ERROR: Undelegate failed: delegation record PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let delegation_data = delegation_record.try_borrow()?;
    let delegation_state = DelegationRecord::try_from_slice(&delegation_data).map_err(|_| {
        pinocchio_log::log!("ERROR: Undelegate failed: delegation record deserialize failed");
        PortalError::DelegationRecordDeserializeFailed
    })?;
    drop(delegation_data);

    if !delegation_state.is_valid() {
        pinocchio_log::log!("ERROR: Undelegate failed: delegation record state invalid");
        return Err(PortalError::DelegationRecordStateInvalid.into());
    }

    if delegation_state.owner_program != owner_program.address().to_bytes() {
        pinocchio_log::log!("ERROR: Undelegate failed: owner program mismatch");
        return Err(PortalError::Unauthorized.into());
    }

    if !delegated_account.owned_by(program_id) {
        pinocchio_log::log!("ERROR: Undelegate failed: delegated account owner mismatch");
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    unsafe { delegated_account.assign(owner_program.address()) };

    let delegation_record_lamports = delegation_record.lamports();
    if delegation_record_lamports > 0 {
        let new_authority_lamports = authority
            .lamports()
            .checked_add(delegation_record_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
        authority.set_lamports(new_authority_lamports);
        delegation_record.set_lamports(0);
    }

    delegation_record.try_borrow_mut()?.fill(0);

    pinocchio_log::log!("Undelegate success");

    Ok(())
}
