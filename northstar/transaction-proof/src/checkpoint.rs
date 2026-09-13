use {
    crate::{AccountWitnessV1, ReplayError, ReplayWitnessV1},
    borsh::{BorshDeserialize, BorshSerialize},
    sha2::{Digest, Sha256},
    std::collections::{BTreeMap, BTreeSet},
};

pub const CHECKPOINT_FORMAT_VERSION_V1: u8 = 1;
pub const CANONICAL_CHECKPOINT_STEPS_V1: u32 = 16;
const CHECKPOINT_HASH_DOMAIN_V1: &[u8] = b"northstar-checkpoint-v1";

type Hash32 = [u8; 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, BorshDeserialize, BorshSerialize)]
pub struct StateAccountValueV1 {
    pub account: Hash32,
    pub owner: Hash32,
    pub lamports: u64,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data_hash: Hash32,
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
    pub transaction_hash: Hash32,
    pub executed_units: u64,
    pub account_effects: Vec<CheckpointAccountEffectV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, BorshDeserialize, BorshSerialize)]
pub struct ReadonlyL1ValueV1 {
    pub account: Hash32,
    pub owner: Hash32,
    pub lamports: u64,
    pub data_hash: Hash32,
    pub observed_l1_slot: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct MerkleAuthenticationPathV1 {
    pub leaf_index: u32,
    pub leaf_count: u32,
    pub siblings: Vec<Hash32>,
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

#[derive(Debug, Default, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct CheckpointBindingV1 {
    pub pre_state_accounts: Vec<StateAccountValueV1>,
    pub post_state_accounts: Vec<StateAccountValueV1>,
    pub transaction_effect: Vec<u8>,
    pub transaction_effect_path: MerkleAuthenticationPathV1,
    pub checkpoint_transaction_effect_root: Hash32,
    pub readonly_l1_values: Vec<AuthenticatedReadonlyL1ValueV1>,
    pub readonly_l1_root: Hash32,
    pub settlement_effects: Vec<AuthenticatedSettlementEffectV1>,
    pub settlement_effect_root: Hash32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointPublicInputsV1 {
    pub pre_state_root: Hash32,
    pub post_state_root: Hash32,
    pub transaction_effect_commitment: Hash32,
    pub readonly_l1_root: Hash32,
    pub settlement_effect_root: Hash32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TreeKind {
    State,
    TransactionEffect,
    ReadonlyL1,
    SettlementEffect,
}

impl TreeKind {
    fn tag(self) -> &'static [u8] {
        match self {
            Self::State => b"state",
            Self::TransactionEffect => b"transaction-effect",
            Self::ReadonlyL1 => b"readonly-l1",
            Self::SettlementEffect => b"settlement-effect",
        }
    }

    fn root_is_field(self) -> bool {
        matches!(
            self,
            Self::State | Self::ReadonlyL1 | Self::SettlementEffect
        )
    }
}

#[derive(BorshSerialize)]
struct StateAccountLeafV1<'a> {
    index: u32,
    account: &'a StateAccountValueV1,
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

pub fn verify_checkpoint_binding(
    witness: &ReplayWitnessV1,
) -> Result<CheckpointPublicInputsV1, ReplayError> {
    let binding = &witness.checkpoint;
    let step_index = u32::try_from(witness.step_index).map_err(|_| ReplayError::Commitment)?;
    let step_count = binding.transaction_effect_path.leaf_count;
    if !(1..=CANONICAL_CHECKPOINT_STEPS_V1).contains(&step_count)
        || step_index >= step_count
        || !state_accounts_are_canonical(&binding.pre_state_accounts)
        || !state_accounts_are_canonical(&binding.post_state_accounts)
    {
        return Err(ReplayError::Commitment);
    }

    let pre_state_root = state_root(&binding.pre_state_accounts)?;
    let post_state_root = state_root(&binding.post_state_accounts)?;
    let transaction_effect =
        borsh::from_slice::<CheckpointTransactionEffectV1>(&binding.transaction_effect)
            .map_err(|_| ReplayError::Commitment)?;
    if borsh::to_vec(&transaction_effect).map_err(|_| ReplayError::Commitment)?
        != binding.transaction_effect
        || transaction_effect.version != CHECKPOINT_FORMAT_VERSION_V1
        || transaction_effect.step_index != step_index
        || transaction_effect.transaction_hash != sha256(&witness.transaction_bytes)
        || transaction_effect.executed_units != witness.result.executed_units
    {
        return Err(ReplayError::Commitment);
    }

    let mut expected_effects = witness
        .post_accounts
        .iter()
        .map(|account| CheckpointAccountEffectV1 {
            step_index,
            value: state_account(account),
            deleted: account.lamports == 0,
        })
        .collect::<Vec<_>>();
    expected_effects.sort_unstable_by_key(|effect| effect.value.account);
    if transaction_effect.account_effects != expected_effects {
        return Err(ReplayError::Commitment);
    }
    validate_state_transition(
        &binding.pre_state_accounts,
        &binding.post_state_accounts,
        &transaction_effect.account_effects,
        &witness.pre_accounts,
    )?;

    let transaction_effect_commitment = transaction_effect_leaf_hash(
        step_index,
        &witness.transaction_bytes,
        &binding.transaction_effect,
    )?;
    if binding.transaction_effect_path.leaf_index != step_index
        || !verify_path(
            TreeKind::TransactionEffect,
            &binding.transaction_effect_path,
            transaction_effect_commitment,
            binding.checkpoint_transaction_effect_root,
        )
    {
        return Err(ReplayError::Commitment);
    }

    validate_readonly_values(witness, step_index)?;
    validate_settlement_effects(witness, step_index, &transaction_effect.account_effects)?;

    Ok(CheckpointPublicInputsV1 {
        pre_state_root,
        post_state_root,
        transaction_effect_commitment,
        readonly_l1_root: binding.readonly_l1_root,
        settlement_effect_root: binding.settlement_effect_root,
    })
}

fn validate_state_transition(
    pre: &[StateAccountValueV1],
    post: &[StateAccountValueV1],
    effects: &[CheckpointAccountEffectV1],
    execution_pre: &[AccountWitnessV1],
) -> Result<(), ReplayError> {
    let pre_map = pre
        .iter()
        .map(|value| (value.account, *value))
        .collect::<BTreeMap<_, _>>();
    let post_map = post
        .iter()
        .map(|value| (value.account, *value))
        .collect::<BTreeMap<_, _>>();
    let effect_keys = effects
        .iter()
        .map(|effect| effect.value.account)
        .collect::<BTreeSet<_>>();
    if effect_keys.len() != effects.len() {
        return Err(ReplayError::Commitment);
    }
    for account in execution_pre {
        if pre_map.get(&account.key) != Some(&state_account(account)) {
            return Err(ReplayError::Commitment);
        }
    }
    for key in pre_map
        .keys()
        .chain(post_map.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        if pre_map.get(&key) != post_map.get(&key) && !effect_keys.contains(&key) {
            return Err(ReplayError::Commitment);
        }
    }
    for effect in effects {
        if effect.deleted {
            if effect.value.lamports != 0 || post_map.contains_key(&effect.value.account) {
                return Err(ReplayError::Commitment);
            }
        } else if post_map.get(&effect.value.account) != Some(&effect.value) {
            return Err(ReplayError::Commitment);
        }
    }
    Ok(())
}

fn validate_readonly_values(witness: &ReplayWitnessV1, step_index: u32) -> Result<(), ReplayError> {
    let binding = &witness.checkpoint;
    let mut expected = witness
        .readonly_accounts
        .iter()
        .map(|account| ReadonlyL1ValueV1 {
            account: account.key,
            owner: account.owner,
            lamports: account.lamports,
            data_hash: sha256(&account.data),
            observed_l1_slot: witness.runtime.slot,
        })
        .collect::<Vec<_>>();
    expected.sort_unstable();
    if binding.readonly_l1_values.len() != expected.len() {
        return Err(ReplayError::Commitment);
    }
    if expected.is_empty() {
        return (binding.readonly_l1_root == empty_root(TreeKind::ReadonlyL1))
            .then_some(())
            .ok_or(ReplayError::Commitment);
    }
    for (value_index, (authenticated, expected)) in binding
        .readonly_l1_values
        .iter()
        .zip(expected.iter())
        .enumerate()
    {
        let value_index = u32::try_from(value_index).map_err(|_| ReplayError::Commitment)?;
        if authenticated.value != *expected
            || authenticated.value_index != value_index
            || authenticated.path.leaf_index != authenticated.global_index
            || !verify_path(
                TreeKind::ReadonlyL1,
                &authenticated.path,
                readonly_leaf_hash(
                    authenticated.global_index,
                    step_index,
                    value_index,
                    expected,
                )?,
                binding.readonly_l1_root,
            )
        {
            return Err(ReplayError::Commitment);
        }
    }
    Ok(())
}

fn validate_settlement_effects(
    witness: &ReplayWitnessV1,
    step_index: u32,
    effects: &[CheckpointAccountEffectV1],
) -> Result<(), ReplayError> {
    let binding = &witness.checkpoint;
    if binding.settlement_effects.len() != effects.len() {
        return Err(ReplayError::Commitment);
    }
    if effects.is_empty() {
        return (binding.settlement_effect_root == empty_root(TreeKind::SettlementEffect))
            .then_some(())
            .ok_or(ReplayError::Commitment);
    }
    for (effect_index, (authenticated, effect)) in binding
        .settlement_effects
        .iter()
        .zip(effects.iter())
        .enumerate()
    {
        let effect_index = u32::try_from(effect_index).map_err(|_| ReplayError::Commitment)?;
        let encoded = borsh::to_vec(effect).map_err(|_| ReplayError::Commitment)?;
        if authenticated.effect != encoded
            || authenticated.effect_index != effect_index
            || authenticated.path.leaf_index != authenticated.global_index
            || !verify_path(
                TreeKind::SettlementEffect,
                &authenticated.path,
                settlement_leaf_hash(
                    authenticated.global_index,
                    step_index,
                    effect_index,
                    &encoded,
                )?,
                binding.settlement_effect_root,
            )
        {
            return Err(ReplayError::Commitment);
        }
    }
    Ok(())
}

fn state_account(account: &AccountWitnessV1) -> StateAccountValueV1 {
    StateAccountValueV1 {
        account: account.key,
        owner: account.owner,
        lamports: account.lamports,
        executable: account.executable,
        rent_epoch: account.rent_epoch,
        data_hash: sha256(&account.data),
    }
}

fn state_accounts_are_canonical(accounts: &[StateAccountValueV1]) -> bool {
    !accounts.iter().any(|account| account.data_hash == [0; 32])
        && !accounts.windows(2).any(|pair| pair[0] >= pair[1])
}

pub fn state_root(accounts: &[StateAccountValueV1]) -> Result<Hash32, ReplayError> {
    if !state_accounts_are_canonical(accounts) {
        return Err(ReplayError::Commitment);
    }
    let leaves = accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            let index = u32::try_from(index).map_err(|_| ReplayError::Commitment)?;
            typed_hash(
                TreeKind::State,
                b"leaf",
                &borsh::to_vec(&StateAccountLeafV1 { index, account })
                    .map_err(|_| ReplayError::Commitment)?,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MerkleTree::new(TreeKind::State, leaves)?.root())
}

fn transaction_effect_leaf_hash(
    index: u32,
    transaction: &[u8],
    transaction_effect: &[u8],
) -> Result<Hash32, ReplayError> {
    let mut hash = typed_hash(
        TreeKind::TransactionEffect,
        b"leaf",
        &borsh::to_vec(&TransactionEffectLeafV1 {
            index,
            transaction,
            transaction_effect,
        })
        .map_err(|_| ReplayError::Commitment)?,
    )?;
    hash[0] &= 0x1f;
    Ok(hash)
}

fn readonly_leaf_hash(
    global_index: u32,
    step_index: u32,
    value_index: u32,
    value: &ReadonlyL1ValueV1,
) -> Result<Hash32, ReplayError> {
    typed_hash(
        TreeKind::ReadonlyL1,
        b"leaf",
        &borsh::to_vec(&ReadonlyL1LeafV1 {
            global_index,
            step_index,
            value_index,
            value,
        })
        .map_err(|_| ReplayError::Commitment)?,
    )
}

fn settlement_leaf_hash(
    global_index: u32,
    step_index: u32,
    effect_index: u32,
    effect: &[u8],
) -> Result<Hash32, ReplayError> {
    typed_hash(
        TreeKind::SettlementEffect,
        b"leaf",
        &borsh::to_vec(&SettlementEffectLeafV1 {
            global_index,
            step_index,
            effect_index,
            effect,
        })
        .map_err(|_| ReplayError::Commitment)?,
    )
}

fn verify_path(
    kind: TreeKind,
    path: &MerkleAuthenticationPathV1,
    leaf: Hash32,
    expected_root: Hash32,
) -> bool {
    if path.leaf_count == 0 || path.leaf_index >= path.leaf_count {
        return false;
    }
    let width = (path.leaf_count as usize).next_power_of_two();
    if path.siblings.len() != width.trailing_zeros() as usize {
        return false;
    }
    let mut index = path.leaf_index as usize;
    let mut current = leaf;
    for (level, sibling) in path.siblings.iter().enumerate() {
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
    root_hash(kind, path.leaf_count, &current) == expected_root
}

fn sha256(bytes: &[u8]) -> Hash32 {
    Sha256::digest(bytes).into()
}

fn hashv(parts: &[&[u8]]) -> Hash32 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn typed_hash(kind: TreeKind, label: &[u8], bytes: &[u8]) -> Result<Hash32, ReplayError> {
    let length = u64::try_from(bytes.len())
        .map_err(|_| ReplayError::Commitment)?
        .to_le_bytes();
    Ok(hashv(&[
        CHECKPOINT_HASH_DOMAIN_V1,
        kind.tag(),
        label,
        &length,
        bytes,
    ]))
}

fn empty_leaf_hash(kind: TreeKind, index: usize) -> Result<Hash32, ReplayError> {
    let index = u32::try_from(index).map_err(|_| ReplayError::Commitment)?;
    typed_hash(kind, b"empty-leaf", &index.to_le_bytes())
}

fn node_hash(kind: TreeKind, level: u32, left: &Hash32, right: &Hash32) -> Hash32 {
    hashv(&[
        CHECKPOINT_HASH_DOMAIN_V1,
        kind.tag(),
        b"node",
        &level.to_le_bytes(),
        left,
        right,
    ])
}

fn root_hash(kind: TreeKind, leaf_count: u32, inner: &Hash32) -> Hash32 {
    let mut root = hashv(&[
        CHECKPOINT_HASH_DOMAIN_V1,
        kind.tag(),
        b"root",
        &leaf_count.to_le_bytes(),
        inner,
    ]);
    if kind.root_is_field() {
        root[0] &= 0x1f;
    }
    root
}

fn empty_root(kind: TreeKind) -> Hash32 {
    MerkleTree::new(kind, Vec::new())
        .expect("empty checkpoint tree is valid")
        .root()
}

struct MerkleTree {
    kind: TreeKind,
    leaf_count: u32,
    layers: Vec<Vec<Hash32>>,
}

impl MerkleTree {
    fn new(kind: TreeKind, leaves: Vec<Hash32>) -> Result<Self, ReplayError> {
        let leaf_count = u32::try_from(leaves.len()).map_err(|_| ReplayError::Commitment)?;
        let width = leaves.len().max(1).next_power_of_two();
        let mut leaf_layer = leaves;
        for index in leaf_layer.len()..width {
            leaf_layer.push(empty_leaf_hash(kind, index)?);
        }
        let mut layers = vec![leaf_layer];
        let mut level = 0u32;
        while layers.last().expect("tree has leaf layer").len() > 1 {
            let next = layers
                .last()
                .expect("tree has previous layer")
                .chunks_exact(2)
                .map(|children| node_hash(kind, level, &children[0], &children[1]))
                .collect();
            layers.push(next);
            level = level.checked_add(1).ok_or(ReplayError::Commitment)?;
        }
        Ok(Self {
            kind,
            leaf_count,
            layers,
        })
    }

    fn root(&self) -> Hash32 {
        root_hash(
            self.kind,
            self.leaf_count,
            &self.layers.last().expect("tree has root layer")[0],
        )
    }

    #[cfg(feature = "host")]
    fn path(&self, leaf_index: usize) -> Result<MerkleAuthenticationPathV1, ReplayError> {
        if leaf_index >= self.leaf_count as usize {
            return Err(ReplayError::Commitment);
        }
        let mut index = leaf_index;
        let mut siblings = Vec::with_capacity(self.layers.len().saturating_sub(1));
        for layer in self.layers.iter().take(self.layers.len().saturating_sub(1)) {
            siblings.push(layer[index ^ 1]);
            index /= 2;
        }
        Ok(MerkleAuthenticationPathV1 {
            leaf_index: u32::try_from(leaf_index).map_err(|_| ReplayError::Commitment)?,
            leaf_count: self.leaf_count,
            siblings,
        })
    }
}

#[cfg(feature = "host")]
pub fn fixture_checkpoint_binding(
    witness: &ReplayWitnessV1,
) -> Result<CheckpointBindingV1, ReplayError> {
    fixture_checkpoint_binding_for_steps(witness, CANONICAL_CHECKPOINT_STEPS_V1)
}

fn fixture_checkpoint_binding_for_steps(
    witness: &ReplayWitnessV1,
    step_count: u32,
) -> Result<CheckpointBindingV1, ReplayError> {
    let step_index = u32::try_from(witness.step_index).map_err(|_| ReplayError::Commitment)?;
    let mut pre_state_accounts = witness
        .pre_accounts
        .iter()
        .map(state_account)
        .collect::<Vec<_>>();
    pre_state_accounts.sort_unstable();
    let mut post_state_accounts = witness
        .post_accounts
        .iter()
        .map(state_account)
        .collect::<Vec<_>>();
    post_state_accounts.sort_unstable();

    let mut account_effects = witness
        .post_accounts
        .iter()
        .map(|account| CheckpointAccountEffectV1 {
            step_index,
            value: state_account(account),
            deleted: account.lamports == 0,
        })
        .collect::<Vec<_>>();
    account_effects.sort_unstable_by_key(|effect| effect.value.account);
    let transaction_effect = borsh::to_vec(&CheckpointTransactionEffectV1 {
        version: CHECKPOINT_FORMAT_VERSION_V1,
        step_index,
        transaction_hash: sha256(&witness.transaction_bytes),
        executed_units: witness.result.executed_units,
        account_effects: account_effects.clone(),
    })
    .map_err(|_| ReplayError::Commitment)?;

    let transaction_leaves = (0..step_count)
        .map(|index| {
            if index == step_index {
                transaction_effect_leaf_hash(index, &witness.transaction_bytes, &transaction_effect)
            } else {
                transaction_effect_leaf_hash(index, &[0x80, index as u8], &[0x40, index as u8])
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let transaction_tree = MerkleTree::new(TreeKind::TransactionEffect, transaction_leaves)?;

    let mut readonly = witness
        .readonly_accounts
        .iter()
        .map(|account| ReadonlyL1ValueV1 {
            account: account.key,
            owner: account.owner,
            lamports: account.lamports,
            data_hash: sha256(&account.data),
            observed_l1_slot: witness.runtime.slot,
        })
        .collect::<Vec<_>>();
    readonly.sort_unstable();
    let readonly_leaves = readonly
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let index = u32::try_from(index).map_err(|_| ReplayError::Commitment)?;
            readonly_leaf_hash(index, step_index, index, value)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let readonly_tree = MerkleTree::new(TreeKind::ReadonlyL1, readonly_leaves)?;
    let readonly_l1_values = readonly
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let index = u32::try_from(index).map_err(|_| ReplayError::Commitment)?;
            Ok(AuthenticatedReadonlyL1ValueV1 {
                global_index: index,
                value_index: index,
                value,
                path: readonly_tree.path(index as usize)?,
            })
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;

    let effect_bytes = account_effects
        .iter()
        .map(|effect| borsh::to_vec(effect).map_err(|_| ReplayError::Commitment))
        .collect::<Result<Vec<_>, _>>()?;
    let settlement_leaves = effect_bytes
        .iter()
        .enumerate()
        .map(|(index, effect)| {
            let index = u32::try_from(index).map_err(|_| ReplayError::Commitment)?;
            settlement_leaf_hash(index, step_index, index, effect)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let settlement_tree = MerkleTree::new(TreeKind::SettlementEffect, settlement_leaves)?;
    let settlement_effects = effect_bytes
        .into_iter()
        .enumerate()
        .map(|(index, effect)| {
            let index = u32::try_from(index).map_err(|_| ReplayError::Commitment)?;
            Ok(AuthenticatedSettlementEffectV1 {
                global_index: index,
                effect_index: index,
                effect,
                path: settlement_tree.path(index as usize)?,
            })
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;

    Ok(CheckpointBindingV1 {
        pre_state_accounts,
        post_state_accounts,
        transaction_effect,
        transaction_effect_path: transaction_tree.path(step_index as usize)?,
        checkpoint_transaction_effect_root: transaction_tree.root(),
        readonly_l1_values,
        readonly_l1_root: readonly_tree.root(),
        settlement_effects,
        settlement_effect_root: settlement_tree.root(),
    })
}

#[cfg(all(test, feature = "host"))]
mod partial_tests {
    use super::*;

    #[test]
    fn replay_authenticates_nonempty_partial_checkpoint_paths() {
        let reference = crate::fixture::build_replay_witness_v1().unwrap();
        for count in [1, 2, 3, 15, 16] {
            let mut witness = reference.clone();
            witness.step_index = u64::from(count - 1);
            witness.checkpoint = fixture_checkpoint_binding_for_steps(&witness, count).unwrap();
            crate::set_trace_hash(&mut witness);
            crate::replay(&witness).unwrap();
            for invalid_count in [0, count - 1, 17] {
                let mut changed = witness.clone();
                changed.checkpoint.transaction_effect_path.leaf_count = invalid_count;
                assert!(crate::replay(&changed).is_err());
            }
        }
    }
}
