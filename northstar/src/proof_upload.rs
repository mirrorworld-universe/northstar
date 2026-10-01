//! Reconcile a durable proof with authenticated L1 accounts; L1 owns upload progress.
use {
    crate::{
        checkpoint::CheckpointArtifactV1, proof_jobs::ProofJobBinding,
        proof_verification::VerifiedProof,
    },
    borsh::BorshDeserialize,
    northstar_portal::{
        Challenge, ChallengeStatus, ChallengeTurn, Checkpoint, CheckpointCursor, CheckpointStatus,
        CreateStepProof, DataAvailabilityProof, DataAvailabilityStatus, PortalInstruction,
        ResolveChallenge, SealStepProof, Session, StepProofAccount, StepProofVerifierMode,
        WriteStepProof, MAX_STEP_PROOF_CHUNK, TX_EFFECT_AUTH_PATH_NODES,
    },
    northstar_transaction_proof::{
        commitment::{bytes, fr_to_bytes, SESSION_CONTEXT_TAG},
        public_inputs_bytes, session_context_bytes_v1,
    },
    northstar_zk_types::{
        ErStepPublicInputsV1, FrBytes, ER_STEP_PROOF_KIND_FULL_TRANSACTION,
        ER_STEP_PROOF_VERSION_V1,
    },
    solana_account::ReadableAccount,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
    solana_runtime::bank::Bank,
    solana_sha256_hasher::hashv,
};

#[derive(Debug, PartialEq, Eq)]
pub enum ProofUploadAction {
    Wait,
    Prove,
    Submit(Instruction),
    Finished { proof_accepted: bool },
}

pub fn expected_binding(
    portal: Pubkey,
    session: &Session,
    artifact: &CheckpointArtifactV1,
    step: u32,
) -> Result<ProofJobBinding, &'static str> {
    let commitment = &artifact.checkpoint;
    let page = artifact
        .da
        .pages
        .get(step as usize)
        .ok_or("proof step missing")?;
    if commitment.session != northstar_portal::find_session_pda(&portal).0
        || page.step_index != step
        || !page.verify_authentication_paths(commitment)
    {
        return Err("proof checkpoint binding");
    }
    let context = session_context_bytes_v1(
        portal.to_bytes(),
        commitment.session.to_bytes(),
        session.grid_id,
        session.nonce,
        session.validator.to_bytes(),
    );
    let field = |value| FrBytes::new(value).map_err(|_| "noncanonical proof input");
    let public_inputs = public_inputs_bytes(ErStepPublicInputsV1 {
        domain: FrBytes::er_step_domain_v1(
            ER_STEP_PROOF_KIND_FULL_TRANSACTION,
            ER_STEP_PROOF_VERSION_V1,
        ),
        session_context: fr_to_bytes(
            bytes(SESSION_CONTEXT_TAG, &context).map_err(|_| "session context")?,
        ),
        slot_step: FrBytes::from_u64_pair(commitment.er_slot, u64::from(step)),
        pre_state_root: field(page.pre_state_root)?,
        post_state_root: field(page.post_state_root)?,
        tx_effect_root: field(page.transaction_effect_commitment)?,
        readonly_l1_root: field(commitment.readonly_l1_root)?,
        settlement_effect_root: field(commitment.effect_commitment)?,
    });
    let checkpoint =
        northstar_portal::find_checkpoint_pda(&portal, &commitment.session, commitment.er_slot).0;
    Ok(ProofJobBinding {
        portal: portal.to_bytes(),
        session: commitment.session.to_bytes(),
        checkpoint: checkpoint.to_bytes(),
        challenge: northstar_portal::find_challenge_pda(&portal, &checkpoint)
            .0
            .to_bytes(),
        er_slot: commitment.er_slot,
        step,
        public_inputs,
    })
}

fn state<T: BorshDeserialize>(
    bank: &Bank,
    portal: &Pubkey,
    key: &Pubkey,
    discriminator: &[u8; 8],
) -> Result<Option<T>, &'static str> {
    let Some(account) = bank.get_account(key) else {
        return Ok(None);
    };
    if account.owner() != portal || !account.data().starts_with(discriminator) {
        return Err("proof account owner or discriminator");
    }
    T::try_from_slice(account.data())
        .map(Some)
        .map_err(|_| "proof account encoding")
}

