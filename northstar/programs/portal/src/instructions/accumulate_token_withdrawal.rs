use {
    super::initialize_pda_account,
    crate::{
        account_size, find_checkpoint_pda, find_session_bridge_pda, find_session_pda,
        find_token_withdrawal_authorization_pda,
        instructions::settlement::{accumulate_token_withdrawal_checksum, TokenWithdrawalChecksum},
        AccumulateTokenWithdrawal, Checkpoint, CheckpointStatus, ConsumeTokenWithdrawal,
        PortalError, Session, SessionBridge, SettlementStatus, TokenWithdrawalAuthorization,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        cpi::{Seed, Signer},
        error::ProgramError,
        sysvars::{rent::Rent, Sysvar},
        AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
    solana_sha256_hasher::hashv,
};

#[p_instruction(
    id = 26,
    accounts = [
        validator(signer, mut),
        session(mut, state = Session),
        session_bridge(state = SessionBridge),
        vault,
        er_token_account,
        vault_token_account,
        destination_token_account,
        mint,
        token_program,
        authorization(mut, state = TokenWithdrawalAuthorization),
        system_program
    ],
    data = [er_slot: u64, checksum: Hash32, amount: u64, withdrawn: u64, decimals: u8]
)]
pub fn process_accumulate_token_withdrawal(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    settlement: AccumulateTokenWithdrawal,
) -> ProgramResult {
    let [validator, session, session_bridge, vault, er_token_account, vault_token_account, destination_token_account, mint, token_program, authorization, _system_program, ..] =
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

    let bridge = load_bridge(
        program_id,
        session,
        session_bridge,
        vault,
        mint,
        token_program,
    )?;
    let checkpoint = find_checkpoint_pda(program_id, session.address(), settlement.er_slot).0;
    let (expected_authorization, authorization_bump) = find_token_withdrawal_authorization_pda(
        program_id,
        &checkpoint,
        vault.address(),
        settlement.withdrawn,
    );
    if authorization.address() != &expected_authorization {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let withdrawal = TokenWithdrawalChecksum {
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
    };
    let authorization_state = TokenWithdrawalAuthorization {
        discriminator: TokenWithdrawalAuthorization::DISCRIMINATOR,
        checkpoint,
        tuple_hash: token_withdrawal_tuple_hash(&withdrawal),
        consumed: false,
        bump: authorization_bump,
    };

    if authorization.is_data_empty() {
        let authorization_size = account_size(&authorization_state);
        let authorization_lamports = Rent::get()?.try_minimum_balance(authorization_size)?;
        let withdrawn_bytes = settlement.withdrawn.to_le_bytes();
        let bump_bytes = [authorization_bump];
        let authorization_seeds = [
            Seed::from(TokenWithdrawalAuthorization::SEED_PREFIX),
            Seed::from(checkpoint.as_ref()),
            Seed::from(vault.address().as_ref()),
            Seed::from(withdrawn_bytes.as_ref()),
            Seed::from(bump_bytes.as_ref()),
        ];
        initialize_pda_account(
            validator,
            authorization,
            authorization_lamports,
            authorization_size as u64,
            program_id,
            Signer::from(&authorization_seeds),
        )?;
        let mut authorization_data = authorization.try_borrow_mut()?;
        BorshSerialize::serialize(&authorization_state, &mut &mut authorization_data[..]).unwrap();
    } else {
        if !authorization.owned_by(program_id) {
            return Err(PortalError::InvalidAccountData.into());
        }
        let existing = TokenWithdrawalAuthorization::try_from_slice(&authorization.try_borrow()?)
            .map_err(|_| PortalError::InvalidAccountData)?;
        if existing != authorization_state {
            return Err(PortalError::InvalidAccountData.into());
        }
        return Ok(());
    }

    session_state.settlement_accumulator =
        accumulate_token_withdrawal_checksum(session_state.settlement_accumulator, &withdrawal);
    let mut session_data = session.try_borrow_mut()?;
    BorshSerialize::serialize(&session_state, &mut &mut session_data[..]).unwrap();
    Ok(())
}

#[p_instruction(
    id = 27,
    accounts = [
        validator(signer),
        session(state = Session),
        checkpoint(state = Checkpoint),
        authorization(mut, state = TokenWithdrawalAuthorization),
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
pub fn process_consume_token_withdrawal(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    settlement: ConsumeTokenWithdrawal,
) -> ProgramResult {
    let [validator, session, checkpoint, authorization, session_bridge, vault, er_token_account, vault_token_account, destination_token_account, mint, token_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if session.address() != &find_session_pda(program_id).0 || !session.owned_by(program_id) {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let session_state = Session::try_from_slice(&session.try_borrow()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !session_state.is_valid()
        || !validator.is_signer()
        || validator.address() != &session_state.validator
    {
        return Err(PortalError::Unauthorized.into());
    }

    let expected_checkpoint =
        find_checkpoint_pda(program_id, session.address(), settlement.er_slot).0;
    if checkpoint.address() != &expected_checkpoint || !checkpoint.owned_by(program_id) {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let checkpoint_state = Checkpoint::try_from_slice(&checkpoint.try_borrow()?)
        .map_err(|_| PortalError::InvalidAccountData)?;
    if !checkpoint_state.is_valid()
        || checkpoint_state.session != *session.address()
        || checkpoint_state.er_slot != settlement.er_slot
        || checkpoint_state.effect_commitment != settlement.checksum
        || checkpoint_state.status != CheckpointStatus::Settled
    {
        return Err(PortalError::InvalidAccountData.into());
    }

    let bridge = load_bridge(
        program_id,
        session,
        session_bridge,
        vault,
        mint,
        token_program,
    )?;
    if !vault.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }
    let (expected_authorization, authorization_bump) = find_token_withdrawal_authorization_pda(
        program_id,
        checkpoint.address(),
        vault.address(),
        settlement.withdrawn,
    );
    if authorization.address() != &expected_authorization || !authorization.owned_by(program_id) {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let withdrawal = TokenWithdrawalChecksum {
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
    };
    let mut authorization_state =
        TokenWithdrawalAuthorization::try_from_slice(&authorization.try_borrow()?)
            .map_err(|_| PortalError::InvalidAccountData)?;
    if !authorization_state.is_valid()
        || authorization_state.checkpoint != *checkpoint.address()
        || authorization_state.tuple_hash != token_withdrawal_tuple_hash(&withdrawal)
        || authorization_state.bump != authorization_bump
        || authorization_state.consumed
    {
        return Err(PortalError::InvalidAccountData.into());
    }

    authorization_state.consumed = true;
    let mut authorization_data = authorization.try_borrow_mut()?;
    BorshSerialize::serialize(&authorization_state, &mut &mut authorization_data[..]).unwrap();
    Ok(())
}

fn load_bridge(
    program_id: &Pubkey,
    session: &AccountInfo,
    session_bridge: &AccountInfo,
    vault: &AccountInfo,
    mint: &AccountInfo,
    token_program: &AccountInfo,
) -> Result<SessionBridge, ProgramError> {
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
    {
        return Err(PortalError::InvalidAccountData.into());
    }
    Ok(bridge)
}

fn token_withdrawal_tuple_hash(withdrawal: &TokenWithdrawalChecksum<'_>) -> [u8; 32] {
    hashv(&[
        b"northstar-token-withdrawal-authorization-v0",
        withdrawal.bridge_program.as_ref(),
        withdrawal.session_bridge.as_ref(),
        withdrawal.er_token_account.as_ref(),
        withdrawal.vault.as_ref(),
        withdrawal.vault_token_account.as_ref(),
        withdrawal.destination_token_account.as_ref(),
        withdrawal.mint.as_ref(),
        withdrawal.token_program.as_ref(),
        &withdrawal.amount.to_le_bytes(),
        &withdrawal.withdrawn.to_le_bytes(),
        &[withdrawal.decimals],
    ])
    .to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_hash_binds_destination_and_amount() {
        let bridge_program = Pubkey::new_from_array([1; 32]);
        let session_bridge = Pubkey::new_from_array([2; 32]);
        let er_token_account = Pubkey::new_from_array([3; 32]);
        let vault = Pubkey::new_from_array([4; 32]);
        let vault_token_account = Pubkey::new_from_array([5; 32]);
        let destination = Pubkey::new_from_array([6; 32]);
        let other_destination = Pubkey::new_from_array([7; 32]);
        let mint = Pubkey::new_from_array([8; 32]);
        let token_program = Pubkey::new_from_array([9; 32]);
        let withdrawal = |destination_token_account, amount| TokenWithdrawalChecksum {
            bridge_program: &bridge_program,
            session_bridge: &session_bridge,
            er_token_account: &er_token_account,
            vault: &vault,
            vault_token_account: &vault_token_account,
            destination_token_account,
            mint: &mint,
            token_program: &token_program,
            amount,
            withdrawn: 10,
            decimals: 6,
        };

        let expected = token_withdrawal_tuple_hash(&withdrawal(&destination, 10));
        assert_ne!(
            expected,
            token_withdrawal_tuple_hash(&withdrawal(&other_destination, 10))
        );
        assert_ne!(
            expected,
            token_withdrawal_tuple_hash(&withdrawal(&destination, 9))
        );
    }
}
