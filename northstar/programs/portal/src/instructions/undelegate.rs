use {
    crate::{error::PortalError, pda::find_delegation_record_pda, state::DelegationRecord},
    borsh::BorshDeserialize,
    pinocchio::{
        ProgramResult, account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey,
    },
};

/// Undelegate an account, returning ownership to the program that originally owned it.
///
/// Two flows are supported, mirroring `process_delegate`:
///
/// 1. **Keypair-wallet undelegation**: `delegated_account` had no data during the ER
///    session (or only lamports moved). The data is zero, the reassign is a no-op
///    on data, and ownership returns to the recorded `owner_program`. After this,
///    the account is back under `owner_program` (typically `system_program`) and
///    behaves like a regular wallet again.
///
/// 2. **PDA-with-data undelegation**: `delegated_account` has program state from the
///    ER session that needs to come back to L1. Portal zero-fills the data
///    *before* reassigning ownership (Solana's runtime allows reassigning an
///    owned account when its data bytes are all zero — same constraint exercised
///    on the delegate side). Ownership returns to `owner_program`. The owner
///    program is responsible for re-installing the post-ER state into the now-empty
///    account in a *follow-up* instruction (e.g., `mach-amm`'s `restore_pool_state`
///    reads from a caller-staged buffer). This split keeps Portal's logic small
///    and gives the owner program full control over how state is migrated.
///
/// Accounts:
/// 0. `[signer, writable]` authority (receives the lamports refund from the
///    delegation_record)
/// 1. `[writable]` delegated_account (Portal-owned at start, owner_program-owned at end)
/// 2. `[]` owner_program (must equal the value stored in `delegation_record.owner_program`)
/// 3. `[writable]` delegation_record PDA (closed here)
/// 4. `[]` system_program
pub fn process_undelegate(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    pinocchio_log::log!("Instruction: Undelegate");

    if accounts.len() < 5 {
        pinocchio_log::log!("ERROR: Undelegate failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority = &accounts[0];
    let delegated_account = &accounts[1];
    let owner_program = &accounts[2];
    let delegation_record = &accounts[3];
    let _system_program = &accounts[4];

    if !authority.is_signer() {
        pinocchio_log::log!("ERROR: Undelegate failed: authority is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    let delegated_key = *delegated_account.key();
    let (expected_delegation_key, _) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.key() != &expected_delegation_key {
        pinocchio_log::log!("ERROR: Undelegate failed: delegation record PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let delegation_state = DelegationRecord::try_from_slice(&delegation_record.try_borrow_data()?)
        .map_err(|_| {
            pinocchio_log::log!("ERROR: Undelegate failed: delegation record deserialize failed");
            PortalError::DelegationRecordDeserializeFailed
        })?;

    if !delegation_state.is_valid() {
        pinocchio_log::log!("ERROR: Undelegate failed: delegation record state invalid");
        return Err(PortalError::DelegationRecordStateInvalid.into());
    }

    if delegation_state.owner_program != *owner_program.key() {
        pinocchio_log::log!("ERROR: Undelegate failed: owner program mismatch");
        return Err(PortalError::Unauthorized.into());
    }

    if delegated_account.owner() != program_id {
        pinocchio_log::log!("ERROR: Undelegate failed: delegated account owner mismatch");
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    // Zero-fill delegated_account data so the owner reassign is permitted by the
    // Solana runtime (it requires the data bytes to all be zero before the reassign,
    // independent of data length). For keypair-wallet flow this is already a no-op
    // since data is already empty. For PDA-with-data flow, this clears the ER-session
    // state — the owner program's follow-up instruction is responsible for restoring
    // post-ER state from its own state buffer.
    {
        let mut delegated_data = delegated_account.try_borrow_mut_data()?;
        delegated_data.fill(0);
    }

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
