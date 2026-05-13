use {
    crate::{DelegationRecord, DepositReceipt, FeeVault, Session},
    pinocchio::pubkey::Pubkey,
};

#[cfg(target_os = "solana")]
fn find_program_address(seeds: &[&[u8]], program_id: &Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(seeds, program_id)
}

#[cfg(not(target_os = "solana"))]
fn find_program_address(seeds: &[&[u8]], program_id: &Pubkey) -> (Pubkey, u8) {
    let program_id = solana_pubkey::Pubkey::new_from_array(*program_id);
    let (pda, bump) = solana_pubkey::Pubkey::find_program_address(seeds, &program_id);
    (pda.to_bytes(), bump)
}

pub fn find_session_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[Session::SEED_PREFIX];
    find_program_address(seeds, program_id)
}

pub fn find_fee_vault_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[FeeVault::SEED_PREFIX];
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
