use {
    crate::{
        Challenge, Checkpoint, CheckpointCursor, DataAvailabilityProof, DelegationRecord,
        DepositReceipt, FeeVault, Session, SessionBridge, StepProofAccount,
        TokenWithdrawalAuthorization,
    },
    pinocchio::Address as Pubkey,
};

#[cfg(target_os = "solana")]
fn find_program_address(seeds: &[&[u8]], program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(seeds, program_id)
}

#[cfg(not(target_os = "solana"))]
fn find_program_address(seeds: &[&[u8]], program_id: &Pubkey) -> (Pubkey, u8) {
    let program_id = solana_pubkey::Pubkey::new_from_array(program_id.to_bytes());
    let (pda, bump) = solana_pubkey::Pubkey::find_program_address(seeds, &program_id);
    (Pubkey::from(pda.to_bytes()), bump)
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

pub fn find_challenge_pda(program_id: &Pubkey, checkpoint: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[Challenge::SEED_PREFIX, checkpoint.as_ref()];
    find_program_address(seeds, program_id)
}

pub fn find_da_proof_pda(program_id: &Pubkey, challenge: &Pubkey) -> (Pubkey, u8) {
    let seeds = &[DataAvailabilityProof::SEED_PREFIX, challenge.as_ref()];
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

pub fn find_session_bridge_pda(
    program_id: &Pubkey,
    session: &Pubkey,
    mint: &Pubkey,
) -> (Pubkey, u8) {
    let seeds = &[SessionBridge::SEED_PREFIX, session.as_ref(), mint.as_ref()];
    find_program_address(seeds, program_id)
}

pub fn find_token_withdrawal_authorization_pda(
    program_id: &Pubkey,
    checkpoint: &Pubkey,
    vault: &Pubkey,
    withdrawn: u64,
) -> (Pubkey, u8) {
    let withdrawn_bytes = withdrawn.to_le_bytes();
    let seeds = &[
        TokenWithdrawalAuthorization::SEED_PREFIX,
        checkpoint.as_ref(),
        vault.as_ref(),
        withdrawn_bytes.as_ref(),
    ];
    find_program_address(seeds, program_id)
}
