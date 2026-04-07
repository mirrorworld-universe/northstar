use {
    crate::{
        error::PortalError,
        pda::{find_fee_vault_pda, find_session_pda},
        state::Session,
    },
    borsh::BorshDeserialize,
    pinocchio::{
        account_info::AccountInfo,
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{clock::Clock, Sysvar},
        ProgramResult,
    },
};

pub fn process_close_session(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    grid_id: u64,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CloseSession, grid_id={}", grid_id);

    // TODO: close_session should iterate and refund all DepositReceipt PDAs
    // associated with this session back to their respective recipients.
    // For now, deposit receipts persist independently after session close.
    if accounts.len() < 4 {
        pinocchio_log::log!("ERROR: CloseSession failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let owner = &accounts[0];
    let session = &accounts[1];
    let fee_vault = &accounts[2];
    let _system_program = &accounts[3];

    if !owner.is_signer() {
        pinocchio_log::log!("ERROR: CloseSession failed: owner is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    let owner_key = *owner.key();
    let (expected_session_key, _) = find_session_pda(program_id, &owner_key, grid_id);
    let (expected_fee_vault_key, _) = find_fee_vault_pda(program_id, &owner_key);

    if session.key() != &expected_session_key {
        pinocchio_log::log!("ERROR: CloseSession failed: session PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if fee_vault.key() != &expected_fee_vault_key {
        pinocchio_log::log!("ERROR: CloseSession failed: fee vault PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let session_state = {
        let session_data = session.try_borrow_data()?;
        Session::try_from_slice(&session_data).map_err(|_| {
            pinocchio_log::log!("ERROR: CloseSession failed: session deserialize failed");
            PortalError::SessionDeserializeFailed
        })?
    };

    if !session_state.is_valid() {
        pinocchio_log::log!("ERROR: CloseSession failed: session state invalid");
        return Err(PortalError::SessionStateInvalid.into());
    }

    if session_state.owner != owner_key {
        pinocchio_log::log!("ERROR: CloseSession failed: unauthorized owner");
        return Err(PortalError::Unauthorized.into());
    }

    let clock = Clock::get()?;
    let current_slot = clock.slot;

    if !session_state.is_expired(current_slot) {
        pinocchio_log::log!("ERROR: CloseSession failed: session still active");
        return Err(PortalError::SessionStillActive.into());
    }

    // Transfer all lamports from fee_vault and session back to owner
    if fee_vault.lamports() > 0 {
        let mut owner_lamports = owner.try_borrow_mut_lamports()?;
        *owner_lamports = owner_lamports
            .checked_add(fee_vault.lamports())
            .ok_or_else(|| {
                pinocchio_log::log!("ERROR: CloseSession failed: arithmetic overflow on fee vault refund");
                PortalError::ArithmeticOverflow
            })?;
    }
    *fee_vault.try_borrow_mut_lamports()? = 0;

    if session.lamports() > 0 {
        let mut owner_lamports = owner.try_borrow_mut_lamports()?;
        *owner_lamports = owner_lamports
            .checked_add(session.lamports())
            .ok_or_else(|| {
                pinocchio_log::log!("ERROR: CloseSession failed: arithmetic overflow on session refund");
                PortalError::ArithmeticOverflow
            })?;
    }

    *session.try_borrow_mut_lamports()? = 0;

    // Zero account data
    fee_vault.try_borrow_mut_data()?.fill(0);
    session.try_borrow_mut_data()?.fill(0);

    fee_vault.close()?;
    session.close()?;

    pinocchio_log::log!("CloseSession success");

    Ok(())
}
