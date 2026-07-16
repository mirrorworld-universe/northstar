use {
    crate::{
        Checkpoint, CheckpointCursor, DelegationRecord, DepositReceipt, FeeVault, Session,
        StepProofAccount,
    },
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

pub fn find_checkpoint_pda(program_id: &Pubkey, session: &Pubkey, er_slot: u64) -> (Pubkey, u8) {
    let er_slot_bytes = er_slot.to_le_bytes();
    let seeds = &[
        Checkpoint::SEED_PREFIX,
        session.as_ref(),
        er_slot_bytes.as_ref(),
    ];
    find_program_address(seeds, program_id)
}

pub fn find_checkpoint_cursor_pda(program_id: &Pubkey, session: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[CheckpointCursor::SEED_PREFIX, session.as_ref()];
    find_program_address(seeds, program_id)
}

pub fn find_step_proof_pda(program_id: &Pubkey, checkpoint: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[StepProofAccount::SEED_PREFIX, checkpoint.as_ref()];
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
