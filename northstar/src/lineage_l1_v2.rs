use {
    crate::{
        checkpoint::CheckpointCommitmentV1, checkpoint_v2::checkpoint_binding_hash_v2,
        continuity_v2::hash_parts, lineage_store_v2::L1CheckpointHeadV2,
    },
    borsh::BorshDeserialize,
    northstar_portal::{
        Checkpoint, CheckpointBindingV2, CheckpointCursor, CheckpointStatus, SessionV2,
    },
    solana_account::ReadableAccount,
    solana_pubkey::Pubkey,
    solana_runtime::bank::Bank,
    std::io,
};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn session_from_l1_v2(bank: &Bank, portal: &Pubkey) -> io::Result<Option<(Pubkey, SessionV2)>> {
    let (key, bump) = northstar_portal::find_session_pda(portal);
    let Some(account) = bank.get_account(&key) else {
        return Ok(None);
    };
    if account.owner() != portal || account.executable() {
        return Err(invalid("session owner differs"));
    }
    let frame = SessionV2::try_from_slice(account.data())?;
    if !frame.is_valid() || frame.base.bump != bump {
        return Err(invalid("session is not a valid continuity session"));
    }
    frame
        .identity(portal, &key)
        .commitment(hash_parts)
        .map_err(io::Error::other)?;
    Ok(Some((key, frame)))
}

pub fn checkpoint_head_from_l1_v2(
    bank: &Bank,
    portal: &Pubkey,
    session: &Pubkey,
    frame: &SessionV2,
) -> io::Result<Option<(L1CheckpointHeadV2, Checkpoint)>> {
    let (cursor_key, bump) = northstar_portal::find_checkpoint_cursor_pda(portal, session);
    let Some(account) = bank.get_account(&cursor_key) else {
        if frame.consumed_ingress_count != 0 {
            return Err(invalid("accepted checkpoint cursor is missing"));
        }
        return Ok(None);
    };
    if account.owner() != portal || account.executable() {
        return Err(invalid("checkpoint cursor owner differs"));
    }
    let cursor = CheckpointCursor::try_from_slice(account.data())?;
    if !cursor.is_valid() || cursor.session != *session || cursor.bump != bump {
        return Err(invalid("checkpoint cursor binding differs"));
    }
    if cursor.latest_finalized_checkpoint == Pubkey::default() {
        if frame.consumed_ingress_count != 0
            || cursor.latest_finalized_er_slot != 0
            || cursor.latest_finalized_state_root != [0; 32]
        {
            return Err(invalid("initial checkpoint cursor differs"));
        }
        return Ok(None);
    }
    let checkpoint_key = cursor.latest_finalized_checkpoint;
    let account = bank
        .get_account(&checkpoint_key)
        .ok_or_else(|| invalid("accepted checkpoint is missing"))?;
    if account.owner() != portal || account.executable() {
        return Err(invalid("checkpoint owner differs"));
    }
    let checkpoint = Checkpoint::try_from_slice(account.data())?;
    if !checkpoint.is_valid()
        || checkpoint.session != *session
        || checkpoint.er_slot != cursor.latest_finalized_er_slot
        || checkpoint.new_state_root != cursor.latest_finalized_state_root
        || checkpoint.proposer != frame.base.validator
        || northstar_portal::find_checkpoint_pda(portal, session, checkpoint.er_slot).0
            != checkpoint_key
        || !matches!(
            checkpoint.status,
            CheckpointStatus::Committed | CheckpointStatus::Settled
        )
    {
        return Err(invalid("accepted checkpoint binding differs"));
    }
    let (binding_key, binding_bump) =
        northstar_portal::find_checkpoint_binding_v2_pda(portal, &checkpoint_key);
    let binding_account = bank
        .get_account(&binding_key)
        .ok_or_else(|| invalid("checkpoint context is missing"))?;
    if binding_account.owner() != portal || binding_account.executable() {
        return Err(invalid("checkpoint context owner differs"));
    }
    let binding = CheckpointBindingV2::try_from_slice(binding_account.data())?;
    let context = frame
        .identity(portal, session)
        .commitment(hash_parts)
        .map_err(io::Error::other)?;
    if binding.discriminator != CheckpointBindingV2::DISCRIMINATOR
        || binding.bump != binding_bump
        || binding.checkpoint != checkpoint_key.to_bytes()
        || binding.pre.session_context != context
        || binding.post.session_context != context
        || binding.post.ingress_count != frame.consumed_ingress_count
        || binding.post.ingress_root != frame.consumed_ingress_root
        || binding
            .pre
            .commitment(hash_parts)
            .map_err(io::Error::other)?
            != checkpoint.previous_state_root
        || binding
            .post
            .commitment(hash_parts)
            .map_err(io::Error::other)?
            != checkpoint.new_state_root
        || binding.input_context_root != checkpoint.readonly_l1_root
    {
        return Err(invalid("accepted checkpoint context differs"));
    }
    let header = CheckpointCommitmentV1 {
        version: 2,
        session: *session,
        er_slot: checkpoint.er_slot,
        step_count: u32::try_from(checkpoint.step_count)
            .map_err(|_| invalid("checkpoint count exceeds its encoding"))?,
        previous_state_root: checkpoint.previous_state_root,
        new_state_root: checkpoint.new_state_root,
        trace_root: checkpoint.trace_root,
        transaction_effect_root: checkpoint.tx_effect_root,
        readonly_l1_root: checkpoint.readonly_l1_root,
        da_commitment: checkpoint.da_commitment,
        effect_commitment: checkpoint.effect_commitment,
    };
    Ok(Some((
        L1CheckpointHeadV2 {
            checkpoint: checkpoint_key.to_bytes(),
            er_slot: checkpoint.er_slot,
            state_root: checkpoint.new_state_root,
            checkpoint_hash: checkpoint_binding_hash_v2(&header, &binding.pre, &binding.post)
                .map_err(io::Error::other)?,
        },
        checkpoint,
    )))
}