pub fn reconcile_upload(
    bank: &Bank,
    challenger: &Pubkey,
    binding: &ProofJobBinding,
    artifact: &CheckpointArtifactV1,
    verified_proof: Option<&VerifiedProof>,
) -> Result<ProofUploadAction, &'static str> {
    let portal = Pubkey::new_from_array(binding.portal);
    let session_key = Pubkey::new_from_array(binding.session);
    let checkpoint_key = Pubkey::new_from_array(binding.checkpoint);
    let challenge_key = Pubkey::new_from_array(binding.challenge);
    let Some(session) = state::<Session>(bank, &portal, &session_key, &Session::DISCRIMINATOR)?
    else {
        return Ok(ProofUploadAction::Wait);
    };
    if expected_binding(portal, &session, artifact, binding.step)? != *binding {
        return Err("proof job context changed");
    }
    let Some(checkpoint) =
        state::<Checkpoint>(bank, &portal, &checkpoint_key, &Checkpoint::DISCRIMINATOR)?
    else {
        return Ok(ProofUploadAction::Wait);
    };
    let expected = &artifact.checkpoint;
    if checkpoint.session != session_key
        || checkpoint.er_slot != binding.er_slot
        || checkpoint.step_count != u64::from(expected.step_count)
        || checkpoint.previous_state_root != expected.previous_state_root
        || checkpoint.new_state_root != expected.new_state_root
        || checkpoint.trace_root != expected.trace_root
        || checkpoint.tx_effect_root != expected.transaction_effect_root
        || checkpoint.readonly_l1_root != expected.readonly_l1_root
        || checkpoint.da_commitment != expected.da_commitment
        || checkpoint.effect_commitment != expected.effect_commitment
        || checkpoint.proposer != session.validator
    {
        return Err("proof checkpoint changed");
    }
    let Some(challenge) =
        state::<Challenge>(bank, &portal, &challenge_key, &Challenge::DISCRIMINATOR)?
    else {
        return Ok(ProofUploadAction::Wait);
    };
    if challenge.checkpoint != checkpoint_key
        || challenge.challenger != *challenger
        || checkpoint.challenger != *challenger
        || challenge.respondent != session.validator
    {
        return Err("proof challenger authority binding");
    }
    if challenge.status != ChallengeStatus::Active {
        if !checkpoint.challenge_resolved {
            return Err("proof outcome mismatch");
        }
        return Ok(ProofUploadAction::Finished {
            proof_accepted: challenge.status == ChallengeStatus::ValidatorWon,
        });
    }
    if checkpoint.status != CheckpointStatus::Challenged || checkpoint.challenge_resolved {
        return Err("proof checkpoint is not challenged");
    }
    if bank.slot() >= challenge.turn_deadline_l1_slot
        || bank.slot() >= challenge.hard_deadline_l1_slot
    {
        return Err("proof challenge deadline elapsed");
    }
    if challenge.turn != ChallengeTurn::Prove || challenge.start_step != u64::from(binding.step) {
        return Ok(ProofUploadAction::Wait);
    }
    let page = &artifact.da.pages[binding.step as usize];
    if challenge.end_step != challenge.start_step.saturating_add(1)
        || challenge.start_state_root != page.pre_state_root
        || challenge.end_state_root != page.post_state_root
    {
        return Err("proof isolated interval binding");
    }
    let da_key = northstar_portal::find_da_proof_pda(&portal, &challenge_key).0;
    let Some(da) = state::<DataAvailabilityProof>(
        bank,
        &portal,
        &da_key,
        &DataAvailabilityProof::DISCRIMINATOR,
    )?
    else {
        return Ok(ProofUploadAction::Wait);
    };
    if da.challenge != challenge_key
        || da.checkpoint != checkpoint_key
        || da.commitment != checkpoint.da_commitment
    {
        return Err("proof data availability binding");
    }
    if da.status != DataAvailabilityStatus::Revealed || da.payload_root != checkpoint.da_commitment
    {
        return Ok(ProofUploadAction::Wait);
    }
    let cursor_key = northstar_portal::find_checkpoint_cursor_pda(&portal, &session_key).0;
    let cursor =
        state::<CheckpointCursor>(bank, &portal, &cursor_key, &CheckpointCursor::DISCRIMINATOR)?
            .ok_or("proof cursor missing")?;
    if cursor.session != session_key
        || cursor.active_checkpoint != checkpoint_key
        || cursor.active_er_slot != binding.er_slot
    {
        return Err("proof active checkpoint binding");
    }
    let Some(proof) = verified_proof else {
        return Ok(ProofUploadAction::Prove);
    };
    let proof = proof.bytes_for(binding)?;
    let proof_key = northstar_portal::find_step_proof_pda(&portal, &checkpoint_key).0;
    let proof_state =
        state::<StepProofAccount>(bank, &portal, &proof_key, &StepProofAccount::DISCRIMINATOR)?;
    let common = || {
        vec![
            AccountMeta::new_readonly(*challenger, true),
            AccountMeta::new_readonly(session_key, false),
            AccountMeta::new_readonly(checkpoint_key, false),
            AccountMeta::new_readonly(challenge_key, false),
            AccountMeta::new(proof_key, false),
        ]
    };
    let (accounts, instruction) = if let Some(upload) = proof_state {
        if upload.checkpoint != checkpoint_key
            || upload.challenge != challenge_key
            || upload.authority != *challenger
            || upload.proof_kind != ER_STEP_PROOF_KIND_FULL_TRANSACTION
            || upload.proof_version != ER_STEP_PROOF_VERSION_V1
            || upload.step_index != u64::from(binding.step)
            || upload.session_context != binding.public_inputs[32..64]
            || upload.tx_effect_root != page.transaction_effect_commitment
            || upload.readonly_l1_root != expected.readonly_l1_root
            || upload.settlement_effect_root != expected.effect_commitment
            || upload.public_input_hash
                != northstar_portal::step_proof_public_input_hash(
                    &portal,
                    &session_key,
                    &checkpoint_key,
                    &checkpoint,
                    &challenge,
                    &upload,
                )
        {
            return Err("proof upload authority or context changed");
        }
        let offset = upload.written_len as usize;
        if offset > proof.len() || upload.data[..offset] != proof[..offset] {
            return Err("proof uploaded prefix mismatch");
        }
        if upload.sealed {
            if offset != proof.len() || upload.proof_hash != hashv(&[proof]).to_bytes() {
                return Err("proof sealed result mismatch");
            }
            (
                vec![
                    AccountMeta::new_readonly(*challenger, true),
                    AccountMeta::new_readonly(session_key, false),
                    AccountMeta::new(checkpoint_key, false),
                    AccountMeta::new(challenge_key, false),
                    AccountMeta::new_readonly(da_key, false),
                    AccountMeta::new_readonly(proof_key, false),
                    AccountMeta::new(*challenger, false),
                    AccountMeta::new(cursor_key, false),
                ],
                PortalInstruction::ResolveChallenge(ResolveChallenge {
                    er_slot: binding.er_slot,
                    verifier_mode: StepProofVerifierMode::Production,
                }),
            )
        } else if offset == proof.len() {
            (
                common(),
                PortalInstruction::SealStepProof(SealStepProof {
                    er_slot: binding.er_slot,
                    proof_len: proof.len() as u32,
                }),
            )
        } else {
            let remaining = &proof[offset..proof.len().min(offset + MAX_STEP_PROOF_CHUNK)];
            let mut chunk = [0; MAX_STEP_PROOF_CHUNK];
            chunk[..remaining.len()].copy_from_slice(remaining);
            (
                common(),
                PortalInstruction::WriteStepProof(WriteStepProof {
                    er_slot: binding.er_slot,
                    offset: offset as u32,
                    chunk_len: remaining.len() as u16,
                    chunk,
                }),
            )
        }
    } else {
        let mut path = [[0; 32]; TX_EFFECT_AUTH_PATH_NODES];
        let siblings = &page.transaction_effect_path.siblings;
        if siblings.len() > path.len() {
            return Err("proof authentication path too long");
        }
        path[..siblings.len()].copy_from_slice(siblings);
        let mut accounts = common();
        accounts[0].is_writable = true;
        accounts.push(AccountMeta::new_readonly(
            solana_sdk_ids::system_program::id(),
            false,
        ));
        (
            accounts,
            PortalInstruction::CreateStepProof(CreateStepProof {
                er_slot: binding.er_slot,
                proof_kind: ER_STEP_PROOF_KIND_FULL_TRANSACTION,
                proof_version: ER_STEP_PROOF_VERSION_V1,
                step_index: u64::from(binding.step),
                session_context: binding.public_inputs[32..64].try_into().unwrap(),
                tx_effect_root: page.transaction_effect_commitment,
                tx_effect_path_len: siblings.len() as u8,
                tx_effect_path: path,
                readonly_l1_root: expected.readonly_l1_root,
                settlement_effect_root: expected.effect_commitment,
            }),
        )
    };
    Ok(ProofUploadAction::Submit(Instruction {
        program_id: portal,
        accounts,
        data: borsh::to_vec(&instruction).map_err(|_| "proof instruction encoding")?,
    }))
}

#[cfg(test)]
#[path = "proof_upload_tests.rs"]
mod tests;
