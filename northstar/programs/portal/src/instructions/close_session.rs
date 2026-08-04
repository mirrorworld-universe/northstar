use {
    crate::{
        error::PortalError,
        pda::{find_checkpoint_cursor_pda, find_fee_vault_pda, find_session_pda},
        state::{CheckpointCursor, FeeVault, Session, SettlementStatus},
    },
    borsh::BorshDeserialize,
    pinocchio::{
        error::ProgramError, AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
};

#[p_instruction(
    id = 1,
    accounts = [
        closer(signer, mut),
        session(mut, state = Session),
        fee_vault(mut, state = FeeVault),
        system_program,
        checkpoint_cursor(mut, state = CheckpointCursor)
    ]
)]
pub fn process_close_session(program_id: &Pubkey, accounts: &mut [AccountInfo]) -> ProgramResult {
    pinocchio_log::log!("Instruction: CloseSession");

    // TODO: close_session should iterate and refund all DepositReceipt PDAs
    // associated with this session back to their respective recipients.
    // For now, deposit receipts persist independently after session close.
    let [closer, session, fee_vault, _system_program, checkpoint_cursor, ..] = accounts else {
        pinocchio_log::log!("ERROR: CloseSession failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !closer.is_signer() {
        pinocchio_log::log!("ERROR: CloseSession failed: closer is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    let (expected_session_key, _) = find_session_pda(program_id);
    let (expected_fee_vault_key, _) = find_fee_vault_pda(program_id);
    let (expected_cursor_key, _) = find_checkpoint_cursor_pda(program_id, session.address());

    if session.address() != &expected_session_key {
        pinocchio_log::log!("ERROR: CloseSession failed: session PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if fee_vault.address() != &expected_fee_vault_key {
        pinocchio_log::log!("ERROR: CloseSession failed: fee vault PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if checkpoint_cursor.address() != &expected_cursor_key {
        pinocchio_log::log!("ERROR: CloseSession failed: cursor PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let session_state = {
        let session_data = session.try_borrow()?;
        Session::try_from_slice(&session_data).map_err(|_| {
            pinocchio_log::log!("ERROR: CloseSession failed: session deserialize failed");
            PortalError::SessionDeserializeFailed
        })?
    };

    if !session_state.is_valid() {
        pinocchio_log::log!("ERROR: CloseSession failed: session state invalid");
        return Err(PortalError::SessionStateInvalid.into());
    }

    if closer.address() != &session_state.authority {
        return Err(PortalError::Unauthorized.into());
    }

    let fee_vault_state = {
        let fee_vault_data = fee_vault.try_borrow()?;
        FeeVault::try_from_slice(&fee_vault_data).map_err(|_| PortalError::InvalidAccountData)?
    };
    if !fee_vault_state.is_valid() || fee_vault_state.authority != closer.address().to_bytes() {
        return Err(PortalError::Unauthorized.into());
    }

    if session_state.settlement_status == SettlementStatus::InProgress {
        pinocchio_log::log!("ERROR: CloseSession failed: settlement in progress");
        return Err(PortalError::SettlementInProgress.into());
    }

    if !checkpoint_cursor.is_data_empty() {
        if !checkpoint_cursor.owned_by(program_id) {
            return Err(PortalError::CheckpointCursorStateInvalid.into());
        }
        let cursor_state = CheckpointCursor::try_from_slice(&checkpoint_cursor.try_borrow()?)
            .map_err(|_| PortalError::CheckpointCursorDeserializeFailed)?;
        if !cursor_state.is_valid() || cursor_state.session != *session.address() {
            return Err(PortalError::CheckpointCursorStateInvalid.into());
        }
        if cursor_state.active_checkpoint != [0; 32].into() {
            return Err(PortalError::CheckpointActiveExists.into());
        }
    }

    // Transfer all lamports from fee_vault and session back to the closer.
    if fee_vault.lamports() > 0 {
        let closer_lamports = closer
            .lamports()
            .checked_add(fee_vault.lamports())
            .ok_or_else(|| {
                pinocchio_log::log!(
                    "ERROR: CloseSession failed: arithmetic overflow on fee vault refund"
                );
                PortalError::ArithmeticOverflow
            })?;
        closer.set_lamports(closer_lamports);
    }
    fee_vault.set_lamports(0);

    if session.lamports() > 0 {
        let closer_lamports = closer
            .lamports()
            .checked_add(session.lamports())
            .ok_or_else(|| {
                pinocchio_log::log!(
                    "ERROR: CloseSession failed: arithmetic overflow on session refund"
                );
                PortalError::ArithmeticOverflow
            })?;
        closer.set_lamports(closer_lamports);
    }

    if checkpoint_cursor.lamports() > 0 {
        let closer_lamports = closer
            .lamports()
            .checked_add(checkpoint_cursor.lamports())
            .ok_or(PortalError::ArithmeticOverflow)?;
        closer.set_lamports(closer_lamports);
        checkpoint_cursor.set_lamports(0);
        checkpoint_cursor.try_borrow_mut()?.fill(0);
        checkpoint_cursor.close()?;
    }

    session.set_lamports(0);

    // Zero account data
    fee_vault.try_borrow_mut()?.fill(0);
    session.try_borrow_mut()?.fill(0);

    fee_vault.close()?;
    session.close()?;

    pinocchio_log::log!("CloseSession success");

    Ok(())
}
