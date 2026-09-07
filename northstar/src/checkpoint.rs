use {
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_zk_types::FrBytes,
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hashv,
    std::collections::HashSet,
    thiserror::Error,
};

pub type CommitmentHash = [u8; 32];

pub const CHECKPOINT_FORMAT_VERSION_V1: u8 = 1;
pub const CANONICAL_CHECKPOINT_STEPS_V1: usize = 16;
pub const MAX_TRANSACTION_BYTES_V1: usize = 1_232;
pub const MAX_TRANSACTION_EFFECT_BYTES_V1: usize = 1_048_576;
pub const MAX_SETTLEMENT_EFFECT_BYTES_V1: usize = 16_384;
pub const MAX_READONLY_L1_VALUES_PER_STEP_V1: usize = 256;
pub const MAX_SETTLEMENT_EFFECTS_PER_STEP_V1: usize = 256;

const CHECKPOINT_HASH_DOMAIN_V1: &[u8] = b"northstar-checkpoint-v1";

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointArtifactError {
    #[error("checkpoint encoding failed")]
    Encoding,
    #[error("checkpoint version is unsupported")]
    UnsupportedVersion,
    #[error("checkpoint must contain exactly 16 ordered steps")]
    InvalidStepCount,
    #[error("checkpoint step indexes are not canonical")]
    InvalidStepIndex,
    #[error("checkpoint state-root chain is invalid")]
    InvalidStateRoot,
    #[error("checkpoint account-state values are not canonical")]
    InvalidStateAccounts,
    #[error("checkpoint transaction data is invalid")]
    InvalidTransaction,
    #[error("checkpoint transaction effect is invalid")]
    InvalidTransactionEffect,
    #[error("checkpoint readonly L1 values are not canonical")]
    InvalidReadonlyL1Values,
    #[error("checkpoint settlement effects are not canonical")]
    InvalidSettlementEffects,
    #[error("checkpoint DA manifest is invalid")]
    InvalidManifest,
    #[error("checkpoint DA page is invalid")]
    InvalidPage,
    #[error("checkpoint commitment does not match its DA package")]
    CommitmentMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, BorshDeserialize, BorshSerialize)]
pub struct ReadonlyL1ValueV1 {
    pub account: Pubkey,
    pub owner: Pubkey,
    pub lamports: u64,
    pub data_hash: CommitmentHash,
    pub observed_l1_slot: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, BorshDeserialize, BorshSerialize)]
