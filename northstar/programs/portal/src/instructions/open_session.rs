use {
    crate::{
        error::PortalError,
        pda::{find_fee_vault_pda, find_session_pda},
        state::{FeeVault, Session, FEE_VAULT_DISCRIMINATOR, SESSION_DISCRIMINATOR},
        OpenSession,
    },
    borsh::BorshSerialize,
    pinocchio::{
        account_info::AccountInfo,
        instruction::{Seed, Signer},
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        ProgramResult,
    },
    pinocchio_system::instructions::CreateAccount,
};

pub fn process_open_session(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    OpenSession {
        grid_id,
        ttl_slots,
        fee_cap,
        owner: owner_key,
    }: OpenSession,
) -> ProgramResult {
    if accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let owner = &accounts[0];
    let session = &accounts[1];
    let fee_vault = &accounts[2];
    let _system_program = &accounts[3];

    if !owner.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let (expected_session_key, session_bump) = find_session_pda(program_id, &owner_key, grid_id);
    let (expected_fee_vault_key, fee_vault_bump) = find_fee_vault_pda(program_id, &owner_key);

    if session.key() != &expected_session_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if fee_vault.key() != &expected_fee_vault_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let clock = Clock::get()?;
    let current_slot = clock.slot;

    let rent = Rent::get()?;
    let session_lamports = rent.minimum_balance(Session::LEN);
    let fee_vault_lamports = rent.minimum_balance(FeeVault::LEN);

    // Create Session PDA
    let grid_id_bytes = grid_id.to_le_bytes();
    let session_bump_bytes = [session_bump];
    let session_seeds = &[
        Seed::from(Session::SEED_PREFIX),
        Seed::from(owner_key.as_ref()),
        Seed::from(grid_id_bytes.as_ref()),
        Seed::from(session_bump_bytes.as_ref()),
    ];
    let session_signer = Signer::from(session_seeds);

    CreateAccount {
        from: owner,
        to: session,
        lamports: session_lamports,
        space: Session::LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[session_signer])?;

    // Create FeeVault PDA
    let fee_vault_bump_bytes = [fee_vault_bump];
    let fee_vault_seeds = &[
        Seed::from(FeeVault::SEED_PREFIX),
        Seed::from(owner_key.as_ref()),
        Seed::from(fee_vault_bump_bytes.as_ref()),
    ];
    let fee_vault_signer = Signer::from(fee_vault_seeds);

    CreateAccount {
        from: owner,
        to: fee_vault,
        lamports: fee_vault_lamports,
        space: FeeVault::LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[fee_vault_signer])?;

    // Write Session state
    let session_state = Session {
        discriminator: SESSION_DISCRIMINATOR,
        owner: owner_key,
        grid_id,
        ttl_slots,
        fee_cap,
        created_at: current_slot,
        nonce: 0,
        bump: session_bump,
    };
    let mut session_data = session.try_borrow_mut_data()?;
    BorshSerialize::serialize(&session_state, &mut &mut session_data[..Session::LEN]).unwrap();

    // Write FeeVault state
    let fee_vault_state = FeeVault {
        discriminator: FEE_VAULT_DISCRIMINATOR,
        authority: owner_key,
        balance: 0,
        bump: fee_vault_bump,
    };
    let mut fee_vault_data = fee_vault.try_borrow_mut_data()?;
    BorshSerialize::serialize(&fee_vault_state, &mut &mut fee_vault_data[..FeeVault::LEN]).unwrap();

    pinocchio_log::log!("Session opened");

    Ok(())
}
