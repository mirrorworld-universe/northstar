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

/// Delegate an account into a NorthStar Ephemeral Rollup session.
///
/// Caller must pre-stage two accounts: `delegated_account` (Portal-owned, post-
/// `system::Assign`) and `buffer` (`owner_program`-owned, data_len matching
/// `delegated_account`, holding bytes to install into it). Portal copies
/// `buffer → delegated_account` after creating the `DelegationRecord`. For the
/// keypair-wallet flow both have 0-length data and the copy is a no-op.
///
/// Accounts:
/// 0. `[signer, writable]` payer
/// 1. `[signer, writable]` delegated_account
/// 2. `[]` owner_program (stored in `DelegationRecord.owner_program`)
/// 3. `[writable]` delegation_record PDA (`["delegation", delegated_account]` under Portal)
/// 4. `[]` system_program
/// 5. `[]` buffer
pub fn process_delegate(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    grid_id: u64,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: Delegate, grid_id={}", grid_id);

    if accounts.len() < 6 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let payer = &accounts[0];
    let delegated_account = &accounts[1];
    let owner_program = &accounts[2];
    let delegation_record = &accounts[3];
    let _system_program = &accounts[4];
    let buffer = &accounts[5];

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
        grid_id,
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

    pinocchio_log::log!("Delegate success");

    Ok(())
}
