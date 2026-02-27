use {
    crate::{error::PortalError, pda::find_fee_vault_pda, state::FeeVault},
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
    },
    pinocchio_system::instructions::Transfer,
};

pub fn process_deposit_fee(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if accounts.len() < 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let owner = &accounts[0];
    let fee_vault = &accounts[1];
    let _system_program = &accounts[2];

    if !owner.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    if data.len() < 9 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let amount = u64::from_le_bytes(data[1..9].try_into().unwrap());

    let owner_key = *owner.key();
    let (expected_fee_vault_key, _) = find_fee_vault_pda(program_id, &owner_key);

    if fee_vault.key() != &expected_fee_vault_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let vault_data = fee_vault.try_borrow_data()?;
    let vault_state =
        FeeVault::try_from_slice(&vault_data).map_err(|_| PortalError::InvalidAccountData)?;
    drop(vault_data);

    if !vault_state.is_valid() {
        return Err(PortalError::InvalidAccountData.into());
    }

    if vault_state.authority != owner_key {
        return Err(PortalError::Unauthorized.into());
    }

    Transfer {
        from: owner,
        to: fee_vault,
        lamports: amount,
    }
    .invoke()?;

    let mut vault_state = vault_state;
    vault_state.balance = vault_state
        .balance
        .checked_add(amount)
        .ok_or(PortalError::ArithmeticOverflow)?;

    let mut fee_vault_data = fee_vault.try_borrow_mut_data()?;
    BorshSerialize::serialize(&vault_state, &mut &mut fee_vault_data[..FeeVault::LEN]).unwrap();

    pinocchio_log::log!("DepositFee");

    Ok(())
}
