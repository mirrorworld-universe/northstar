use {
    super::initialize_pda_account,
    crate::{
        find_checkpoint_cursor_pda, find_checkpoint_pda, find_session_pda, find_step_proof_pda,
        CancelCheckpoint, ChallengeCheckpoint, Checkpoint, CheckpointBondStatus, CheckpointCursor,
        CheckpointStatus, CommitCheckpoint, CreateStepProof, PortalError, ProposeCheckpoint,
        SealStepProof, Session, StepProofAccount, StepProofVerifierMode, SubmitStepProof,
        WriteStepProof, CHECKPOINT_PROPOSER_BOND_LAMPORTS, MAX_STEP_PROOF_BYTES,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        account_info::AccountInfo,
        instruction::{Seed, Signer},
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        ProgramResult,
    },
    pinocchio_system::instructions::Transfer,
    solana_sha256_hasher::hashv,
};

fn load_session(program_id: &Pubkey, session: &AccountInfo) -> Result<Session, ProgramError> {
    let (expected_session_key, _) = find_session_pda(program_id);
    if session.key() != &expected_session_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if session.owner() != program_id {
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }
    let session_state = Session::try_from_slice(&session.try_borrow_data()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !session_state.is_valid() {
        return Err(PortalError::SessionStateInvalid.into());
    }
    Ok(session_state)
}

fn store_checkpoint(checkpoint: &AccountInfo, checkpoint_state: &Checkpoint) -> ProgramResult {
    let mut checkpoint_data = checkpoint.try_borrow_mut_data()?;
    BorshSerialize::serialize(
        checkpoint_state,
        &mut &mut checkpoint_data[..Checkpoint::LEN],
    )
    .unwrap();
    Ok(())
}

fn store_cursor(cursor: &AccountInfo, cursor_state: &CheckpointCursor) -> ProgramResult {
    let mut cursor_data = cursor.try_borrow_mut_data()?;
    BorshSerialize::serialize(cursor_state, &mut &mut cursor_data[..CheckpointCursor::LEN])
        .unwrap();
    Ok(())
}

fn store_step_proof(proof: &AccountInfo, proof_state: &StepProofAccount) -> ProgramResult {
    let mut proof_data = proof.try_borrow_mut_data()?;
    BorshSerialize::serialize(proof_state, &mut &mut proof_data[..StepProofAccount::LEN]).unwrap();
    Ok(())
}

fn load_step_proof(
    program_id: &Pubkey,
    checkpoint_key: &Pubkey,
    proof: &AccountInfo,
) -> Result<StepProofAccount, ProgramError> {
    let (expected_proof_key, _) = find_step_proof_pda(program_id, checkpoint_key);
    if proof.key() != &expected_proof_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if proof.owner() != program_id {
        return Err(PortalError::StepProofStateInvalid.into());
    }
    let proof_state = StepProofAccount::try_from_slice(&proof.try_borrow_data()?)
        .map_err(|_| PortalError::StepProofDeserializeFailed)?;
    if !proof_state.is_valid() || proof_state.checkpoint != *checkpoint_key {
        return Err(PortalError::StepProofStateInvalid.into());
    }
    Ok(proof_state)
}

fn resolve_checkpoint_bond(
    checkpoint: &AccountInfo,
    recipient: &AccountInfo,
    checkpoint_state: &mut Checkpoint,
    next_status: CheckpointBondStatus,
) -> ProgramResult {
    if checkpoint_state.bond_status != CheckpointBondStatus::Locked {
        return Err(PortalError::CheckpointBondAlreadyResolved.into());
    }

    let bond_lamports = checkpoint_state.bond_lamports;
    if bond_lamports == 0 {
        return Err(PortalError::CheckpointBondInsufficient.into());
    }

    let rent_lamports = Rent::get()?.minimum_balance(Checkpoint::LEN);
    let minimum_locked_lamports = rent_lamports
        .checked_add(bond_lamports)
        .ok_or(PortalError::ArithmeticOverflow)?;
    if checkpoint.lamports() < minimum_locked_lamports {
        return Err(PortalError::CheckpointBondInsufficient.into());
    }

    {
        let mut recipient_lamports = recipient.try_borrow_mut_lamports()?;
        *recipient_lamports = recipient_lamports
            .checked_add(bond_lamports)
            .ok_or(PortalError::ArithmeticOverflow)?;
    }
    *checkpoint.try_borrow_mut_lamports()? = checkpoint
        .lamports()
        .checked_sub(bond_lamports)
        .ok_or(PortalError::ArithmeticOverflow)?;
    checkpoint_state.bond_status = next_status;

    Ok(())
}

fn release_checkpoint_bond(
    checkpoint: &AccountInfo,
    proposer: &AccountInfo,
    checkpoint_state: &mut Checkpoint,
) -> ProgramResult {
    if proposer.key() != &checkpoint_state.proposer {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    resolve_checkpoint_bond(
        checkpoint,
        proposer,
        checkpoint_state,
        CheckpointBondStatus::Released,
    )
}

#[allow(dead_code)]
pub(crate) fn slash_checkpoint_bond(
    checkpoint: &AccountInfo,
    recipient: &AccountInfo,
    checkpoint_state: &mut Checkpoint,
) -> ProgramResult {
    resolve_checkpoint_bond(
        checkpoint,
        recipient,
        checkpoint_state,
        CheckpointBondStatus::Slashed,
    )
}

fn load_checkpoint(
    program_id: &Pubkey,
    session_key: &Pubkey,
    er_slot: u64,
    checkpoint: &AccountInfo,
) -> Result<Checkpoint, ProgramError> {
    let (expected_checkpoint_key, _) = find_checkpoint_pda(program_id, session_key, er_slot);
    if checkpoint.key() != &expected_checkpoint_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if checkpoint.owner() != program_id {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let checkpoint_state = Checkpoint::try_from_slice(&checkpoint.try_borrow_data()?)
        .map_err(|_| PortalError::CheckpointDeserializeFailed)?;
    if !checkpoint_state.is_valid()
        || checkpoint_state.session != *session_key
        || checkpoint_state.er_slot != er_slot
    {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    Ok(checkpoint_state)
}

fn load_cursor(
    program_id: &Pubkey,
    session_key: &Pubkey,
    cursor: &AccountInfo,
) -> Result<CheckpointCursor, ProgramError> {
    let (expected_cursor_key, _) = find_checkpoint_cursor_pda(program_id, session_key);
    if cursor.key() != &expected_cursor_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if cursor.owner() != program_id {
        return Err(PortalError::CheckpointCursorStateInvalid.into());
    }
    let cursor_state = CheckpointCursor::try_from_slice(&cursor.try_borrow_data()?)
        .map_err(|_| PortalError::CheckpointCursorDeserializeFailed)?;
    if !cursor_state.is_valid() || cursor_state.session != *session_key {
        return Err(PortalError::CheckpointCursorStateInvalid.into());
    }
    Ok(cursor_state)
}

fn create_cursor(
    program_id: &Pubkey,
    payer: &AccountInfo,
    session_key: &Pubkey,
    cursor: &AccountInfo,
) -> Result<CheckpointCursor, ProgramError> {
    let (expected_cursor_key, cursor_bump) = find_checkpoint_cursor_pda(program_id, session_key);
    if cursor.key() != &expected_cursor_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let rent = Rent::get()?;
    let cursor_lamports = rent.minimum_balance(CheckpointCursor::LEN);
    let cursor_bump_bytes = [cursor_bump];
    let cursor_seeds = &[
        Seed::from(CheckpointCursor::SEED_PREFIX),
        Seed::from(session_key.as_ref()),
        Seed::from(cursor_bump_bytes.as_ref()),
    ];
    let cursor_signer = Signer::from(cursor_seeds);

    initialize_pda_account(
        payer,
        cursor,
        cursor_lamports,
        CheckpointCursor::LEN as u64,
        program_id,
        cursor_signer,
    )?;

    let cursor_state = CheckpointCursor {
        discriminator: CheckpointCursor::DISCRIMINATOR,
        session: *session_key,
        latest_finalized_checkpoint: [0; 32],
        latest_finalized_er_slot: 0,
        latest_finalized_state_root: [0; 32],
        active_checkpoint: [0; 32],
        active_er_slot: 0,
        bump: cursor_bump,
    };
    store_cursor(cursor, &cursor_state)?;
    Ok(cursor_state)
}

fn load_or_create_cursor(
    program_id: &Pubkey,
    payer: &AccountInfo,
    session_key: &Pubkey,
    cursor: &AccountInfo,
) -> Result<CheckpointCursor, ProgramError> {
    if cursor.data_is_empty() {
        create_cursor(program_id, payer, session_key, cursor)
    } else {
        load_cursor(program_id, session_key, cursor)
    }
}

fn require_advancing_checkpoint(
    session_state: &Session,
    cursor_state: &CheckpointCursor,
    er_slot: u64,
) -> ProgramResult {
    if er_slot <= session_state.last_settled_er_slot
        || er_slot <= cursor_state.latest_finalized_er_slot
    {
        return Err(PortalError::CheckpointErSlotNotAdvanced.into());
    }
    Ok(())
}

fn require_no_active_checkpoint(cursor_state: &CheckpointCursor) -> ProgramResult {
    if cursor_state.active_checkpoint != [0; 32] {
        return Err(PortalError::CheckpointActiveExists.into());
    }
    Ok(())
}

fn require_active_checkpoint(
    cursor_state: &CheckpointCursor,
    checkpoint_key: &Pubkey,
    er_slot: u64,
) -> ProgramResult {
    if cursor_state.active_checkpoint != *checkpoint_key || cursor_state.active_er_slot != er_slot {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    Ok(())
}

fn clear_active_checkpoint(cursor_state: &mut CheckpointCursor) {
    cursor_state.active_checkpoint = [0; 32];
    cursor_state.active_er_slot = 0;
}

fn challenge_resolution_deadline(checkpoint_state: &Checkpoint) -> Result<u64, ProgramError> {
    let challenge_window_slots = checkpoint_state
        .challenge_deadline_l1_slot
        .checked_sub(checkpoint_state.proposed_at_l1_slot)
        .ok_or(PortalError::ArithmeticOverflow)?;
    checkpoint_state
        .challenged_at_l1_slot
        .checked_add(challenge_window_slots)
        .ok_or(PortalError::ArithmeticOverflow.into())
}

fn step_proof_public_input_hash(
    session_key: &Pubkey,
    checkpoint_key: &Pubkey,
    checkpoint_state: &Checkpoint,
    proof_kind: u8,
    proof_version: u8,
    step_index: u64,
) -> [u8; 32] {
    hashv(&[
        b"northstar-step-proof-input-v1",
        session_key,
        checkpoint_key,
        &[proof_kind],
        &[proof_version],
        &checkpoint_state.er_slot.to_le_bytes(),
        &step_index.to_le_bytes(),
        &checkpoint_state.previous_state_root,
        &checkpoint_state.new_state_root,
        &checkpoint_state.effect_commitment,
    ])
    .to_bytes()
}

pub fn process_propose_checkpoint(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    ProposeCheckpoint {
        er_slot,
        previous_state_root,
        new_state_root,
        effect_commitment,
        challenge_window_slots,
    }: ProposeCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: ProposeCheckpoint, er_slot={}", er_slot);

    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    if challenge_window_slots == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let proposer = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];
    let cursor = &accounts[3];
    let _system_program = &accounts[4];

    if !proposer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let session_state = load_session(program_id, session)?;
    if proposer.key() != &session_state.validator {
        return Err(PortalError::Unauthorized.into());
    }

    let current_slot = Clock::get()?.slot;
    if session_state.is_expired(current_slot) {
        return Err(PortalError::SessionExpired.into());
    }

    let session_key = session.key();
    let mut cursor_state = load_or_create_cursor(program_id, proposer, session_key, cursor)?;
    require_no_active_checkpoint(&cursor_state)?;
    require_advancing_checkpoint(&session_state, &cursor_state, er_slot)?;
    if previous_state_root != cursor_state.latest_finalized_state_root {
        return Err(PortalError::CheckpointPreviousRootMismatch.into());
    }

    let (expected_checkpoint_key, checkpoint_bump) =
        find_checkpoint_pda(program_id, session_key, er_slot);
    if checkpoint.key() != &expected_checkpoint_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let rent = Rent::get()?;
    let checkpoint_rent_lamports = rent.minimum_balance(Checkpoint::LEN);
    if checkpoint.data_is_empty() {
        let checkpoint_lamports = checkpoint_rent_lamports
            .checked_add(CHECKPOINT_PROPOSER_BOND_LAMPORTS)
            .ok_or(PortalError::ArithmeticOverflow)?;
        let er_slot_bytes = er_slot.to_le_bytes();
        let checkpoint_bump_bytes = [checkpoint_bump];
        let checkpoint_seeds = &[
            Seed::from(Checkpoint::SEED_PREFIX),
            Seed::from(session_key.as_ref()),
            Seed::from(er_slot_bytes.as_ref()),
            Seed::from(checkpoint_bump_bytes.as_ref()),
        ];
        let checkpoint_signer = Signer::from(checkpoint_seeds);

        initialize_pda_account(
            proposer,
            checkpoint,
            checkpoint_lamports,
            Checkpoint::LEN as u64,
            program_id,
            checkpoint_signer,
        )?;
    } else {
        let previous_checkpoint = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
        if !matches!(
            previous_checkpoint.status,
            CheckpointStatus::Settled | CheckpointStatus::Cancelled | CheckpointStatus::Invalid
        ) || cursor_state.latest_finalized_checkpoint != [0; 32]
            || cursor_state.latest_finalized_er_slot != 0
            || cursor_state.latest_finalized_state_root != [0; 32]
        {
            return Err(PortalError::CheckpointStateInvalid.into());
        }
        if checkpoint.lamports() < checkpoint_rent_lamports {
            return Err(PortalError::CheckpointBondInsufficient.into());
        }
        Transfer {
            from: proposer,
            to: checkpoint,
            lamports: CHECKPOINT_PROPOSER_BOND_LAMPORTS,
        }
        .invoke()?;
    }

    let checkpoint_state = Checkpoint {
        discriminator: Checkpoint::DISCRIMINATOR,
        session: *session_key,
        er_slot,
        previous_state_root,
        new_state_root,
        effect_commitment,
        proposer: *proposer.key(),
        proposed_at_l1_slot: current_slot,
        challenge_deadline_l1_slot: current_slot
            .checked_add(challenge_window_slots)
            .ok_or(PortalError::ArithmeticOverflow)?,
        status: CheckpointStatus::Pending,
        bond_lamports: CHECKPOINT_PROPOSER_BOND_LAMPORTS,
        bond_status: CheckpointBondStatus::Locked,
        challenger: [0; 32],
        challenged_at_l1_slot: 0,
        challenge_resolved: false,
        bump: checkpoint_bump,
    };
    store_checkpoint(checkpoint, &checkpoint_state)?;

    cursor_state.active_checkpoint = *checkpoint.key();
    cursor_state.active_er_slot = er_slot;
    store_cursor(cursor, &cursor_state)?;

    Ok(())
}

pub fn process_commit_checkpoint(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    CommitCheckpoint { er_slot }: CommitCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CommitCheckpoint, er_slot={}", er_slot);

    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let committer = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];
    let cursor = &accounts[3];
    let proposer = &accounts[4];

    if !committer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let session_state = load_session(program_id, session)?;
    let session_key = session.key();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    let mut cursor_state = load_cursor(program_id, session_key, cursor)?;

    let current_slot = Clock::get()?.slot;
    match checkpoint_state.status {
        CheckpointStatus::Pending => {}
        CheckpointStatus::Challenged
            if current_slot >= challenge_resolution_deadline(&checkpoint_state)? =>
        {
            checkpoint_state.challenger = [0; 32];
            checkpoint_state.challenged_at_l1_slot = 0;
            checkpoint_state.challenge_resolved = true;
        }
        CheckpointStatus::Challenged => return Err(PortalError::CheckpointChallenged.into()),
        _ => return Err(PortalError::CheckpointStateInvalid.into()),
    }

    if current_slot < checkpoint_state.challenge_deadline_l1_slot {
        return Err(PortalError::CheckpointCommitTooEarly.into());
    }

    require_advancing_checkpoint(&session_state, &cursor_state, er_slot)?;
    require_active_checkpoint(&cursor_state, checkpoint.key(), er_slot)?;
    if checkpoint_state.previous_state_root != cursor_state.latest_finalized_state_root {
        return Err(PortalError::CheckpointPreviousRootMismatch.into());
    }

    checkpoint_state.status = CheckpointStatus::Committed;
    release_checkpoint_bond(checkpoint, proposer, &mut checkpoint_state)?;
    store_checkpoint(checkpoint, &checkpoint_state)?;

    cursor_state.latest_finalized_checkpoint = *checkpoint.key();
    cursor_state.latest_finalized_er_slot = er_slot;
    cursor_state.latest_finalized_state_root = checkpoint_state.new_state_root;
    store_cursor(cursor, &cursor_state)?;

    Ok(())
}

pub fn process_cancel_checkpoint(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    CancelCheckpoint { er_slot }: CancelCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CancelCheckpoint, er_slot={}", er_slot);

    if accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let proposer = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];
    let cursor = &accounts[3];

    if !proposer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let session_key = session.key();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    let mut cursor_state = load_cursor(program_id, session_key, cursor)?;

    if checkpoint_state.status == CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointChallenged.into());
    }
    if checkpoint_state.status != CheckpointStatus::Pending {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    if proposer.key() != &checkpoint_state.proposer {
        return Err(PortalError::CheckpointUnauthorizedCancel.into());
    }
    if Clock::get()?.slot >= checkpoint_state.challenge_deadline_l1_slot {
        return Err(PortalError::CheckpointCancelWindowClosed.into());
    }
    require_active_checkpoint(&cursor_state, checkpoint.key(), er_slot)?;

    checkpoint_state.status = CheckpointStatus::Cancelled;
    release_checkpoint_bond(checkpoint, proposer, &mut checkpoint_state)?;
    store_checkpoint(checkpoint, &checkpoint_state)?;
    clear_active_checkpoint(&mut cursor_state);
    store_cursor(cursor, &cursor_state)?;

    Ok(())
}

pub fn process_challenge_checkpoint(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    ChallengeCheckpoint { er_slot }: ChallengeCheckpoint,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: ChallengeCheckpoint, er_slot={}", er_slot);

    if accounts.len() < 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let challenger = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];

    if !challenger.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let session_key = session.key();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;

    match checkpoint_state.status {
        CheckpointStatus::Pending => {}
        CheckpointStatus::Challenged => return Err(PortalError::CheckpointChallenged.into()),
        _ => return Err(PortalError::CheckpointStateInvalid.into()),
    }
    if checkpoint_state.challenge_resolved {
        return Err(PortalError::CheckpointStateInvalid.into());
    }

    let current_slot = Clock::get()?.slot;
    if current_slot >= checkpoint_state.challenge_deadline_l1_slot {
        return Err(PortalError::CheckpointChallengeWindowClosed.into());
    }

    checkpoint_state.status = CheckpointStatus::Challenged;
    checkpoint_state.challenger = *challenger.key();
    checkpoint_state.challenged_at_l1_slot = current_slot;
    store_checkpoint(checkpoint, &checkpoint_state)?;

    Ok(())
}

