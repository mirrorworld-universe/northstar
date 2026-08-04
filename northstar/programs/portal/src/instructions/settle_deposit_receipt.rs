use {
    crate::{
        error::PortalError,
        events::{emit_transfer_event, NorthstarTransferEvent, TransferEventKind},
        instruction::SettleDepositReceipt,
        instructions::settlement::accumulate_receipt_checksum,
        pda::{find_deposit_receipt_pda, find_session_pda},
        state::{DepositReceipt, Session, SettlementStatus},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        error::ProgramError,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
};

#[p_instruction(
    id = 9,
    accounts = [
        validator(signer),
        session(mut, state = Session),
        deposit_receipt(mut, state = DepositReceipt),
        er_source,
        l1_recipient(mut)
    ],
    data = [
        er_slot: u64,
        checksum: Hash32,
        balance: u64,
        withdrawn: u64,
        payout_lamports: u64,
        l1_recipient: Pubkey
    ]
)]
pub fn process_settle_deposit_receipt(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    settle: SettleDepositReceipt,
) -> ProgramResult {
    pinocchio_log::log!(
        "Instruction: SettleDepositReceipt, er_slot={}, balance={}, withdrawn={}, payout={}",
        settle.er_slot,
        settle.balance,
        settle.withdrawn,
        settle.payout_lamports
    );

    let [validator, session, deposit_receipt, er_source, l1_recipient, ..] = accounts else {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let (expected_session_key, _) = find_session_pda(program_id);
    if session.address() != &expected_session_key {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !session.owned_by(program_id) {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session owner mismatch");
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }

    let mut session_state = Session::try_from_slice(&session.try_borrow()?).map_err(|_| {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session deserialize failed");
        PortalError::SessionDeserializeFailed
    })?;
    if !session_state.is_valid() {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: session state invalid");
        return Err(PortalError::SessionStateInvalid.into());
    }
    if !validator.is_signer() || validator.address() != &session_state.validator {
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

    let session_key = *session.address();
    let er_source_key = *er_source.address();
    let l1_recipient_key = *l1_recipient.address();
    if l1_recipient_key != settle.l1_recipient {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: l1 recipient mismatch");
        return Err(PortalError::InvalidAccountData.into());
    }
    let (expected_receipt_key, _) =
        find_deposit_receipt_pda(program_id, &session_key, &er_source_key);
    if deposit_receipt.address() != &expected_receipt_key {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: deposit receipt PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !deposit_receipt.owned_by(program_id) {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt owner mismatch");
        return Err(PortalError::InvalidAccountData.into());
    }

    let mut receipt_state = DepositReceipt::try_from_slice(&deposit_receipt.try_borrow()?)
        .map_err(|_| {
            pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt deserialize failed");
            PortalError::DepositReceiptDeserializeFailed
        })?;
    if !receipt_state.is_valid() {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt state invalid");
        return Err(PortalError::DepositReceiptStateInvalid.into());
    }
    if receipt_state.session != session_key || receipt_state.recipient != er_source_key {
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

    let rent_exempt = Rent::get()?.try_minimum_balance(crate::account_size(&receipt_state))?;
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

    let mut recipient_pre_balance = 0;
    let mut recipient_post_balance = 0;
    if settle.payout_lamports > 0 {
        recipient_pre_balance = l1_recipient.lamports();
        recipient_post_balance = recipient_pre_balance
            .checked_add(settle.payout_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
        l1_recipient.set_lamports(recipient_post_balance);
        let receipt_lamports = deposit_receipt
            .lamports()
            .checked_sub(settle.payout_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
        deposit_receipt.set_lamports(receipt_lamports);
    }

    receipt_state.balance = settle.balance;
    receipt_state.withdrawn = settle.withdrawn;
    let mut receipt_data = deposit_receipt.try_borrow_mut()?;
    BorshSerialize::serialize(&receipt_state, &mut &mut receipt_data[..]).unwrap();
    drop(receipt_data);

    session_state.settlement_accumulator = accumulate_receipt_checksum(
        session_state.settlement_accumulator,
        er_source.address(),
        l1_recipient.address(),
        settle.balance,
        settle.withdrawn,
        settle.payout_lamports,
    );
    let mut session_data = session.try_borrow_mut()?;
    BorshSerialize::serialize(&session_state, &mut &mut session_data[..]).unwrap();

    if settle.payout_lamports > 0 {
        let clock = Clock::get()?;
        emit_transfer_event(&NorthstarTransferEvent {
            version: NorthstarTransferEvent::VERSION,
            kind: TransferEventKind::Withdrawal,
            from: er_source_key,
            to: l1_recipient_key,
            lamports: settle.payout_lamports,
            pre_balance: recipient_pre_balance,
            post_balance: recipient_post_balance,
            slot: settle.er_slot,
            timestamp: clock.unix_timestamp,
        });
    }

    pinocchio_log::log!("SettleDepositReceipt success");

    Ok(())
}
