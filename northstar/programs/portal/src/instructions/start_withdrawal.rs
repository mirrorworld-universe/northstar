use {
    crate::{
        error::PortalError,
        events::{emit_transfer_event, NorthstarTransferEvent, TransferEventKind},
        WITHDRAWAL_SINK,
    },
    pinocchio::{
        error::ProgramError, sysvars::clock::Clock, AccountView as AccountInfo, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
    pinocchio_system::instructions::Transfer,
};

#[p_instruction(
    id = 13,
    accounts = [
        source(signer, mut),
        l1_recipient,
        withdrawal_sink(mut),
        system_program,
        clock
    ],
    data = [lamports: u64]
)]
pub fn process_start_withdrawal(accounts: &mut [AccountInfo], lamports: u64) -> ProgramResult {
    pinocchio_log::log!("Instruction: StartWithdrawal, lamports={}", lamports);

    let [source, l1_recipient, withdrawal_sink, _system_program, clock_account, ..] = accounts
    else {
        pinocchio_log::log!("ERROR: StartWithdrawal failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let clock = Clock::from_account_view(clock_account)?;

    if !source.is_signer() {
        pinocchio_log::log!("ERROR: StartWithdrawal failed: source is not signer");
        return Err(PortalError::Unauthorized.into());
    }
    if withdrawal_sink.address() != &WITHDRAWAL_SINK {
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
        from: *source.address(),
        to: *l1_recipient.address(),
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