pub fn process_create_step_proof(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    CreateStepProof {
        er_slot,
        proof_kind,
        proof_version,
        step_index,
    }: CreateStepProof,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: CreateStepProof, er_slot={}", er_slot);

    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];
    let proof = &accounts[3];
    let _system_program = &accounts[4];

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let session_key = session.key();
    let checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    if proof_kind == 0 || proof_version == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if authority.key() != &checkpoint_state.challenger {
        return Err(PortalError::Unauthorized.into());
    }

    let (expected_proof_key, proof_bump) = find_step_proof_pda(program_id, checkpoint.key());
    if proof.key() != &expected_proof_key {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let rent = Rent::get()?;
    let proof_lamports = rent.minimum_balance(StepProofAccount::LEN);
    if proof.data_is_empty() {
        let proof_bump_bytes = [proof_bump];
        let proof_seeds = &[
            Seed::from(StepProofAccount::SEED_PREFIX),
            Seed::from(checkpoint.key().as_ref()),
            Seed::from(proof_bump_bytes.as_ref()),
        ];
        let proof_signer = Signer::from(proof_seeds);

        initialize_pda_account(
            authority,
            proof,
            proof_lamports,
            StepProofAccount::LEN as u64,
            program_id,
            proof_signer,
        )?;
    } else if proof.owner() != program_id || proof.lamports() < proof_lamports {
        return Err(PortalError::StepProofStateInvalid.into());
    }

    let proof_state = StepProofAccount {
        discriminator: StepProofAccount::DISCRIMINATOR,
        checkpoint: *checkpoint.key(),
        authority: *authority.key(),
        proof_kind,
        proof_version,
        step_index,
        public_input_hash: step_proof_public_input_hash(
            session_key,
            checkpoint.key(),
            &checkpoint_state,
            proof_kind,
            proof_version,
            step_index,
        ),
        written_len: 0,
        sealed: false,
        proof_hash: [0; 32],
        bump: proof_bump,
        data: [0; MAX_STEP_PROOF_BYTES],
    };
    store_step_proof(proof, &proof_state)?;

    Ok(())
}

