use {
    crate::{
        error::PortalError,
        pda::find_deposit_receipt_pda,
        state::{DepositReceipt, Session},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        AccountView, Address, ProgramResult,
        cpi::{Seed, Signer},
        error::ProgramError,
        sysvars::{Sysvar, clock::Clock, rent::Rent},
    },
    pinocchio_system::instructions::{CreateAccount, Transfer},
};

pub fn process_deposit_fee(
    program_id: &Address,
    accounts: &mut [AccountView],
    lamports: u64,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: DepositFee, lamports={}", lamports);

    let [
        depositor,
        session,
        deposit_receipt,
        recipient,
        _system_program,
        ..,
    ] = accounts
    else {
        pinocchio_log::log!("ERROR: DepositFee failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !depositor.is_signer() {
        pinocchio_log::log!("ERROR: DepositFee failed: depositor is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    if !session.owned_by(program_id) {
        pinocchio_log::log!("ERROR: DepositFee failed: session owner mismatch");
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }

    let session_data = session.try_borrow()?;
    let session_state = Session::try_from_slice(&session_data).map_err(|_| {
        pinocchio_log::log!("ERROR: DepositFee failed: session deserialize failed");
        PortalError::SessionDeserializeFailed
    })?;

    if !session_state.is_valid() {
        pinocchio_log::log!("ERROR: DepositFee failed: session state invalid");
        return Err(PortalError::SessionStateInvalid.into());
    }

    if session_state.is_expired(Clock::get()?.slot) {
        pinocchio_log::log!("ERROR: DepositFee failed: session expired");
        return Err(PortalError::SessionExpired.into());
    }

    if lamports == 0 {
        pinocchio_log::log!("WARN: Deposited 0 lamports");
        return Ok(());
    }

    let session_key = *session.address();
    let recipient_key = *recipient.address();
    let (expected_receipt_key, receipt_bump) =
        find_deposit_receipt_pda(program_id, &session_key, &recipient_key);

    if deposit_receipt.address() != &expected_receipt_key {
        pinocchio_log::log!("ERROR: DepositFee failed: deposit receipt PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    if deposit_receipt.is_data_empty() {
        let rent = Rent::get()?;
        let receipt_lamports = rent.try_minimum_balance(DepositReceipt::LEN)?;

        let receipt_bump_bytes = [receipt_bump];
        let receipt_seeds = [
            Seed::from(DepositReceipt::SEED_PREFIX),
            Seed::from(session_key.as_ref()),
            Seed::from(recipient_key.as_ref()),
            Seed::from(receipt_bump_bytes.as_ref()),
        ];
        let receipt_signer = Signer::from(&receipt_seeds);

        CreateAccount {
            from: depositor,
            to: deposit_receipt,
            lamports: receipt_lamports,
            space: DepositReceipt::LEN as u64,
            owner: program_id,
        }
        .invoke_signed(&[receipt_signer])?;

        let receipt_state = DepositReceipt {
            discriminator: DepositReceipt::DISCRIMINATOR,
            session: session_key.to_bytes(),
            recipient: recipient_key.to_bytes(),
            balance: lamports,
            bump: receipt_bump,
        };
        let mut receipt_data = deposit_receipt.try_borrow_mut()?;
        BorshSerialize::serialize(
            &receipt_state,
            &mut &mut receipt_data[..DepositReceipt::LEN],
        )
        .unwrap();
    } else {
        let receipt_data = deposit_receipt.try_borrow()?;
        let mut receipt_state = DepositReceipt::try_from_slice(&receipt_data).map_err(|_| {
            pinocchio_log::log!("ERROR: DepositFee failed: receipt deserialize failed");
            PortalError::DepositReceiptDeserializeFailed
        })?;

        if !receipt_state.is_valid() {
            pinocchio_log::log!("ERROR: DepositFee failed: receipt state invalid");
            return Err(PortalError::DepositReceiptStateInvalid.into());
        }

        receipt_state.balance = receipt_state
            .balance
            .checked_add(lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;

        drop(receipt_data);
        let mut receipt_data = deposit_receipt.try_borrow_mut()?;
        BorshSerialize::serialize(
            &receipt_state,
            &mut &mut receipt_data[..DepositReceipt::LEN],
        )
        .unwrap();
    }

    Transfer {
        from: depositor,
        to: deposit_receipt,
        lamports,
    }
    .invoke()?;

    pinocchio_log::log!("DepositFee success");

    Ok(())
}
