use {
    crate::{
        error::PortalError,
        instruction::SettleDepositReceipt,
        instructions::settlement::accumulate_receipt_checksum,
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
        "Instruction: SettleDepositReceipt, er_slot={}, balance={}, withdrawn={}, payout={}",
        settle.er_slot,
        settle.balance,
        settle.withdrawn,
        settle.payout_lamports
    );

    if accounts.len() < 5 {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let validator = &accounts[0];
    let session = &accounts[1];
    let deposit_receipt = &accounts[2];
    let er_source = &accounts[3];
    let l1_recipient = &accounts[4];

    let (expected_session_key, _) = find_session_pda(program_id);
    if session.key() != &expected_session_key {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if session.owner() != program_id {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session owner mismatch");
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }

    let mut session_state = Session::try_from_slice(&session.try_borrow_data()?).map_err(|_| {
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
    let er_source_key = er_source.key();
    let l1_recipient_key = l1_recipient.key();
    if l1_recipient_key != &settle.l1_recipient {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: l1 recipient mismatch");
        return Err(PortalError::InvalidAccountData.into());
    }
    let (expected_receipt_key, _) =
        find_deposit_receipt_pda(program_id, session_key, er_source_key);
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
    if receipt_state.session != *session_key || receipt_state.recipient != *er_source_key {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt state seeds mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    if receipt_state.balance == settle.balance && receipt_state.withdrawn >= settle.withdrawn {
        pinocchio_log::log!("SettleDepositReceipt duplicate; already settled");
        return Ok(());
    }
    if settle.withdrawn < receipt_state.withdrawn {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: withdrawn counter regressed");
        return Err(PortalError::InvalidAccountData.into());
    }

    let withdrawn_delta = settle
        .withdrawn
        .checked_sub(receipt_state.withdrawn)
        .ok_or(PortalError::ArithmeticOverflow)?;
    if settle.payout_lamports != withdrawn_delta {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: payout mismatch");
        return Err(PortalError::InvalidAccountData.into());
    }

    let rent_exempt = Rent::get()?.minimum_balance(DepositReceipt::LEN);
    let escrow_lamports = deposit_receipt
        .lamports()
        .checked_sub(rent_exempt)
        .ok_or(PortalError::InsufficientFees)?;
    let escrow_after_payout = escrow_lamports
        .checked_sub(settle.payout_lamports)
        .ok_or(PortalError::InsufficientFees)?;
    if settle.balance > escrow_after_payout {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: balance exceeds escrow");
        return Err(PortalError::InsufficientFees.into());
    }

    if settle.payout_lamports > 0 {
        {
            let mut recipient_lamports = l1_recipient.try_borrow_mut_lamports()?;
            *recipient_lamports = recipient_lamports
                .checked_add(settle.payout_lamports)
                .ok_or(PortalError::ArithmeticOverflow)?;
        }
        *deposit_receipt.try_borrow_mut_lamports()? = deposit_receipt
            .lamports()
            .checked_sub(settle.payout_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
    }

    receipt_state.balance = settle.balance;
    receipt_state.withdrawn = settle.withdrawn;
    let mut receipt_data = deposit_receipt.try_borrow_mut_data()?;
    BorshSerialize::serialize(
        &receipt_state,
        &mut &mut receipt_data[..DepositReceipt::LEN],
    )
    .unwrap();
    drop(receipt_data);

    session_state.settlement_accumulator = accumulate_receipt_checksum(
        session_state.settlement_accumulator,
        er_source.key(),
        l1_recipient.key(),
        settle.balance,
        settle.withdrawn,
        settle.payout_lamports,
    );
    let mut session_data = session.try_borrow_mut_data()?;
    BorshSerialize::serialize(&session_state, &mut &mut session_data[..Session::LEN]).unwrap();

    pinocchio_log::log!("SettleDepositReceipt success");

    Ok(())
}
