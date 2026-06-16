use {
    crate::{
        error::PortalError,
        pda::{find_deposit_receipt_pda, find_withdrawal_sink_pda},
        state::{DepositReceipt, Session, WithdrawalSink},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        account_info::AccountInfo,
        instruction::{Seed, Signer},
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        ProgramResult,
    },
    pinocchio_system::instructions::{CreateAccount, Transfer},
};

pub fn process_deposit_fee(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    lamports: u64,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: DepositFee, lamports={}", lamports);

    if accounts.len() < 5 {
        pinocchio_log::log!("ERROR: DepositFee failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let depositor = &accounts[0];
    let session = &accounts[1];
    let deposit_receipt = &accounts[2]; // lamport receiver account belong to this program
    let recipient = &accounts[3]; // who will receive the lamports
    let _system_program = &accounts[4];
    let withdrawal_sink = accounts.get(5);

    if !depositor.is_signer() {
        pinocchio_log::log!("ERROR: DepositFee failed: depositor is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    // Validate session is owned by portal program
    if session.owner() != program_id {
        pinocchio_log::log!("ERROR: DepositFee failed: session owner mismatch");
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }

    let session_state = Session::try_from_slice(&session.try_borrow_data()?).map_err(|_| {
        pinocchio_log::log!("ERROR: DepositFee failed: session deserialize failed");
        PortalError::SessionDeserializeFailed
    })?;

    if !session_state.is_valid() {
        pinocchio_log::log!("ERROR: DepositFee failed: session state invalid");
        return Err(PortalError::SessionStateInvalid.into());
    }

    // Check session is not expired
    let clock = Clock::get()?;
    if session_state.is_expired(clock.slot) {
        pinocchio_log::log!("ERROR: DepositFee failed: session expired");
        return Err(PortalError::SessionExpired.into());
    }

    if lamports == 0 {
        pinocchio_log::log!("WARN: Deposited 0 lamports");
        return Ok(());
    }

    // Validate deposit_receipt PDA
    let session_key = session.key();
    let recipient_key = recipient.key();
    let (expected_receipt_key, receipt_bump) =
        find_deposit_receipt_pda(program_id, session_key, recipient_key);

    if deposit_receipt.key() != &expected_receipt_key {
        pinocchio_log::log!("ERROR: DepositFee failed: deposit receipt PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    // Create or update DepositReceipt
    if deposit_receipt.data_is_empty() {
        // First deposit — create the PDA
        let rent = Rent::get()?;
        let receipt_lamports = rent.minimum_balance(DepositReceipt::LEN);

        let receipt_bump_bytes = [receipt_bump];
        let receipt_seeds = &[
            Seed::from(DepositReceipt::SEED_PREFIX),
            Seed::from(session_key.as_ref()),
            Seed::from(recipient_key.as_ref()),
            Seed::from(receipt_bump_bytes.as_ref()),
        ];
        let receipt_signer = Signer::from(receipt_seeds);

        // Create account — depositor pays rent
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
            session: *session_key,
            recipient: *recipient_key,
            balance: 0,
            withdrawn: 0,
            bump: receipt_bump,
        };
        let mut receipt_data = deposit_receipt.try_borrow_mut_data()?;
        BorshSerialize::serialize(
            &receipt_state,
            &mut &mut receipt_data[..DepositReceipt::LEN],
        )
        .unwrap();
    } else {
        // Subsequent deposit — update existing receipt
        let receipt_state = DepositReceipt::try_from_slice(&deposit_receipt.try_borrow_data()?)
            .map_err(|_| {
                pinocchio_log::log!("ERROR: DepositFee failed: receipt deserialize failed");
                PortalError::DepositReceiptDeserializeFailed
            })?;

        if !receipt_state.is_valid() {
            pinocchio_log::log!("ERROR: DepositFee failed: receipt state invalid");
            return Err(PortalError::DepositReceiptStateInvalid.into());
        }

        if receipt_state.session != *session_key || receipt_state.recipient != *recipient_key {
            pinocchio_log::log!("ERROR: DepositFee failed: receipt state seeds mismatch");
            return Err(PortalError::InvalidPdaSeeds.into());
        }
    }

    if let Some(withdrawal_sink) = withdrawal_sink {
        let (expected_sink_key, sink_bump) =
            find_withdrawal_sink_pda(program_id, session_key, recipient_key);
        if withdrawal_sink.key() != &expected_sink_key {
            pinocchio_log::log!("ERROR: DepositFee failed: withdrawal sink PDA mismatch");
            return Err(PortalError::InvalidPdaSeeds.into());
        }
        if withdrawal_sink.lamports() == 0 {
            let rent = Rent::get()?;
            let sink_lamports = rent.minimum_balance(0);
            let sink_bump_bytes = [sink_bump];
            let sink_seeds = &[
                Seed::from(WithdrawalSink::SEED_PREFIX),
                Seed::from(session_key.as_ref()),
                Seed::from(recipient_key.as_ref()),
                Seed::from(sink_bump_bytes.as_ref()),
            ];
            let sink_signer = Signer::from(sink_seeds);
            CreateAccount {
                from: depositor,
                to: withdrawal_sink,
                lamports: sink_lamports,
                space: 0,
                owner: &pinocchio_system::ID,
            }
            .invoke_signed(&[sink_signer])?;
        }
    }

    // Transfer lamports from depositor to the deposit_receipt PDA
    Transfer {
        from: depositor,
        to: deposit_receipt,
        lamports,
    }
    .invoke()?;

    pinocchio_log::log!("DepositFee success");

    Ok(())
}
