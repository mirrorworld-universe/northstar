use {
    crate::{FeeVault, OpenSession, PortalError, Session, find_fee_vault_pda, find_session_pda},
    borsh::BorshSerialize,
    pinocchio::{
        AccountView, Address, ProgramResult,
        cpi::{Seed, Signer},
        error::ProgramError,
        sysvars::{Sysvar, clock::Clock, rent::Rent},
    },
    pinocchio_system::instructions::CreateAccount,
};

pub fn process_open_session(
    program_id: &Address,
    accounts: &mut [AccountView],
    OpenSession {
        grid_id,
        ttl_slots,
        fee_cap,
    }: OpenSession,
) -> ProgramResult {
    pinocchio_log::log!(
        "Instruction: OpenSession, grid_id={}, ttl_slots={}, fee_cap={}",
        grid_id,
        ttl_slots,
        fee_cap
    );

    let [owner, session, fee_vault, _system_program, ..] = accounts else {
        pinocchio_log::log!("ERROR: OpenSession failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !owner.is_signer() {
        pinocchio_log::log!("ERROR: OpenSession failed: owner is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    let owner_key = *owner.address();
    let (expected_session_key, session_bump) = find_session_pda(program_id, &owner_key, grid_id);
    let (expected_fee_vault_key, fee_vault_bump) = find_fee_vault_pda(program_id, &owner_key);

    if session.address() != &expected_session_key {
        pinocchio_log::log!("ERROR: OpenSession failed: session PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if fee_vault.address() != &expected_fee_vault_key {
        pinocchio_log::log!("ERROR: OpenSession failed: fee vault PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let current_slot = Clock::get()?.slot;

    let rent = Rent::get()?;
    let session_lamports = rent.try_minimum_balance(Session::LEN)?;
    let fee_vault_lamports = rent.try_minimum_balance(FeeVault::LEN)?;

    let grid_id_bytes = grid_id.to_le_bytes();
    let session_bump_bytes = [session_bump];
    let session_seeds = [
        Seed::from(Session::SEED_PREFIX),
        Seed::from(owner_key.as_ref()),
        Seed::from(grid_id_bytes.as_ref()),
        Seed::from(session_bump_bytes.as_ref()),
    ];
    let session_signer = Signer::from(&session_seeds);

    CreateAccount {
        from: owner,
        to: session,
        lamports: session_lamports,
        space: Session::LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[session_signer])?;

    let fee_vault_bump_bytes = [fee_vault_bump];
    let fee_vault_seeds = [
        Seed::from(FeeVault::SEED_PREFIX),
        Seed::from(owner_key.as_ref()),
        Seed::from(fee_vault_bump_bytes.as_ref()),
    ];
    let fee_vault_signer = Signer::from(&fee_vault_seeds);

    CreateAccount {
        from: owner,
        to: fee_vault,
        lamports: fee_vault_lamports,
        space: FeeVault::LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[fee_vault_signer])?;

    let session_state = Session {
        discriminator: Session::DISCRIMINATOR,
        owner: owner_key.to_bytes(),
        grid_id,
        ttl_slots,
        fee_cap,
        created_at: current_slot,
        nonce: 0,
        bump: session_bump,
    };
    let mut session_data = session.try_borrow_mut()?;
    BorshSerialize::serialize(&session_state, &mut &mut session_data[..Session::LEN]).unwrap();

    let fee_vault_state = FeeVault {
        discriminator: FeeVault::DISCRIMINATOR,
        authority: owner_key.to_bytes(),
        bump: fee_vault_bump,
    };
    let mut fee_vault_data = fee_vault.try_borrow_mut()?;
    BorshSerialize::serialize(&fee_vault_state, &mut &mut fee_vault_data[..FeeVault::LEN]).unwrap();

    pinocchio_log::log!("OpenSession success");

    Ok(())
}
