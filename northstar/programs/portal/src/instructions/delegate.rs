use {
    crate::{error::PortalError, pda::find_delegation_record_pda, state::DelegationRecord},
    borsh::BorshSerialize,
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

pub fn process_delegate(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    grid_id: u64,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: Delegate, grid_id={}", grid_id);

    if accounts.len() < 5 {
        pinocchio_log::log!("ERROR: Delegate failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let payer = &accounts[0];
    let delegated_account = &accounts[1];
    let owner_program = &accounts[2]; // the original program of delegated account
    let delegation_record = &accounts[3];
    let _system_program = &accounts[4];

    if !payer.is_signer() {
        pinocchio_log::log!("ERROR: Delegate failed: payer is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    if delegated_account.owner() != program_id {
        pinocchio_log::log!("ERROR: Delegate failed: delegated account owner mismatch");
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    let delegated_key = *delegated_account.key();
    let (expected_delegation_key, bump) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.key() != &expected_delegation_key {
        pinocchio_log::log!("ERROR: Delegate failed: delegation record PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    if delegation_record.lamports() > 0 {
        pinocchio_log::log!("ERROR: Delegate failed: delegation record already initialized");
        return Err(PortalError::DelegationRecordAlreadyInitialized.into());
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
        grid_id,
        bump,
    };
    let mut delegation_data = delegation_record.try_borrow_mut_data()?;
    BorshSerialize::serialize(
        &delegation_state,
        &mut &mut delegation_data[..DelegationRecord::LEN],
    )
    .unwrap();

    pinocchio_log::log!("Delegate success");

    Ok(())
}
