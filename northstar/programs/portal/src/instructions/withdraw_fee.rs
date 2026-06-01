use {
    crate::{
        error::PortalError,
        pda::{find_deposit_receipt_pda, find_session_pda},
        state::{DepositReceipt, Session},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        ProgramResult, account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey,
    },
};

pub fn process_withdraw_fee(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    lamports: u64,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: WithdrawFee, lamports={}", lamports);

    if accounts.len() < 4 {
        pinocchio_log::log!("ERROR: WithdrawFee failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let recipient = &accounts[0];
    let session = &accounts[1];
    let deposit_receipt = &accounts[2];
    let _system_program = &accounts[3];

    if !recipient.is_signer() {
        pinocchio_log::log!("ERROR: WithdrawFee failed: recipient is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    let (expected_session_key, _) = find_session_pda(program_id);
    if session.key() != &expected_session_key {
        pinocchio_log::log!("ERROR: WithdrawFee failed: session PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if session.owner() != program_id {
        pinocchio_log::log!("ERROR: WithdrawFee failed: session owner mismatch");
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }

    let session_state = Session::try_from_slice(&session.try_borrow_data()?).map_err(|_| {
        pinocchio_log::log!("ERROR: WithdrawFee failed: session deserialize failed");
        PortalError::SessionDeserializeFailed
    })?;
    if !session_state.is_valid() {
        pinocchio_log::log!("ERROR: WithdrawFee failed: session state invalid");
        return Err(PortalError::SessionStateInvalid.into());
    }

    let session_key = session.key();
    let recipient_key = recipient.key();
    let (expected_receipt_key, _) =
        find_deposit_receipt_pda(program_id, session_key, recipient_key);
    if deposit_receipt.key() != &expected_receipt_key {
        pinocchio_log::log!("ERROR: WithdrawFee failed: deposit receipt PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if deposit_receipt.owner() != program_id {
        pinocchio_log::log!("ERROR: WithdrawFee failed: receipt owner mismatch");
        return Err(PortalError::InvalidAccountData.into());
    }

    if lamports == 0 {
        pinocchio_log::log!("WARN: Withdrew 0 lamports");
        return Ok(());
    }

    let mut receipt_state = DepositReceipt::try_from_slice(&deposit_receipt.try_borrow_data()?)
        .map_err(|_| {
            pinocchio_log::log!("ERROR: WithdrawFee failed: receipt deserialize failed");
            PortalError::DepositReceiptDeserializeFailed
        })?;

    if !receipt_state.is_valid() {
        pinocchio_log::log!("ERROR: WithdrawFee failed: receipt state invalid");
        return Err(PortalError::DepositReceiptStateInvalid.into());
    }
    if receipt_state.session != *session_key || receipt_state.recipient != *recipient_key {
        pinocchio_log::log!("ERROR: WithdrawFee failed: receipt state seeds mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if receipt_state.balance < lamports {
        pinocchio_log::log!("ERROR: WithdrawFee failed: insufficient receipt balance");
        return Err(PortalError::InsufficientFees.into());
    }
    if deposit_receipt.lamports() < lamports {
        pinocchio_log::log!("ERROR: WithdrawFee failed: insufficient receipt lamports");
        return Err(PortalError::InsufficientFees.into());
    }

    receipt_state.balance = receipt_state.balance.checked_sub(lamports).ok_or_else(|| {
        pinocchio_log::log!("ERROR: WithdrawFee failed: arithmetic underflow");
        PortalError::ArithmeticOverflow
    })?;

    {
        let mut receipt_data = deposit_receipt.try_borrow_mut_data()?;
        BorshSerialize::serialize(
            &receipt_state,
            &mut &mut receipt_data[..DepositReceipt::LEN],
        )
        .unwrap();
    }

    {
        let mut recipient_lamports = recipient.try_borrow_mut_lamports()?;
        *recipient_lamports = recipient_lamports.checked_add(lamports).ok_or_else(|| {
            pinocchio_log::log!("ERROR: WithdrawFee failed: recipient lamport overflow");
            PortalError::ArithmeticOverflow
        })?;
    }
    *deposit_receipt.try_borrow_mut_lamports()? = deposit_receipt
        .lamports()
        .checked_sub(lamports)
        .ok_or_else(|| {
            pinocchio_log::log!("ERROR: WithdrawFee failed: receipt lamport underflow");
            PortalError::ArithmeticOverflow
        })?;

    pinocchio_log::log!("WithdrawFee success");

    Ok(())
}
