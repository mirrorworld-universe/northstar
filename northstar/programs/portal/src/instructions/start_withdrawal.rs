use {
    crate::{
        error::PortalError,
        events::{emit_transfer_event, NorthstarTransferEvent, TransferEventKind},
        WITHDRAWAL_SINK,
    },
    pinocchio::{
        account_info::AccountInfo, program_error::ProgramError, sysvars::clock::Clock,
        ProgramResult,
    },
    pinocchio_system::instructions::Transfer,
};

pub fn process_start_withdrawal(accounts: &[AccountInfo], lamports: u64) -> ProgramResult {
    pinocchio_log::log!("Instruction: StartWithdrawal, lamports={}", lamports);

    if accounts.len() < 5 {
        pinocchio_log::log!("ERROR: StartWithdrawal failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let source = &accounts[0];
    let l1_recipient = &accounts[1];
    let withdrawal_sink = &accounts[2];
    let _system_program = &accounts[3];
    let clock = Clock::from_account_info(&accounts[4])?;

    if !source.is_signer() {
        pinocchio_log::log!("ERROR: StartWithdrawal failed: source is not signer");
        return Err(PortalError::Unauthorized.into());
    }
    if withdrawal_sink.key() != &WITHDRAWAL_SINK {
        pinocchio_log::log!("ERROR: StartWithdrawal failed: withdrawal sink mismatch");
        return Err(PortalError::WithdrawalSinkMismatch.into());
    }
    if lamports == 0 {
        pinocchio_log::log!("WARN: StartWithdrawal requested 0 lamports");
        return Ok(());
    }

    let pre_balance = source.lamports();
    let post_balance = pre_balance
        .checked_sub(lamports)
        .ok_or(ProgramError::InsufficientFunds)?;

    emit_transfer_event(&NorthstarTransferEvent {
        version: NorthstarTransferEvent::VERSION,
        kind: TransferEventKind::Withdrawal,
        from: *source.key(),
        to: *l1_recipient.key(),
        lamports,
        pre_balance,
        post_balance,
        slot: clock.slot,
        timestamp: clock.unix_timestamp,
    });

    Transfer {
        from: source,
        to: withdrawal_sink,
        lamports,
    }
    .invoke()?;

    pinocchio_log::log!("StartWithdrawal success");
    Ok(())
}
