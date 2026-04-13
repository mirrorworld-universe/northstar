use {
    crate::{DelegationRecord, DepositReceipt, FeeVault, Session},
    pinocchio::Address,
};

pub fn find_session_pda(program_id: &Address, owner: &Address, grid_id: u64) -> (Address, u8) {
    let grid_id_bytes = grid_id.to_le_bytes();
    let seeds = &[Session::SEED_PREFIX, owner.as_ref(), &grid_id_bytes];
    Address::find_program_address(seeds, program_id)
}

pub fn find_fee_vault_pda(program_id: &Address, owner: &Address) -> (Address, u8) {
    let seeds = &[FeeVault::SEED_PREFIX, owner.as_ref()];
    Address::find_program_address(seeds, program_id)
}

pub fn find_delegation_record_pda(
    program_id: &Address,
    delegated_account: &Address,
) -> (Address, u8) {
    let seeds = &[DelegationRecord::SEED_PREFIX, delegated_account.as_ref()];
    Address::find_program_address(seeds, program_id)
}

pub fn find_deposit_receipt_pda(
    program_id: &Address,
    session: &Address,
    recipient: &Address,
) -> (Address, u8) {
    let seeds = &[
        DepositReceipt::SEED_PREFIX,
        session.as_ref(),
        recipient.as_ref(),
    ];
    Address::find_program_address(seeds, program_id)
}