pub struct StateAccountValueV1 {
    pub account: Pubkey,
    pub owner: Pubkey,
    pub lamports: u64,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data_hash: CommitmentHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct CheckpointAccountEffectV1 {
    pub step_index: u32,
    pub value: StateAccountValueV1,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct CheckpointTransactionEffectV1 {
    pub version: u8,
    pub step_index: u32,
    pub transaction_hash: CommitmentHash,
    pub executed_units: u64,
    pub account_effects: Vec<CheckpointAccountEffectV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointStepInputV1 {
    pub step_index: u32,
    pub transaction: Vec<u8>,
    pub transaction_effect: Vec<u8>,
    pub pre_state_root: CommitmentHash,
    pub post_state_root: CommitmentHash,
    pub readonly_l1_values: Vec<ReadonlyL1ValueV1>,
    pub settlement_effects: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct MerkleAuthenticationPathV1 {
    pub leaf_index: u32,
    pub leaf_count: u32,
    pub siblings: Vec<CommitmentHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct AuthenticatedReadonlyL1ValueV1 {
    pub global_index: u32,
    pub value_index: u32,
    pub value: ReadonlyL1ValueV1,
    pub path: MerkleAuthenticationPathV1,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct AuthenticatedSettlementEffectV1 {
    pub global_index: u32,
    pub effect_index: u32,
    pub effect: Vec<u8>,
    pub path: MerkleAuthenticationPathV1,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct DaStepPageV1 {
    pub version: u8,
    pub page_index: u32,
    pub step_index: u32,
    pub transaction: Vec<u8>,
    pub transaction_effect: Vec<u8>,
    pub transaction_effect_commitment: CommitmentHash,
    pub pre_state_root: CommitmentHash,
    pub post_state_root: CommitmentHash,
    pub pre_state_path: MerkleAuthenticationPathV1,
    pub post_state_path: MerkleAuthenticationPathV1,
    pub transaction_effect_path: MerkleAuthenticationPathV1,
    pub readonly_l1_values: Vec<AuthenticatedReadonlyL1ValueV1>,
    pub settlement_effects: Vec<AuthenticatedSettlementEffectV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum DaCodecV1 {
    Raw = 0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct DaPageDescriptorV1 {
    pub page_index: u32,
    pub step_index: u32,
    pub uncompressed_len: u32,
    pub encoded_len: u32,
    pub payload_hash: CommitmentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct DaManifestV1 {
    pub version: u8,
    pub session: Pubkey,
    pub er_slot: u64,
    pub step_count: u32,
    pub codec: DaCodecV1,
    pub dictionary_hash: CommitmentHash,
    pub trace_root: CommitmentHash,
    pub transaction_effect_root: CommitmentHash,
    pub readonly_l1_root: CommitmentHash,
    pub effect_commitment: CommitmentHash,
    pub pages: Vec<DaPageDescriptorV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct DaPackageV1 {
    pub manifest: DaManifestV1,
    pub pages: Vec<DaStepPageV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct CheckpointCommitmentV1 {
    pub version: u8,
    pub session: Pubkey,
    pub er_slot: u64,
    pub step_count: u32,
    pub previous_state_root: CommitmentHash,
    pub new_state_root: CommitmentHash,
    pub trace_root: CommitmentHash,
    pub transaction_effect_root: CommitmentHash,
    pub readonly_l1_root: CommitmentHash,
    pub da_commitment: CommitmentHash,
    pub effect_commitment: CommitmentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct CheckpointArtifactV1 {
    pub checkpoint: CheckpointCommitmentV1,
    pub da: DaPackageV1,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TreeKind {
    State,
    Trace,
    TransactionEffect,
    ReadonlyL1,
    SettlementEffect,
    DataAvailability,
}

impl TreeKind {
    fn tag(self) -> &'static [u8] {
        match self {
            Self::State => b"state",
            Self::Trace => b"trace",
            Self::TransactionEffect => b"transaction-effect",
            Self::ReadonlyL1 => b"readonly-l1",
            Self::SettlementEffect => b"settlement-effect",
            Self::DataAvailability => b"data-availability",
        }
    }

    fn root_is_field(self) -> bool {
        matches!(
            self,
            Self::State | Self::ReadonlyL1 | Self::SettlementEffect
        )
    }
}

struct MerkleTree {
    kind: TreeKind,
    leaf_count: u32,
    layers: Vec<Vec<CommitmentHash>>,
}

impl MerkleTree {
    fn new(kind: TreeKind, leaves: Vec<CommitmentHash>) -> Result<Self, CheckpointArtifactError> {
        let leaf_count =
            u32::try_from(leaves.len()).map_err(|_| CheckpointArtifactError::Encoding)?;
        let width = leaves.len().max(1).next_power_of_two();
        let mut leaf_layer = leaves;
        for index in leaf_layer.len()..width {
            leaf_layer.push(empty_leaf_hash(kind, index)?);
        }
        let mut layers = vec![leaf_layer];
        let mut level = 0u32;
        while layers.last().expect("tree has leaf layer").len() > 1 {
            let previous = layers.last().expect("tree has previous layer");
            let next = previous
                .chunks_exact(2)
                .map(|children| node_hash(kind, level, &children[0], &children[1]))
                .collect();
            layers.push(next);
            level = level
                .checked_add(1)
                .ok_or(CheckpointArtifactError::Encoding)?;
        }
        Ok(Self {
            kind,
            leaf_count,
            layers,
        })
    }

    fn root(&self) -> CommitmentHash {
        let inner = self.layers.last().expect("tree has root layer")[0];
        root_hash(self.kind, self.leaf_count, &inner)
    }

    fn path(
        &self,
        leaf_index: usize,
    ) -> Result<MerkleAuthenticationPathV1, CheckpointArtifactError> {
        if leaf_index >= self.leaf_count as usize {
            return Err(CheckpointArtifactError::InvalidPage);
        }
        let mut index = leaf_index;
        let mut siblings = Vec::with_capacity(self.layers.len().saturating_sub(1));
        for layer in self.layers.iter().take(self.layers.len().saturating_sub(1)) {
            siblings.push(layer[index ^ 1]);
            index /= 2;
        }
        Ok(MerkleAuthenticationPathV1 {
            leaf_index: u32::try_from(leaf_index).map_err(|_| CheckpointArtifactError::Encoding)?,
            leaf_count: self.leaf_count,
            siblings,
        })
    }
}

impl MerkleAuthenticationPathV1 {
    fn verify(
        &self,
        kind: TreeKind,
        leaf_hash: CommitmentHash,
        expected_root: CommitmentHash,
    ) -> bool {
        if self.leaf_count == 0 || self.leaf_index >= self.leaf_count {
            return false;
        }
        let width = (self.leaf_count as usize).max(1).next_power_of_two();
        if self.siblings.len() != width.trailing_zeros() as usize {
            return false;
        }
        let mut index = self.leaf_index as usize;
        let mut current = leaf_hash;
        for (level, sibling) in self.siblings.iter().enumerate() {
            let Ok(level) = u32::try_from(level) else {
                return false;
            };
            current = if index.is_multiple_of(2) {
                node_hash(kind, level, &current, sibling)
            } else {
                node_hash(kind, level, sibling, &current)
            };
            index /= 2;
        }
        root_hash(kind, self.leaf_count, &current) == expected_root
    }
}

impl DaStepPageV1 {
    pub fn verify_authentication_paths(&self, checkpoint: &CheckpointCommitmentV1) -> bool {
        if self.version != CHECKPOINT_FORMAT_VERSION_V1 || self.page_index != self.step_index {
            return false;
        }
        let Ok(pre_leaf) = trace_leaf_hash(self.step_index, &self.pre_state_root) else {
            return false;
        };
        let Some(post_index) = self.step_index.checked_add(1) else {
            return false;
        };
        let Ok(post_leaf) = trace_leaf_hash(post_index, &self.post_state_root) else {
            return false;
        };
        let Ok(transaction_leaf) = transaction_effect_leaf_hash(
            self.step_index,
            &self.transaction,
            &self.transaction_effect,
        ) else {
            return false;
        };
        if transaction_leaf != self.transaction_effect_commitment {
            return false;
        }
        if !self
            .pre_state_path
            .verify(TreeKind::Trace, pre_leaf, checkpoint.trace_root)
            || !self
                .post_state_path
                .verify(TreeKind::Trace, post_leaf, checkpoint.trace_root)
            || !self.transaction_effect_path.verify(
                TreeKind::TransactionEffect,
                transaction_leaf,
                checkpoint.transaction_effect_root,
            )
        {
            return false;
        }
        for authenticated in &self.readonly_l1_values {
            let Ok(leaf) = readonly_l1_leaf_hash(
                authenticated.global_index,
                self.step_index,
                authenticated.value_index,
                &authenticated.value,
            ) else {
                return false;
            };
            if authenticated.path.leaf_index != authenticated.global_index
                || !authenticated.path.verify(
                    TreeKind::ReadonlyL1,
                    leaf,
                    checkpoint.readonly_l1_root,
                )
            {
                return false;
            }
        }
        for authenticated in &self.settlement_effects {
            let Ok(leaf) = settlement_effect_leaf_hash(
                authenticated.global_index,
                self.step_index,
                authenticated.effect_index,
                &authenticated.effect,
            ) else {
                return false;
            };
            if authenticated.path.leaf_index != authenticated.global_index
                || !authenticated.path.verify(
                    TreeKind::SettlementEffect,
                    leaf,
                    checkpoint.effect_commitment,
                )
            {
                return false;
            }
        }
        true
    }
}

impl CheckpointArtifactV1 {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CheckpointArtifactError> {
        borsh::to_vec(self).map_err(|_| CheckpointArtifactError::Encoding)
    }

    pub fn decode_verified(bytes: &[u8]) -> Result<Self, CheckpointArtifactError> {
        let artifact =
            borsh::from_slice::<Self>(bytes).map_err(|_| CheckpointArtifactError::Encoding)?;
        if artifact.canonical_bytes()?.as_slice() != bytes {
            return Err(CheckpointArtifactError::Encoding);
        }
        artifact.verify()?;
        Ok(artifact)
    }

    pub fn verify(&self) -> Result<(), CheckpointArtifactError> {
        if self.checkpoint.version != CHECKPOINT_FORMAT_VERSION_V1
            || self.da.manifest.version != CHECKPOINT_FORMAT_VERSION_V1
        {
            return Err(CheckpointArtifactError::UnsupportedVersion);
        }
        if self.da.pages.len() != CANONICAL_CHECKPOINT_STEPS_V1
            || self.da.manifest.pages.len() != CANONICAL_CHECKPOINT_STEPS_V1
        {
            return Err(CheckpointArtifactError::InvalidStepCount);
        }
        let steps = self.da.pages.iter().map(page_to_input).collect::<Vec<_>>();
        let rebuilt =
            build_checkpoint_artifact_v1(self.checkpoint.session, self.checkpoint.er_slot, steps)?;
        if &rebuilt != self {
            return Err(CheckpointArtifactError::CommitmentMismatch);
        }
        if !self
            .da
            .pages
            .iter()
            .all(|page| page.verify_authentication_paths(&self.checkpoint))
        {
            return Err(CheckpointArtifactError::InvalidPage);
        }
        Ok(())
    }
}

pub fn build_checkpoint_artifact_v1(
    session: Pubkey,
    er_slot: u64,
    steps: Vec<CheckpointStepInputV1>,
) -> Result<CheckpointArtifactV1, CheckpointArtifactError> {
    validate_steps(&steps)?;

    let mut trace_leaves = Vec::with_capacity(steps.len() + 1);
    trace_leaves.push(trace_leaf_hash(0, &steps[0].pre_state_root)?);
    for step in &steps {
        let post_index = step
            .step_index
            .checked_add(1)
            .ok_or(CheckpointArtifactError::InvalidStepIndex)?;
        trace_leaves.push(trace_leaf_hash(post_index, &step.post_state_root)?);
    }
    let transaction_leaves = steps
        .iter()
        .map(|step| {
            transaction_effect_leaf_hash(
                step.step_index,
                &step.transaction,
                &step.transaction_effect,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut readonly_leaves = Vec::new();
    let mut readonly_offsets = Vec::with_capacity(steps.len());
    for step in &steps {
        readonly_offsets.push(readonly_leaves.len());
        for (value_index, value) in step.readonly_l1_values.iter().enumerate() {
            readonly_leaves.push(readonly_l1_leaf_hash(
                u32::try_from(readonly_leaves.len())
                    .map_err(|_| CheckpointArtifactError::Encoding)?,
                step.step_index,
                u32::try_from(value_index).map_err(|_| CheckpointArtifactError::Encoding)?,
                value,
            )?);
        }
    }

    let mut settlement_leaves = Vec::new();
    let mut settlement_offsets = Vec::with_capacity(steps.len());
    for step in &steps {
        settlement_offsets.push(settlement_leaves.len());
        for (effect_index, effect) in step.settlement_effects.iter().enumerate() {
            settlement_leaves.push(settlement_effect_leaf_hash(
                u32::try_from(settlement_leaves.len())
                    .map_err(|_| CheckpointArtifactError::Encoding)?,
                step.step_index,
                u32::try_from(effect_index).map_err(|_| CheckpointArtifactError::Encoding)?,
                effect,
            )?);
        }
    }

    let trace_tree = MerkleTree::new(TreeKind::Trace, trace_leaves)?;
    let transaction_tree = MerkleTree::new(TreeKind::TransactionEffect, transaction_leaves)?;
    let readonly_tree = MerkleTree::new(TreeKind::ReadonlyL1, readonly_leaves)?;
    let settlement_tree = MerkleTree::new(TreeKind::SettlementEffect, settlement_leaves)?;

    let mut pages = Vec::with_capacity(steps.len());
    for (page_index, step) in steps.iter().enumerate() {
        let readonly_offset = readonly_offsets[page_index];
        let readonly_l1_values = step
            .readonly_l1_values
            .iter()
            .enumerate()
            .map(|(value_index, value)| {
                let global_index = readonly_offset
                    .checked_add(value_index)
                    .ok_or(CheckpointArtifactError::Encoding)?;
                Ok(AuthenticatedReadonlyL1ValueV1 {
                    global_index: u32::try_from(global_index)
                        .map_err(|_| CheckpointArtifactError::Encoding)?,
                    value_index: u32::try_from(value_index)
                        .map_err(|_| CheckpointArtifactError::Encoding)?,
                    value: *value,
                    path: readonly_tree.path(global_index)?,
                })
            })
            .collect::<Result<Vec<_>, CheckpointArtifactError>>()?;
        let settlement_offset = settlement_offsets[page_index];
        let settlement_effects = step
            .settlement_effects
            .iter()
            .enumerate()
            .map(|(effect_index, effect)| {
                let global_index = settlement_offset
                    .checked_add(effect_index)
                    .ok_or(CheckpointArtifactError::Encoding)?;
                Ok(AuthenticatedSettlementEffectV1 {
                    global_index: u32::try_from(global_index)
                        .map_err(|_| CheckpointArtifactError::Encoding)?,
                    effect_index: u32::try_from(effect_index)
                        .map_err(|_| CheckpointArtifactError::Encoding)?,
                    effect: effect.clone(),
                    path: settlement_tree.path(global_index)?,
                })
            })
            .collect::<Result<Vec<_>, CheckpointArtifactError>>()?;
        pages.push(DaStepPageV1 {
            version: CHECKPOINT_FORMAT_VERSION_V1,
            page_index: u32::try_from(page_index).map_err(|_| CheckpointArtifactError::Encoding)?,
            step_index: step.step_index,
            transaction: step.transaction.clone(),
            transaction_effect: step.transaction_effect.clone(),
            transaction_effect_commitment: transaction_effect_leaf_hash(
                step.step_index,
                &step.transaction,
                &step.transaction_effect,
            )?,
            pre_state_root: step.pre_state_root,
            post_state_root: step.post_state_root,
            pre_state_path: trace_tree.path(page_index)?,
            post_state_path: trace_tree.path(
                page_index
                    .checked_add(1)
                    .ok_or(CheckpointArtifactError::Encoding)?,
            )?,
            transaction_effect_path: transaction_tree.path(page_index)?,
            readonly_l1_values,
            settlement_effects,
        });
    }

    let trace_root = trace_tree.root();
    let transaction_effect_root = transaction_tree.root();
    let readonly_l1_root = readonly_tree.root();
    let effect_commitment = settlement_tree.root();
    let mut descriptors = Vec::with_capacity(pages.len());
    let mut page_hashes = Vec::with_capacity(pages.len());
    for page in &pages {
        let bytes = encode(page)?;
        let payload_hash = typed_hash(TreeKind::DataAvailability, b"page", &bytes);
        page_hashes.push(payload_hash);
        descriptors.push(DaPageDescriptorV1 {
            page_index: page.page_index,
            step_index: page.step_index,
            uncompressed_len: u32::try_from(bytes.len())
                .map_err(|_| CheckpointArtifactError::Encoding)?,
            encoded_len: u32::try_from(bytes.len())
                .map_err(|_| CheckpointArtifactError::Encoding)?,
            payload_hash,
        });
    }
    let manifest = DaManifestV1 {
        version: CHECKPOINT_FORMAT_VERSION_V1,
        session,
        er_slot,
        step_count: u32::try_from(steps.len()).map_err(|_| CheckpointArtifactError::Encoding)?,
        codec: DaCodecV1::Raw,
        dictionary_hash: typed_hash(TreeKind::DataAvailability, b"dictionary", &[]),
        trace_root,
        transaction_effect_root,
        readonly_l1_root,
        effect_commitment,
        pages: descriptors,
    };
    let manifest_hash = typed_hash(TreeKind::DataAvailability, b"manifest", &encode(&manifest)?);
    let mut da_leaves = Vec::with_capacity(page_hashes.len() + 1);
    da_leaves.push(manifest_hash);
    da_leaves.extend(page_hashes);
    let da_commitment = MerkleTree::new(TreeKind::DataAvailability, da_leaves)?.root();

    Ok(CheckpointArtifactV1 {
        checkpoint: CheckpointCommitmentV1 {
            version: CHECKPOINT_FORMAT_VERSION_V1,
            session,
            er_slot,
            step_count: u32::try_from(steps.len())
                .map_err(|_| CheckpointArtifactError::Encoding)?,
            previous_state_root: steps[0].pre_state_root,
            new_state_root: steps
                .last()
                .expect("validated checkpoint has steps")
                .post_state_root,
            trace_root,
            transaction_effect_root,
            readonly_l1_root,
            da_commitment,
            effect_commitment,
        },
        da: DaPackageV1 { manifest, pages },
    })
}

fn validate_steps(steps: &[CheckpointStepInputV1]) -> Result<(), CheckpointArtifactError> {
    if steps.len() != CANONICAL_CHECKPOINT_STEPS_V1 {
        return Err(CheckpointArtifactError::InvalidStepCount);
    }
    let mut transactions = HashSet::with_capacity(steps.len());
    let mut settlement_effects = HashSet::new();
    for (index, step) in steps.iter().enumerate() {
        if step.step_index as usize != index {
            return Err(CheckpointArtifactError::InvalidStepIndex);
        }
        if step.pre_state_root == [0; 32]
            || step.post_state_root == [0; 32]
            || FrBytes::new(step.pre_state_root).is_err()
            || FrBytes::new(step.post_state_root).is_err()
            || index > 0 && steps[index - 1].post_state_root != step.pre_state_root
        {
            return Err(CheckpointArtifactError::InvalidStateRoot);
        }
        if step.transaction.is_empty()
            || step.transaction.len() > MAX_TRANSACTION_BYTES_V1
            || !transactions.insert(step.transaction.clone())
        {
            return Err(CheckpointArtifactError::InvalidTransaction);
        }
        if step.transaction_effect.is_empty()
            || step.transaction_effect.len() > MAX_TRANSACTION_EFFECT_BYTES_V1
        {
            return Err(CheckpointArtifactError::InvalidTransactionEffect);
        }
        if step.readonly_l1_values.len() > MAX_READONLY_L1_VALUES_PER_STEP_V1
            || step
                .readonly_l1_values
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(CheckpointArtifactError::InvalidReadonlyL1Values);
        }
        if step.settlement_effects.len() > MAX_SETTLEMENT_EFFECTS_PER_STEP_V1 {
            return Err(CheckpointArtifactError::InvalidSettlementEffects);
        }
        for effect in &step.settlement_effects {
            if effect.is_empty()
                || effect.len() > MAX_SETTLEMENT_EFFECT_BYTES_V1
                || !settlement_effects.insert(effect.clone())
            {
                return Err(CheckpointArtifactError::InvalidSettlementEffects);
            }
        }
    }
    Ok(())
}

fn page_to_input(page: &DaStepPageV1) -> CheckpointStepInputV1 {
    CheckpointStepInputV1 {
        step_index: page.step_index,
        transaction: page.transaction.clone(),
        transaction_effect: page.transaction_effect.clone(),
        pre_state_root: page.pre_state_root,
        post_state_root: page.post_state_root,
        readonly_l1_values: page
            .readonly_l1_values
            .iter()
            .map(|authenticated| authenticated.value)
            .collect(),
        settlement_effects: page
            .settlement_effects
            .iter()
            .map(|authenticated| authenticated.effect.clone())
            .collect(),
    }
}

pub fn state_root_v1(
    accounts: &[StateAccountValueV1],
) -> Result<CommitmentHash, CheckpointArtifactError> {
    if accounts.windows(2).any(|pair| pair[0] >= pair[1])
        || accounts.iter().any(|account| account.data_hash == [0; 32])
    {
        return Err(CheckpointArtifactError::InvalidStateAccounts);
    }
    let leaves = accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            let index = u32::try_from(index).map_err(|_| CheckpointArtifactError::Encoding)?;
            Ok(typed_hash(
                TreeKind::State,
                b"leaf",
                &encode(&StateAccountLeafV1 { index, account })?,
            ))
        })
        .collect::<Result<Vec<_>, CheckpointArtifactError>>()?;
    Ok(MerkleTree::new(TreeKind::State, leaves)?.root())
}

#[derive(BorshSerialize)]
struct StateAccountLeafV1<'a> {
    index: u32,
    account: &'a StateAccountValueV1,
}

#[derive(BorshSerialize)]
struct TraceLeafV1 {
    index: u32,
    state_root: CommitmentHash,
}

#[derive(BorshSerialize)]
struct TransactionEffectLeafV1<'a> {
    index: u32,
    transaction: &'a [u8],
    transaction_effect: &'a [u8],
}

#[derive(BorshSerialize)]
struct ReadonlyL1LeafV1<'a> {
    global_index: u32,
    step_index: u32,
    value_index: u32,
    value: &'a ReadonlyL1ValueV1,
}

#[derive(BorshSerialize)]
struct SettlementEffectLeafV1<'a> {
    global_index: u32,
    step_index: u32,
    effect_index: u32,
    effect: &'a [u8],
}

fn trace_leaf_hash(
    index: u32,
    state_root: &CommitmentHash,
) -> Result<CommitmentHash, CheckpointArtifactError> {
    Ok(typed_hash(
        TreeKind::Trace,
        b"leaf",
        &encode(&TraceLeafV1 {
            index,
            state_root: *state_root,
        })?,
    ))
}

fn transaction_effect_leaf_hash(
    index: u32,
    transaction: &[u8],
    transaction_effect: &[u8],
) -> Result<CommitmentHash, CheckpointArtifactError> {
    let mut commitment = typed_hash(
        TreeKind::TransactionEffect,
        b"leaf",
        &encode(&TransactionEffectLeafV1 {
            index,
            transaction,
            transaction_effect,
        })?,
    );
    commitment[0] &= 0x1f;
    Ok(commitment)
}

fn readonly_l1_leaf_hash(
    global_index: u32,
    step_index: u32,
    value_index: u32,
    value: &ReadonlyL1ValueV1,
) -> Result<CommitmentHash, CheckpointArtifactError> {
    Ok(typed_hash(
        TreeKind::ReadonlyL1,
        b"leaf",
        &encode(&ReadonlyL1LeafV1 {
            global_index,
            step_index,
            value_index,
            value,
        })?,
    ))
}

fn settlement_effect_leaf_hash(
    global_index: u32,
    step_index: u32,
    effect_index: u32,
    effect: &[u8],
) -> Result<CommitmentHash, CheckpointArtifactError> {
    Ok(typed_hash(
        TreeKind::SettlementEffect,
        b"leaf",
        &encode(&SettlementEffectLeafV1 {
            global_index,
            step_index,
            effect_index,
            effect,
        })?,
    ))
}

fn empty_leaf_hash(
    kind: TreeKind,
    index: usize,
) -> Result<CommitmentHash, CheckpointArtifactError> {
    let index = u32::try_from(index).map_err(|_| CheckpointArtifactError::Encoding)?;
    Ok(typed_hash(kind, b"empty-leaf", &index.to_le_bytes()))
}

fn node_hash(
    kind: TreeKind,
    level: u32,
    left: &CommitmentHash,
    right: &CommitmentHash,
) -> CommitmentHash {
    hashv(&[
        CHECKPOINT_HASH_DOMAIN_V1,
        kind.tag(),
        b"node",
        &level.to_le_bytes(),
        left,
        right,
    ])
    .to_bytes()
}

fn root_hash(kind: TreeKind, leaf_count: u32, inner: &CommitmentHash) -> CommitmentHash {
    let mut root = hashv(&[
        CHECKPOINT_HASH_DOMAIN_V1,
        kind.tag(),
        b"root",
        &leaf_count.to_le_bytes(),
        inner,
    ])
    .to_bytes();
    if kind.root_is_field() {
        root[0] &= 0x1f;
    }
    root
}

fn typed_hash(kind: TreeKind, label: &[u8], bytes: &[u8]) -> CommitmentHash {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes();
    hashv(&[CHECKPOINT_HASH_DOMAIN_V1, kind.tag(), label, &length, bytes]).to_bytes()
}

fn encode<T: BorshSerialize>(value: &T) -> Result<Vec<u8>, CheckpointArtifactError> {
    borsh::to_vec(value).map_err(|_| CheckpointArtifactError::Encoding)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(value: u64) -> CommitmentHash {
        FrBytes::from_u64(value).to_bytes()
    }

    fn steps() -> Vec<CheckpointStepInputV1> {
        (0..CANONICAL_CHECKPOINT_STEPS_V1)
            .map(|index| CheckpointStepInputV1 {
                step_index: index as u32,
                transaction: vec![index as u8, 1, 2],
                transaction_effect: vec![index as u8, 3],
                pre_state_root: root(index as u64 + 1),
                post_state_root: root(index as u64 + 2),
                readonly_l1_values: vec![ReadonlyL1ValueV1 {
                    account: Pubkey::new_from_array([index as u8 + 1; 32]),
                    owner: Pubkey::new_from_array([index as u8 + 2; 32]),
                    lamports: index as u64 + 10,
                    data_hash: hashv(&[b"readonly", &[index as u8]]).to_bytes(),
                    observed_l1_slot: index as u64 + 900,
                }],
                settlement_effects: vec![vec![index as u8, 4]],
            })
            .collect()
    }

    fn artifact() -> CheckpointArtifactV1 {
        build_checkpoint_artifact_v1(Pubkey::new_unique(), 42, steps()).unwrap()
    }

    #[test]
    fn zk_checkpoint_state_root_matches_validator_encoding() {
        let account = StateAccountValueV1 {
            account: Pubkey::new_from_array([1; 32]),
            owner: Pubkey::new_from_array([2; 32]),
            lamports: 3,
            executable: true,
            rent_epoch: 4,
            data_hash: hashv(&[b"state-data"]).to_bytes(),
        };
        let zk_account = northstar_transaction_proof::checkpoint::StateAccountValueV1 {
            account: account.account.to_bytes(),
            owner: account.owner.to_bytes(),
            lamports: account.lamports,
            executable: account.executable,
            rent_epoch: account.rent_epoch,
            data_hash: account.data_hash,
        };
        assert_eq!(
            state_root_v1(&[account]).unwrap(),
            northstar_transaction_proof::checkpoint::state_root(&[zk_account]).unwrap(),
        );
    }

    #[test]
    fn zk_transaction_effect_matches_validator_encoding() {
        let witness = northstar_transaction_proof::fixture::build_replay_witness_v1().unwrap();
        let checkpoint =
            northstar_transaction_proof::checkpoint::verify_checkpoint_binding(&witness).unwrap();
        assert_eq!(
            transaction_effect_leaf_hash(
                witness.step_index as u32,
                &witness.transaction_bytes,
                &witness.checkpoint.transaction_effect,
            )
            .unwrap(),
            checkpoint.transaction_effect_commitment,
        );
    }

    #[test]
    fn sixteen_step_artifact_is_deterministic_and_authenticated() {
        let session = Pubkey::new_unique();
        let first = build_checkpoint_artifact_v1(session, 42, steps()).unwrap();
        let second = build_checkpoint_artifact_v1(session, 42, steps()).unwrap();
        let third = build_checkpoint_artifact_v1(session, 42, steps()).unwrap();
        let first_bytes = first.canonical_bytes().unwrap();
        assert_eq!(first_bytes, second.canonical_bytes().unwrap());
        assert_eq!(first_bytes, third.canonical_bytes().unwrap());
        assert_eq!(first.checkpoint.step_count, 16);
        assert_eq!(first.da.pages.len(), 16);
        assert!(first
            .da
            .pages
            .iter()
            .all(|page| page.verify_authentication_paths(&first.checkpoint)));
        assert_eq!(
            CheckpointArtifactV1::decode_verified(&first_bytes),
            Ok(first)
        );
    }

    #[test]
    fn commitments_are_distinct_and_field_roots_are_canonical() {
        let artifact = artifact();
        let roots = [
            artifact.checkpoint.trace_root,
            artifact.checkpoint.transaction_effect_root,
            artifact.checkpoint.readonly_l1_root,
            artifact.checkpoint.da_commitment,
            artifact.checkpoint.effect_commitment,
        ];
        assert!(roots.iter().all(|root| *root != [0; 32]));
        assert_eq!(
            roots.iter().copied().collect::<HashSet<_>>().len(),
            roots.len()
        );
        assert!(FrBytes::new(artifact.checkpoint.readonly_l1_root).is_ok());
        assert!(FrBytes::new(artifact.checkpoint.effect_commitment).is_ok());
    }

    #[test]
    fn missing_reordered_changed_and_duplicate_pages_fail_closed() {
        let artifact = artifact();

        let mut missing = artifact.clone();
        missing.da.pages.pop();
        assert!(missing.verify().is_err());

        let mut reordered = artifact.clone();
        reordered.da.pages.swap(0, 1);
        assert!(reordered.verify().is_err());

        let mut changed = artifact.clone();
        changed.da.pages[7].transaction[1] ^= 1;
        assert!(changed.verify().is_err());

        let mut duplicate = artifact;
        duplicate.da.pages[8] = duplicate.da.pages[7].clone();
        assert!(duplicate.verify().is_err());
    }

    #[test]
    fn duplicate_or_noncanonical_step_inputs_fail_closed() {
        let session = Pubkey::new_unique();
        let mut duplicate_transaction = steps();
        duplicate_transaction[1].transaction = duplicate_transaction[0].transaction.clone();
        assert_eq!(
            build_checkpoint_artifact_v1(session, 42, duplicate_transaction),
            Err(CheckpointArtifactError::InvalidTransaction)
        );

        let mut duplicate_readonly = steps();
        let duplicate_value = duplicate_readonly[0].readonly_l1_values[0];
        duplicate_readonly[0]
            .readonly_l1_values
            .push(duplicate_value);
        assert_eq!(
            build_checkpoint_artifact_v1(session, 42, duplicate_readonly),
            Err(CheckpointArtifactError::InvalidReadonlyL1Values)
        );

        let mut broken_chain = steps();
        broken_chain[8].pre_state_root = root(999);
        assert_eq!(
            build_checkpoint_artifact_v1(session, 42, broken_chain),
            Err(CheckpointArtifactError::InvalidStateRoot)
        );
    }

    #[test]
    fn trailing_bytes_and_changed_authentication_path_fail_closed() {
        let artifact = artifact();
        let mut encoded = artifact.canonical_bytes().unwrap();
        encoded.push(0);
        assert_eq!(
            CheckpointArtifactV1::decode_verified(&encoded),
            Err(CheckpointArtifactError::Encoding)
        );

        let mut changed = artifact;
        changed.da.pages[4].pre_state_path.siblings[0][0] ^= 1;
        assert!(!changed.da.pages[4].verify_authentication_paths(&changed.checkpoint));
        assert!(changed.verify().is_err());
    }

    #[test]
    fn empty_readonly_and_settlement_trees_have_nonzero_field_roots() {
        let mut inputs = steps();
        for step in &mut inputs {
            step.readonly_l1_values.clear();
            step.settlement_effects.clear();
        }
        let artifact = build_checkpoint_artifact_v1(Pubkey::new_unique(), 42, inputs).unwrap();
        assert_ne!(artifact.checkpoint.readonly_l1_root, [0; 32]);
        assert_ne!(artifact.checkpoint.effect_commitment, [0; 32]);
        assert!(FrBytes::new(artifact.checkpoint.readonly_l1_root).is_ok());
        assert!(FrBytes::new(artifact.checkpoint.effect_commitment).is_ok());
        assert!(artifact.verify().is_ok());
    }
}