pub fn process_write_step_proof(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    WriteStepProof {
        er_slot,
        offset,
        chunk_len,
        chunk,
    }: WriteStepProof,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: WriteStepProof, er_slot={}", er_slot);

    if accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];
    let proof = &accounts[3];

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let checkpoint_state = load_checkpoint(program_id, session.key(), er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }

    let mut proof_state = load_step_proof(program_id, checkpoint.key(), proof)?;
    if proof_state.authority != *authority.key() {
        return Err(PortalError::Unauthorized.into());
    }
    if proof_state.sealed {
        return Err(PortalError::StepProofAlreadySealed.into());
    }

    let chunk_len = chunk_len as usize;
    let offset = offset as usize;
    let end = offset
        .checked_add(chunk_len)
        .ok_or(PortalError::StepProofChunkOutOfBounds)?;
    if end > MAX_STEP_PROOF_BYTES || chunk_len > chunk.len() {
        return Err(PortalError::StepProofChunkOutOfBounds.into());
    }
    proof_state.data[offset..end].copy_from_slice(&chunk[..chunk_len]);
    proof_state.written_len = proof_state.written_len.max(end as u32);
    store_step_proof(proof, &proof_state)?;

    Ok(())
}

pub fn process_seal_step_proof(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    SealStepProof { er_slot, proof_len }: SealStepProof,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: SealStepProof, er_slot={}", er_slot);

    if accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];
    let proof = &accounts[3];

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let checkpoint_state = load_checkpoint(program_id, session.key(), er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }

    let mut proof_state = load_step_proof(program_id, checkpoint.key(), proof)?;
    if proof_state.authority != *authority.key() {
        return Err(PortalError::Unauthorized.into());
    }
    if proof_state.sealed {
        return Err(PortalError::StepProofAlreadySealed.into());
    }
    if proof_len == 0 || proof_len > proof_state.written_len {
        return Err(PortalError::StepProofChunkOutOfBounds.into());
    }

    proof_state.written_len = proof_len;
    proof_state.sealed = true;
    proof_state.proof_hash = hashv(&[&proof_state.data[..proof_len as usize]]).to_bytes();
    store_step_proof(proof, &proof_state)?;

    Ok(())
}

