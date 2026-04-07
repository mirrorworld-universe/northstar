use {
    crate::{error::PortalError, pda::find_delegation_record_pda, state::DelegationRecord},
    borsh::BorshDeserialize,
    pinocchio::{
        account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
    },
    pinocchio_system::instructions::Assign,
};

pub fn process_undelegate(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    pinocchio_log::log!("Instruction: Undelegate");

    if accounts.len() < 5 {
        pinocchio_log::log!("ERROR: Undelegate failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority = &accounts[0];
    let delegated_account = &accounts[1];
    let owner_program = &accounts[2];
    let delegation_record = &accounts[3];
    let _system_program = &accounts[4];

    if !authority.is_signer() {
        pinocchio_log::log!("ERROR: Undelegate failed: authority is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    let delegated_key = *delegated_account.key();
    let (expected_delegation_key, _) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.key() != &expected_delegation_key {
        pinocchio_log::log!("ERROR: Undelegate failed: delegation record PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let delegation_state = DelegationRecord::try_from_slice(&delegation_record.try_borrow_data()?)
        .map_err(|_| {
            pinocchio_log::log!("ERROR: Undelegate failed: delegation record deserialize failed");
            PortalError::DelegationRecordDeserializeFailed
        })?;

    if !delegation_state.is_valid() {
        pinocchio_log::log!("ERROR: Undelegate failed: delegation record state invalid");
        return Err(PortalError::DelegationRecordStateInvalid.into());
    }

    if delegation_state.owner_program != *owner_program.key() {
        pinocchio_log::log!("ERROR: Undelegate failed: owner program mismatch");
        return Err(PortalError::Unauthorized.into());
    }

    if delegated_account.owner() != program_id {
        pinocchio_log::log!("ERROR: Undelegate failed: delegated account owner mismatch");
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    unsafe { delegated_account.assign(owner_program.key()) };


    let delegation_record_lamports = delegation_record.lamports();

    if delegation_record_lamports > 0 {
        let mut authority_lamports = authority.try_borrow_mut_lamports()?;
        *authority_lamports = authority_lamports
            .checked_add(delegation_record_lamports)
            .ok_or_else(|| {
                PortalError::ArithmeticOverflow
            })?;
        *delegation_record.try_borrow_mut_lamports()? = 0;
    }

    delegation_record.try_borrow_mut_data()?.fill(0);

    pinocchio_log::log!("Undelegate success");

    Ok(())
}
