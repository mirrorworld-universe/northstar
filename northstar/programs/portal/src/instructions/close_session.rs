use {
    crate::{
        error::PortalError,
        pda::{find_fee_vault_pda, find_session_pda},
        state::Session,
    },
    borsh::BorshDeserialize,
    pinocchio::{
        AccountView, Address, ProgramResult,
        error::ProgramError,
        sysvars::{Sysvar, clock::Clock},
    },
};

pub fn process_close_session(
    program_id: &Address,
    accounts: &mut [AccountView],
    grid_id: u64,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CloseSession, grid_id={}", grid_id);

    // TODO: close_session should iterate and refund all DepositReceipt PDAs
    // associated with this session back to their respective recipients.
    // For now, deposit receipts persist independently after session close.
    let [owner, session, fee_vault, _system_program, ..] = accounts else {
        pinocchio_log::log!("ERROR: CloseSession failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !owner.is_signer() {
        pinocchio_log::log!("ERROR: CloseSession failed: owner is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    let owner_key = *owner.address();
    let (expected_session_key, _) = find_session_pda(program_id, &owner_key, grid_id);
    let (expected_fee_vault_key, _) = find_fee_vault_pda(program_id, &owner_key);

    if session.address() != &expected_session_key {
        pinocchio_log::log!("ERROR: CloseSession failed: session PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if fee_vault.address() != &expected_fee_vault_key {
        pinocchio_log::log!("ERROR: CloseSession failed: fee vault PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let session_data = session.try_borrow()?;
    let session_state = Session::try_from_slice(&session_data).map_err(|_| {
        pinocchio_log::log!("ERROR: CloseSession failed: session deserialize failed");
        PortalError::SessionDeserializeFailed
    })?;
    drop(session_data);

    if !session_state.is_valid() {
        pinocchio_log::log!("ERROR: CloseSession failed: session state invalid");
        return Err(PortalError::SessionStateInvalid.into());
    }

    if session_state.owner != owner_key.to_bytes() {
        pinocchio_log::log!("ERROR: CloseSession failed: unauthorized owner");
        return Err(PortalError::Unauthorized.into());
    }

    if !session_state.is_expired(Clock::get()?.slot) {
        pinocchio_log::log!("ERROR: CloseSession failed: session still active");
        return Err(PortalError::SessionStillActive.into());
    }

    if fee_vault.lamports() > 0 {
        let new_owner_lamports = owner
            .lamports()
            .checked_add(fee_vault.lamports())
            .ok_or_else(|| {
                pinocchio_log::log!(
                    "ERROR: CloseSession failed: arithmetic overflow on fee vault refund"
                );
                PortalError::ArithmeticOverflow
            })?;
        owner.set_lamports(new_owner_lamports);
    }
    fee_vault.set_lamports(0);

    if session.lamports() > 0 {
        let new_owner_lamports = owner
            .lamports()
            .checked_add(session.lamports())
            .ok_or_else(|| {
                pinocchio_log::log!(
                    "ERROR: CloseSession failed: arithmetic overflow on session refund"
                );
                PortalError::ArithmeticOverflow
            })?;
        owner.set_lamports(new_owner_lamports);
    }
    session.set_lamports(0);

    session.try_borrow_mut()?.fill(0);
    fee_vault.try_borrow_mut()?.fill(0);

    fee_vault.close()?;
    session.close()?;

    pinocchio_log::log!("CloseSession success");

    Ok(())
}
