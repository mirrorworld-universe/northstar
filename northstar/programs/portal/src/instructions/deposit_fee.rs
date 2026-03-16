use {
    crate::{error::PortalError, state::FeeVault},
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
    },
    pinocchio_system::instructions::Transfer,
};

pub fn process_deposit_fee(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    lamports: u64,
) -> ProgramResult {
    if accounts.len() < 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let depositor = &accounts[0];
    let fee_vault = &accounts[1];
    let _system_program = &accounts[2];

    if !depositor.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    // Validate fee_vault is owned by portal program
    if fee_vault.owner() != program_id {
        return Err(PortalError::InvalidAccountData.into());
    }

    if lamports == 0 {
        pinocchio_log::log!("WARN: Deposited 0 lamports");
        return Ok(());
    }

    let vault_state = FeeVault::try_from_slice(&fee_vault.try_borrow_data()?)
        .map_err(|_| PortalError::InvalidAccountData)?;

    if !vault_state.is_valid() {
        return Err(PortalError::InvalidAccountData.into());
    }

    // NOTE: No authority check — anyone can deposit

    Transfer {
        from: depositor,
        to: fee_vault,
        lamports,
    }
    .invoke()?;

    let mut vault_state = vault_state;
    vault_state.balance = vault_state
        .balance
        .checked_add(lamports)
        .ok_or(PortalError::ArithmeticOverflow)?;

    let mut fee_vault_data = fee_vault.try_borrow_mut_data()?;
    BorshSerialize::serialize(&vault_state, &mut &mut fee_vault_data[..FeeVault::LEN]).unwrap();

    pinocchio_log::log!("DepositFee");

    Ok(())
}
