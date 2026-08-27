use {
    crate::{
        find_session_bridge_pda, find_session_pda,
        instructions::settlement::{accumulate_token_withdrawal_checksum, TokenWithdrawalChecksum},
        AccumulateTokenWithdrawal, PortalError, Session, SessionBridge, SettlementStatus,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        error::ProgramError, AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
};

#[p_instruction(
    id = 26,
    accounts = [
        validator(signer),
        session(mut, state = Session),
        session_bridge(state = SessionBridge),
        vault(signer),
        er_token_account,
        vault_token_account,
        destination_token_account,
        mint,
        token_program
    ],
    data = [er_slot: u64, checksum: Hash32, amount: u64, withdrawn: u64, decimals: u8]
)]
pub fn process_accumulate_token_withdrawal(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    settlement: AccumulateTokenWithdrawal,
) -> ProgramResult {
    let [validator, session, session_bridge, vault, er_token_account, vault_token_account, destination_token_account, mint, token_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if session.address() != &find_session_pda(program_id).0 {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let mut session_state = Session::try_from_slice(&session.try_borrow()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !session_state.is_valid() || !session.owned_by(program_id) {
        return Err(PortalError::SessionStateInvalid.into());
    }
    if !validator.is_signer() || validator.address() != &session_state.validator {
        return Err(PortalError::Unauthorized.into());
    }
    if session_state.settlement_status != SettlementStatus::InProgress {
        return Err(PortalError::SettlementNotInProgress.into());
    }
    if settlement.er_slot != session_state.settlement_er_slot {
        return Err(PortalError::SettlementErSlotMismatch.into());
    }
    if settlement.checksum != session_state.settlement_checksum {
        return Err(PortalError::SettlementChecksumMismatch.into());
    }
    if !session_bridge.owned_by(program_id) {
        return Err(PortalError::InvalidAccountData.into());
    }
    let bridge = SessionBridge::try_from_slice(&session_bridge.try_borrow()?)
        .map_err(|_| PortalError::InvalidAccountData)?;
    let expected_bridge = find_session_bridge_pda(program_id, session.address(), &bridge.mint).0;
    if !bridge.is_valid()
        || bridge.session != *session.address()
        || session_bridge.address() != &expected_bridge
        || bridge.bridge_program != *vault.owner()
        || bridge.vault != *vault.address()
        || bridge.mint != *mint.address()
        || bridge.token_program != *token_program.address()
        || !vault.is_signer()
    {
        return Err(PortalError::InvalidAccountData.into());
    }

    session_state.settlement_accumulator = accumulate_token_withdrawal_checksum(
        session_state.settlement_accumulator,
        &TokenWithdrawalChecksum {
            bridge_program: &bridge.bridge_program,
            session_bridge: session_bridge.address(),
            er_token_account: er_token_account.address(),
            vault: vault.address(),
            vault_token_account: vault_token_account.address(),
            destination_token_account: destination_token_account.address(),
            mint: mint.address(),
            token_program: token_program.address(),
            amount: settlement.amount,
            withdrawn: settlement.withdrawn,
            decimals: settlement.decimals,
        },
    );
    let mut session_data = session.try_borrow_mut()?;
    BorshSerialize::serialize(&session_state, &mut &mut session_data[..]).unwrap();
    Ok(())
}
