use {
    crate::{
        error::PortalError,
        pda::find_delegation_record_pda,
        state::{DelegationRecord, DELEGATION_RECORD_DISCRIMINATOR},
    },
    borsh::BorshSerialize,
    pinocchio::{
        account_info::AccountInfo,
        instruction::{Seed, Signer},
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{rent::Rent, Sysvar},
        ProgramResult,
    },
    pinocchio_system::instructions::CreateAccount,
};

pub fn process_delegate(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    grid_id: u64,
) -> ProgramResult {
    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let payer = &accounts[0];
    let delegated_account = &accounts[1];
    let owner_program = &accounts[2];
    let delegation_record = &accounts[3];
    let _system_program = &accounts[4];

    if !payer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    if delegated_account.owner() != program_id {
        return Err(PortalError::InvalidAccountData.into());
    }

    let delegated_key = *delegated_account.key();
    let (expected_delegation_key, bump) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.key() != &expected_delegation_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    if delegation_record.lamports() > 0 {
        return Err(PortalError::InvalidAccountData.into());
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
        discriminator: DELEGATION_RECORD_DISCRIMINATOR,
        owner_program: *owner_program.key(),
        grid_id,
        bump,
    };
    let mut delegation_data = delegation_record.try_borrow_mut_data()?;
    BorshSerialize::serialize(
        &delegation_state,
        &mut &mut delegation_data[..DelegationRecord::LEN],
    )
    .unwrap();

    pinocchio_log::log!("Account delegated");

    Ok(())
}
