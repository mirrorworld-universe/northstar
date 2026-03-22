use {
    crate::{DelegationRecord, DepositReceipt, FeeVault, Session},
    pinocchio::pubkey::{find_program_address, Pubkey},
};

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
