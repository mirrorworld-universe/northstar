use {
    crate::{
        error::PortalError,
        instruction::SettleDepositReceipt,
        pda::{find_deposit_receipt_pda, find_session_pda},
        state::{DepositReceipt, Session, SettlementStatus},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        ProgramResult,
        account_info::AccountInfo,
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{Sysvar, rent::Rent},
    },
};

pub fn process_settle_deposit_receipt(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    settle: SettleDepositReceipt,
) -> ProgramResult {
    pinocchio_log::log!(
        "Instruction: SettleDepositReceipt, er_slot={}, balance={}",
        settle.er_slot,
        settle.balance
    );

    if accounts.len() < 4 {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let validator = &accounts[0];
    let session = &accounts[1];
    let deposit_receipt = &accounts[2];
    let recipient = &accounts[3];

    let (expected_session_key, _) = find_session_pda(program_id);
    if session.key() != &expected_session_key {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if session.owner() != program_id {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session owner mismatch");
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }

    let session_state = Session::try_from_slice(&session.try_borrow_data()?).map_err(|_| {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session deserialize failed");
        PortalError::SessionDeserializeFailed
    })?;
    if !session_state.is_valid() {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session state invalid");
        return Err(PortalError::SessionStateInvalid.into());
    }
    if !validator.is_signer() || validator.key() != &session_state.validator {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: validator unauthorized");
        return Err(PortalError::Unauthorized.into());
    }
    if session_state.settlement_status != SettlementStatus::InProgress {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: settlement not in progress");
        return Err(PortalError::SettlementNotInProgress.into());
    }
    if settle.er_slot != session_state.settlement_er_slot {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: ER slot mismatch");
        return Err(PortalError::SettlementErSlotMismatch.into());
    }
    if settle.checksum != session_state.settlement_checksum {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: checksum mismatch");
        return Err(PortalError::SettlementChecksumMismatch.into());
    }

    let session_key = session.key();
    let recipient_key = recipient.key();
    let (expected_receipt_key, _) =
        find_deposit_receipt_pda(program_id, session_key, recipient_key);
    if deposit_receipt.key() != &expected_receipt_key {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: deposit receipt PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if deposit_receipt.owner() != program_id {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt owner mismatch");
        return Err(PortalError::InvalidAccountData.into());
    }

    let mut receipt_state = DepositReceipt::try_from_slice(&deposit_receipt.try_borrow_data()?)
        .map_err(|_| {
            pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt deserialize failed");
            PortalError::DepositReceiptDeserializeFailed
        })?;
    if !receipt_state.is_valid() {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt state invalid");
        return Err(PortalError::DepositReceiptStateInvalid.into());
    }
    if receipt_state.session != *session_key || receipt_state.recipient != *recipient_key {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt state seeds mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let rent_exempt = Rent::get()?.minimum_balance(DepositReceipt::LEN);
    let escrow_lamports = deposit_receipt
        .lamports()
        .checked_sub(rent_exempt)
        .ok_or(PortalError::InsufficientFees)?;
    if settle.balance > escrow_lamports {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: balance exceeds escrow");
        return Err(PortalError::InsufficientFees.into());
    }

    receipt_state.balance = settle.balance;
    let mut receipt_data = deposit_receipt.try_borrow_mut_data()?;
    BorshSerialize::serialize(
        &receipt_state,
        &mut &mut receipt_data[..DepositReceipt::LEN],
    )
    .unwrap();

    pinocchio_log::log!("SettleDepositReceipt success");

    Ok(())
}