#[allow(dead_code)]
enum StepProofVerification {
    Valid,
    Invalid,
    Unavailable,
}

fn verify_step_proof(
    verifier_mode: StepProofVerifierMode,
    proof_state: &StepProofAccount,
) -> StepProofVerification {
    match verifier_mode {
        StepProofVerifierMode::Production => StepProofVerification::Unavailable,
        StepProofVerifierMode::TestOnly => verify_step_proof_test_only(proof_state),
    }
}

fn verify_step_proof_test_only(proof_state: &StepProofAccount) -> StepProofVerification {
    // Dummy verifier is for program-test only. Solana/SBF builds need the explicit
    // build-script escape hatch, otherwise `test-verifier` fails compilation.
    #[cfg(feature = "test-verifier")]
    {
        match proof_state.data.first().copied() {
            Some(2) => StepProofVerification::Valid,
            Some(1) => StepProofVerification::Invalid,
            _ => StepProofVerification::Unavailable,
        }
    }
    #[cfg(not(feature = "test-verifier"))]
    {
        let _ = proof_state;
        StepProofVerification::Unavailable
    }
}

pub fn process_submit_step_proof(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    SubmitStepProof {
        er_slot,
        verifier_mode,
    }: SubmitStepProof,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: SubmitStepProof, er_slot={}", er_slot);

    if accounts.len() < 6 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let submitter = &accounts[0];
    let session = &accounts[1];
    let checkpoint = &accounts[2];
    let proof = &accounts[3];
    let bond_recipient = &accounts[4];
    let cursor = &accounts[5];

    if !submitter.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    load_session(program_id, session)?;
    let session_key = session.key();
    let mut checkpoint_state = load_checkpoint(program_id, session_key, er_slot, checkpoint)?;
    if checkpoint_state.status != CheckpointStatus::Challenged {
        return Err(PortalError::CheckpointStateInvalid.into());
    }
    let mut cursor_state = load_cursor(program_id, session_key, cursor)?;
    require_active_checkpoint(&cursor_state, checkpoint.key(), er_slot)?;

    let proof_state = load_step_proof(program_id, checkpoint.key(), proof)?;
    if proof_state.checkpoint != *checkpoint.key() {
        return Err(PortalError::StepProofCheckpointMismatch.into());
    }
    if proof_state.authority != checkpoint_state.challenger {
        return Err(PortalError::Unauthorized.into());
    }
    if !proof_state.sealed {
        return Err(PortalError::StepProofNotSealed.into());
    }
    let proof_len = proof_state.written_len as usize;
    if proof_len == 0 || proof_len > MAX_STEP_PROOF_BYTES {
        return Err(PortalError::StepProofChunkOutOfBounds.into());
    }
    let proof_hash = hashv(&[&proof_state.data[..proof_len]]).to_bytes();
    if proof_hash != proof_state.proof_hash {
        return Err(PortalError::StepProofHashMismatch.into());
    }
    if proof_state.public_input_hash
        != step_proof_public_input_hash(
            session_key,
            checkpoint.key(),
            &checkpoint_state,
            proof_state.proof_kind,
            proof_state.proof_version,
            proof_state.step_index,
        )
    {
        return Err(PortalError::StepProofPublicInputMismatch.into());
    }

    match verify_step_proof(verifier_mode, &proof_state) {
        StepProofVerification::Unavailable => Err(PortalError::StepProofVerifierUnavailable.into()),
        StepProofVerification::Invalid => {
            if bond_recipient.key() != &checkpoint_state.challenger {
                return Err(PortalError::Unauthorized.into());
            }
            slash_checkpoint_bond(checkpoint, bond_recipient, &mut checkpoint_state)?;
            checkpoint_state.status = CheckpointStatus::Invalid;
            store_checkpoint(checkpoint, &checkpoint_state)?;
            clear_active_checkpoint(&mut cursor_state);
            store_cursor(cursor, &cursor_state)
        }
        StepProofVerification::Valid => {
            checkpoint_state.status = CheckpointStatus::Pending;
            checkpoint_state.challenger = [0; 32];
            checkpoint_state.challenged_at_l1_slot = 0;
            checkpoint_state.challenge_resolved = true;
            store_checkpoint(checkpoint, &checkpoint_state)
        }
    }
}
