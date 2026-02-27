use {
    crate::{error::PortalError, pda::find_delegation_record_pda, state::DelegationRecord},
    borsh::BorshDeserialize,
    pinocchio::{
        account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
    },
    pinocchio_system::instructions::Assign,
};

pub fn process_undelegate(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority = &accounts[0];
    let delegated_account = &accounts[1];
    let owner_program = &accounts[2];
    let delegation_record = &accounts[3];
    let _system_program = &accounts[4];

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let delegated_key = *delegated_account.key();
    let (expected_delegation_key, _) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.key() != &expected_delegation_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let delegation_data = delegation_record.try_borrow_data()?;
    let delegation_state = DelegationRecord::try_from_slice(&delegation_data)
        .map_err(|_| PortalError::InvalidAccountData)?;

    if !delegation_state.is_valid() {
        return Err(PortalError::InvalidAccountData.into());
    }

    if delegation_state.owner_program != *owner_program.key() {
        return Err(PortalError::Unauthorized.into());
    }

    if delegated_account.owner() != program_id {
        return Err(PortalError::InvalidAccountData.into());
    }

    Assign {
        account: delegated_account,
        owner: owner_program.key(),
    }
    .invoke()?;

    let delegation_record_lamports = delegation_record.lamports();

    if delegation_record_lamports > 0 {
        let mut authority_lamports = authority.try_borrow_mut_lamports()?;
        *authority_lamports = authority_lamports
            .checked_add(delegation_record_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
        let mut delegation_record_lamports_mut = delegation_record.try_borrow_mut_lamports()?;
        *delegation_record_lamports_mut = 0;
    }

    let mut delegation_record_data = delegation_record.try_borrow_mut_data()?;
    delegation_record_data.fill(0);

    pinocchio_log::log!("Account undelegated");

    Ok(())
}
