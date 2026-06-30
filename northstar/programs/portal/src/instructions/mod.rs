use {
    crate::PortalError,
    pinocchio::{account_info::AccountInfo, instruction::Signer, pubkey::Pubkey, ProgramResult},
    pinocchio_system::instructions::{Allocate, Assign, Transfer},
};

pub mod checkpoint;
pub mod close_session;
pub mod delegate;
pub mod deposit_fee;
pub mod open_session;
pub mod settle_deposit_receipt;
pub mod settlement;
pub mod start_withdrawal;
pub mod undelegate;

fn initialize_pda_account(
    payer: &AccountInfo,
    pda: &AccountInfo,
    lamports: u64,
    space: u64,
    owner: &Pubkey,
    signer: Signer,
) -> ProgramResult {
    if !pda.data_is_empty() || pda.owner() != &pinocchio_system::ID {
        return Err(PortalError::InvalidAccountData.into());
    }

    if pda.lamports() < lamports {
        Transfer {
            from: payer,
            to: pda,
            lamports: lamports
                .checked_sub(pda.lamports())
                .ok_or(PortalError::ArithmeticOverflow)?,
        }
        .invoke()?;
    }

    Allocate {
        account: pda,
        space,
    }
    .invoke_signed(&[signer.clone()])?;
    Assign {
        account: pda,
        owner,
    }
    .invoke_signed(&[signer])?;

    Ok(())
}

pub use {
    checkpoint::{
        process_cancel_checkpoint, process_challenge_checkpoint, process_commit_checkpoint,
        process_create_step_proof, process_propose_checkpoint, process_seal_step_proof,
        process_submit_step_proof, process_write_step_proof,
    },
    close_session::process_close_session,
    delegate::process_delegate,
    deposit_fee::process_deposit_fee,
    open_session::process_open_session,
    settle_deposit_receipt::process_settle_deposit_receipt,
    settlement::{
        process_abort_settlement, process_begin_settlement, process_finish_settlement,
        process_settle_account_lamports, process_settle_account_owner,
        process_write_settlement_chunk,
    },
    start_withdrawal::process_start_withdrawal,
    undelegate::{process_undelegate, process_undelegate_handoff},
};
