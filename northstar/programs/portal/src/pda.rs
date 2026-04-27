use {
    crate::{DelegationRecord, DepositReceipt, FeeVault, Session},
    pinocchio::pubkey::{Pubkey, find_program_address},
};

/// Seed prefix for the per-delegation buffer PDA used to preserve a stateful
/// account's data across the owner reassignment required to delegate it.
///
/// The buffer is owned by the `owner_program` of the account being delegated
/// (e.g. `mach-amm` for an AMM `Pool`). It is derived under that program's ID,
/// not Portal's, so the calling program can sign for buffer creation/teardown
/// via `invoke_signed`.
pub const DELEGATE_BUFFER_SEED_PREFIX: &[u8] = b"portal_buffer";

pub fn find_session_pda(program_id: &Pubkey, owner: &Pubkey, grid_id: u64) -> (Pubkey, u8) {
    let grid_id_bytes = grid_id.to_le_bytes();
    let seeds = &[Session::SEED_PREFIX, owner.as_ref(), &grid_id_bytes];
    find_program_address(seeds, program_id)
}

pub fn find_fee_vault_pda(program_id: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[FeeVault::SEED_PREFIX, owner.as_ref()];
    find_program_address(seeds, program_id)
}

pub fn find_delegation_record_pda(program_id: &Pubkey, delegated_account: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[DelegationRecord::SEED_PREFIX, delegated_account.as_ref()];
    find_program_address(seeds, program_id)
}

pub fn find_deposit_receipt_pda(
    program_id: &Pubkey,
    session: &Pubkey,
    recipient: &Pubkey,
) -> (Pubkey, u8) {
    let seeds = &[
        DepositReceipt::SEED_PREFIX,
        session.as_ref(),
        recipient.as_ref(),
    ];
    find_program_address(seeds, program_id)
}

/// Derive the buffer PDA for a given `delegated_account` under the `owner_program`'s ID.
/// Used during the buffer-dance flow when delegating a stateful program-owned account.
pub fn find_delegate_buffer_pda(
    owner_program: &Pubkey,
    delegated_account: &Pubkey,
) -> (Pubkey, u8) {
    let seeds = &[DELEGATE_BUFFER_SEED_PREFIX, delegated_account.as_ref()];
    find_program_address(seeds, owner_program)
}
