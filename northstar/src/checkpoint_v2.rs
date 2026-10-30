//! Canonical transport and structural reconstruction of continuity checkpoints.
//! Artifact validation does not authenticate L1 inputs or verify SVM execution.

use {
    crate::{
        checkpoint::{
            self, CheckpointArtifactError, CheckpointCommitmentV1, MerkleAuthenticationPathV1,
            MerkleTree, TreeKind,
        },
        continuity_v2::{
            hash_parts, CommittedStateV2, RecordedTransitionV2, StateErrorV2, TransitionPayloadV2,
        },
    },
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_zk_types::continuity::{InputContextV2, StateRootOpeningV2},
    solana_pubkey::Pubkey,
    thiserror::Error,
};

pub const MAX_CHECKPOINT_TRANSITIONS_V2: usize = 16;
const MAX_DA_BYTES_V2: usize = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ArtifactErrorV2 {
    #[error("invalid versioned checkpoint state: {0}")]
    State(#[from] StateErrorV2),
    #[error("invalid checkpoint transport: {0}")]
    Transport(#[from] CheckpointArtifactError),
    #[error("invalid v2 checkpoint encoding")]
    Encoding,
    #[error("invalid v2 checkpoint transition count or slot")]
    InvalidBatch,
    #[error("v2 checkpoint DA limit exceeded")]
    Capacity,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DaStepPageV2 {
    pub index: u32,
    pub pre_state: CommittedStateV2,
    pub transition: RecordedTransitionV2,
    pub pre_state_path: MerkleAuthenticationPathV1,
    pub post_state_path: MerkleAuthenticationPathV1,
    pub effect_path: MerkleAuthenticationPathV1,
    pub effect_leaf: [u8; 32],
    pub readonly_paths: Vec<MerkleAuthenticationPathV1>,
    pub settlement_paths: Vec<MerkleAuthenticationPathV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CheckpointArtifactV2 {
    pub checkpoint: CheckpointCommitmentV1,
    pub pre: StateRootOpeningV2,
    pub post: StateRootOpeningV2,
    pub readonly_l1_root: [u8; 32],
    pub pages: Vec<DaStepPageV2>,
}

fn encode<T: BorshSerialize>(value: &T) -> Result<Vec<u8>, ArtifactErrorV2> {
    borsh::to_vec(value).map_err(|_| ArtifactErrorV2::Encoding)
}

fn index(value: usize) -> Result<u32, ArtifactErrorV2> {
    u32::try_from(value).map_err(|_| ArtifactErrorV2::Capacity)
}

pub fn build_checkpoint_artifact_v2(
    base: &CommittedStateV2,
    er_slot: u64,
    transitions: &[RecordedTransitionV2],
) -> Result<CheckpointArtifactV2, ArtifactErrorV2> {
    if transitions.is_empty()
        || transitions.len() > MAX_CHECKPOINT_TRANSITIONS_V2
        || er_slot == 0
        || transitions.iter().any(|step| step.er_slot > er_slot)
        || transitions
            .windows(2)
            .any(|pair| pair[0].er_slot > pair[1].er_slot)
    {
        return Err(ArtifactErrorV2::InvalidBatch);
    }
    let mut state = base.clone();
    let pre = state.opening()?;
    let mut roots = vec![state.root()?];
    let mut prestates = Vec::with_capacity(transitions.len());
    let mut effect_leaves = Vec::with_capacity(transitions.len());
    let mut readonly_leaves = Vec::new();
    let mut settlement_leaves = Vec::new();
    let mut readonly_ranges = Vec::new();
    let mut settlement_ranges = Vec::new();
    for (step_index, transition) in transitions.iter().enumerate() {
        if transition.readonly_l1_values.len() > checkpoint::MAX_READONLY_L1_VALUES_PER_STEP_V1
            || transition
                .readonly_l1_values
                .windows(2)
                .any(|pair| pair[0].account >= pair[1].account)
            || transition.settlement_effects.len() > checkpoint::MAX_SETTLEMENT_EFFECTS_PER_STEP_V1
        {
            return Err(ArtifactErrorV2::InvalidBatch);
        }
        prestates.push(state.clone());
        transition.apply(&mut state)?;
        roots.push(state.root()?);
        let transaction = match &transition.payload {
            TransitionPayloadV2::Ingress { record, .. } => encode(record)?,
            TransitionPayloadV2::Execution { transaction, .. }
            | TransitionPayloadV2::TokenIngress { transaction, .. } => transaction.clone(),
        };
        let mut effect = b"northstar-step-v2".to_vec();
        effect.extend(encode(transition)?);
        if effect.len() > checkpoint::MAX_TRANSACTION_EFFECT_BYTES_V1 {
            return Err(ArtifactErrorV2::Capacity);
        }
        effect_leaves.push(checkpoint::transaction_effect_leaf_hash(
            index(step_index)?,
            &transaction,
            &effect,
        )?);
        let start = readonly_leaves.len();
        for (value_index, value) in transition.readonly_l1_values.iter().enumerate() {
            readonly_leaves.push(checkpoint::readonly_l1_leaf_hash(
                index(readonly_leaves.len())?,
                index(step_index)?,
                index(value_index)?,
                value,
            )?);
        }
        readonly_ranges.push(start..readonly_leaves.len());
        let start = settlement_leaves.len();
        for (effect_index, effect) in transition.settlement_effects.iter().enumerate() {
            if effect.is_empty() || effect.len() > checkpoint::MAX_SETTLEMENT_EFFECT_BYTES_V1 {
                return Err(ArtifactErrorV2::Capacity);
            }
            settlement_leaves.push(checkpoint::settlement_effect_leaf_hash(
                index(settlement_leaves.len())?,
                index(step_index)?,
                index(effect_index)?,
                effect,
            )?);
        }
        settlement_ranges.push(start..settlement_leaves.len());
    }
    let post = state.opening()?;
    let trace = MerkleTree::new(
        TreeKind::Trace,
        roots
            .iter()
            .enumerate()
            .map(|(position, root)| {
                checkpoint::trace_leaf_hash(
                    index(position).map_err(|_| CheckpointArtifactError::Encoding)?,
                    root,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    let effects = MerkleTree::new(TreeKind::TransactionEffect, effect_leaves.clone())?;
    let readonly = MerkleTree::new(TreeKind::ReadonlyL1, readonly_leaves)?;
    let settlements = MerkleTree::new(TreeKind::SettlementEffect, settlement_leaves)?;
    let input_context = InputContextV2 {
        session_context: pre.session_context,
        readonly_l1_root: readonly.root(),
        ingress_start: pre.ingress_count,
        ingress_start_root: pre.ingress_root,
        ingress_end: post.ingress_count,
        ingress_end_root: post.ingress_root,
    }
    .commitment(hash_parts)
    .map_err(StateErrorV2::from)?;
    let mut pages = Vec::with_capacity(transitions.len());
    for (position, (pre_state, transition)) in prestates.into_iter().zip(transitions).enumerate() {
        pages.push(DaStepPageV2 {
            index: index(position)?,
            pre_state,
            transition: transition.clone(),
            pre_state_path: trace.path(position)?,
            post_state_path: trace.path(position + 1)?,
            effect_path: effects.path(position)?,
            effect_leaf: effect_leaves[position],
            readonly_paths: readonly_ranges[position]
                .clone()
                .map(|offset| readonly.path(offset))
                .collect::<Result<_, _>>()?,
            settlement_paths: settlement_ranges[position]
                .clone()
                .map(|offset| settlements.path(offset))
                .collect::<Result<_, _>>()?,
        });
    }
    if borsh::object_length(&pages).map_err(|_| ArtifactErrorV2::Encoding)? > MAX_DA_BYTES_V2 {
        return Err(ArtifactErrorV2::Capacity);
    }
    let mut artifact = CheckpointArtifactV2 {
        checkpoint: CheckpointCommitmentV1 {
            version: 2,
            session: Pubkey::new_from_array(base.identity().session),
            er_slot,
            step_count: index(transitions.len())?,
            previous_state_root: pre.commitment(hash_parts).map_err(StateErrorV2::from)?,
            new_state_root: post.commitment(hash_parts).map_err(StateErrorV2::from)?,
            trace_root: trace.root(),
            transaction_effect_root: effects.root(),
            readonly_l1_root: input_context,
            effect_commitment: settlements.root(),
            da_commitment: [0; 32],
        },
        pre,
        post,
        readonly_l1_root: readonly.root(),
        pages,
    };
    artifact.checkpoint.da_commitment = hash_parts(&[b"northstar-da-v2", &encode(&artifact)?]);
    Ok(artifact)
}

pub fn checkpoint_binding_hash_v2(
    header: &CheckpointCommitmentV1,
    pre: &StateRootOpeningV2,
    post: &StateRootOpeningV2,
) -> Result<[u8; 32], ArtifactErrorV2> {
    Ok(hash_parts(&[
        b"northstar-checkpoint-binding-v2",
        &encode(&(header, pre, post))?,
    ]))
}

impl CheckpointArtifactV2 {
    pub fn binding_hash(&self) -> Result<[u8; 32], ArtifactErrorV2> {
        checkpoint_binding_hash_v2(&self.checkpoint, &self.pre, &self.post)
    }

    pub fn validate(&self) -> Result<(), ArtifactErrorV2> {
        let first = self.pages.first().ok_or(ArtifactErrorV2::InvalidBatch)?;
        let transitions = self
            .pages
            .iter()
            .map(|page| page.transition.clone())
            .collect::<Vec<_>>();
        let expected =
            build_checkpoint_artifact_v2(&first.pre_state, self.checkpoint.er_slot, &transitions)?;
        if expected != *self {
            return Err(ArtifactErrorV2::InvalidBatch);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "checkpoint_v2_tests.rs"]
mod tests;
