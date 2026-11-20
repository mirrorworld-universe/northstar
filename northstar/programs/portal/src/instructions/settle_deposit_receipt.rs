use {
    crate::{
        error::PortalError,
        events::{emit_transfer_event, NorthstarTransferEvent, TransferEventKind},
        instruction::SettleDepositReceipt,
        instructions::settlement::accumulate_receipt_checksum,
        pda::{find_deposit_receipt_pda, find_session_pda},
        receipt::{
            payout_delta, plan_payment, same_binding, Binding, Counters, Lamports, ReceiptError,
        },
        state::SettlementStatus,
    },
    borsh::BorshDeserialize,
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

    let mut session_state = crate::continuity::parse_session(&session.try_borrow()?)?;
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
    let context = if session
        .try_borrow()?
        .starts_with(&crate::SessionV2::DISCRIMINATOR)
    {
        Some(
            crate::SessionV2::try_from_slice(&session.try_borrow()?)
                .map_err(|_| PortalError::SessionDeserializeFailed)?
                .identity(program_id, &session_key)
                .commitment(|parts| solana_sha256_hasher::hashv(parts).to_bytes())
                .map_err(|_| PortalError::InvalidAccountData)?,
        )
    } else {
        None
    };
    let (expected_receipt_key, receipt_bump) = match context {
        Some(context) => crate::find_deposit_receipt_v2_pda(program_id, &context, &er_source_key),
        None => find_deposit_receipt_pda(program_id, &session_key, &er_source_key),
    };
    if deposit_receipt.address() != &expected_receipt_key {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: deposit receipt PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !deposit_receipt.owned_by(program_id) {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt owner mismatch");
        return Err(PortalError::InvalidAccountData.into());
    }

    let (mut receipt_state, scope) =
        crate::continuity::read_receipt_frame(&deposit_receipt.try_borrow()?)?;
    if scope.is_some() != context.is_some() {
        return Err(PortalError::DepositReceiptStateInvalid.into());
    }
    if scope.as_ref().map(|s| s.context) != context {
        return Err(PortalError::InvalidAccountData.into());
    }
    if !same_binding(
        receipt_state.binding(),
        Binding {
            session: session_key.as_array(),
            recipient: er_source_key.as_array(),
            bump: receipt_bump,
        },
    ) {
        pinocchio_log::log!("ERROR: SettleDepositReceipt failed: receipt state seeds mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let old = Counters {
        balance: receipt_state.balance,
        withdrawn: receipt_state.withdrawn,
    };
    let next = Counters {
        balance: settle.balance,
        withdrawn: settle.withdrawn,
    };
    let Some(payout) = payout_delta(old, next, settle.payout_lamports).map_err(receipt_error)?
    else {
        pinocchio_log::log!("SettleDepositReceipt duplicate; already settled");
        return Ok(());
    };
    let before = Lamports {
        receipt: deposit_receipt.lamports(),
        recipient: l1_recipient.lamports(),
    };
    let after = plan_payment(
        next,
        payout,
        before,
        Rent::get()?.try_minimum_balance(deposit_receipt.data_len())?,
        scope.map(|s| s.cap),
        deposit_receipt.address() == l1_recipient.address(),
    )
    .map_err(receipt_error)?;
    if payout > 0 {
        deposit_receipt.set_lamports(after.receipt);
        l1_recipient.set_lamports(after.recipient);
    }

    receipt_state.balance = next.balance;
    receipt_state.withdrawn = next.withdrawn;
    let mut receipt_data = deposit_receipt.try_borrow_mut()?;
    crate::continuity::write_deposit_receipt(&mut receipt_data, &receipt_state)?;
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
    crate::continuity::write_session(&mut session_data, &session_state)?;

    if payout > 0 {
        let clock = Clock::get()?;
        emit_transfer_event(&NorthstarTransferEvent {
            version: NorthstarTransferEvent::VERSION,
            kind: TransferEventKind::Withdrawal,
            from: er_source_key,
            to: l1_recipient_key,
            lamports: payout,
            pre_balance: before.recipient,
            post_balance: after.recipient,
            slot: settle.er_slot,
            timestamp: clock.unix_timestamp,
        });
    }

    pinocchio_log::log!("SettleDepositReceipt success");

    Ok(())
}

fn receipt_error(error: ReceiptError) -> PortalError {
    match error {
        ReceiptError::InvalidUpdate => PortalError::InvalidAccountData,
        ReceiptError::InsufficientBacking => PortalError::InsufficientFees,
        ReceiptError::Overflow => PortalError::ArithmeticOverflow,
        ReceiptError::DepositCap => PortalError::DepositReceiptStateInvalid,
    }
}
