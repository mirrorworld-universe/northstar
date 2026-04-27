use {
    crate::{error::PortalError, pda::find_delegation_record_pda, state::DelegationRecord},
    borsh::BorshDeserialize,
    pinocchio::{
        ProgramResult, account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey,
    },
};

/// Undelegate an account, returning ownership to `owner_program`.
///
/// Portal zero-fills `delegated_account.data` before reassigning ownership — Solana's
/// runtime allows owner reassign only when the existing data bytes are all zero. For
/// the keypair-wallet flow this is a no-op (data already empty). For PDA flow, the
/// owner program is responsible for re-installing post-ER state in a follow-up ix.
///
/// Accounts:
/// 0. `[signer, writable]` authority (receives the delegation_record's lamport refund)
/// 1. `[writable]` delegated_account
/// 2. `[]` owner_program (must equal `delegation_record.owner_program`)
/// 3. `[writable]` delegation_record PDA (closed)
/// 4. `[]` system_program
pub fn process_undelegate(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    pinocchio_log::log!("Instruction: Undelegate");

    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority = &accounts[0];
    let delegated_account = &accounts[1];
    let owner_program = &accounts[2];
    let delegation_record = &accounts[3];
    let _system_program = &accounts[4];

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let delegated_key = *delegated_account.key();
    let (expected_delegation_key, _) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.key() != &expected_delegation_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let delegation_state = DelegationRecord::try_from_slice(&delegation_record.try_borrow_data()?)
        .map_err(|_| PortalError::DelegationRecordDeserializeFailed)?;

    if !delegation_state.is_valid() {
        return Err(PortalError::DelegationRecordStateInvalid.into());
    }

    if delegation_state.owner_program != *owner_program.key() {
        return Err(PortalError::Unauthorized.into());
    }

    if delegated_account.owner() != program_id {
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    delegated_account.try_borrow_mut_data()?.fill(0);

    unsafe { delegated_account.assign(owner_program.key()) };

    let delegation_record_lamports = delegation_record.lamports();

    if delegation_record_lamports > 0 {
        let mut authority_lamports = authority.try_borrow_mut_lamports()?;
        *authority_lamports = authority_lamports
            .checked_add(delegation_record_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
        *delegation_record.try_borrow_mut_lamports()? = 0;
    }

    delegation_record.try_borrow_mut_data()?.fill(0);

    pinocchio_log::log!("Undelegate success");

    Ok(())
}
