//! Public wire types for request membership in Northstar continuity-v2 checkpoints.
//!
//! A witness authenticates nothing without a root obtained independently from L1.
//! The encodings match Northstar's checkpoint-v1 state tree and continuity-v2 opening.
#![forbid(unsafe_code)]

use {
    serde::{Deserialize, Serialize},
    sha2::{Digest, Sha256},
};

pub type Hash32 = [u8; 32];
pub const MAX_STATE_ACCOUNTS: usize = 4096;
pub const MAX_REQUEST_BYTES: usize = 1024;
const TREE_DOMAIN: &[u8] = b"northstar-checkpoint-v1";

pub fn hash(parts: &[&[u8]]) -> Hash32 {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountValue {
    pub address: Hash32,
    pub owner: Hash32,
    pub lamports: u64,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data_hash: Hash32,
}

impl AccountValue {
    fn leaf(&self, index: u32) -> Hash32 {
        let mut encoded = Vec::with_capacity(117);
        encoded.extend_from_slice(&index.to_le_bytes());
        encoded.extend_from_slice(&self.address);
        encoded.extend_from_slice(&self.owner);
        encoded.extend_from_slice(&self.lamports.to_le_bytes());
        encoded.push(u8::from(self.executable));
        encoded.extend_from_slice(&self.rent_epoch.to_le_bytes());
        encoded.extend_from_slice(&self.data_hash);
        typed_hash(b"leaf", &encoded)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateOpening {
    pub session_context: Hash32,
    pub account_root: Hash32,
    pub origin_root: Hash32,
    pub ingress_count: u64,
    pub ingress_root: Hash32,
}

impl StateOpening {
    pub fn commitment(&self) -> Option<Hash32> {
        if [self.session_context, self.account_root, self.origin_root].contains(&[0; 32])
            || (self.ingress_count == 0) != (self.ingress_root == [0; 32])
        {
            return None;
        }
        let mut encoded = Vec::with_capacity(136);
        encoded.extend_from_slice(&self.session_context);
        encoded.extend_from_slice(&self.account_root);
        encoded.extend_from_slice(&self.origin_root);
        encoded.extend_from_slice(&self.ingress_count.to_le_bytes());
        encoded.extend_from_slice(&self.ingress_root);
        let mut root = hash(&[
            b"northstar-continuity-v2",
            &5u16.to_le_bytes(),
            b"state",
            &136u16.to_le_bytes(),
            &encoded,
        ]);
        root[0] &= 0x1f;
        Some(root)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipPath {
    pub leaf_index: u32,
    pub leaf_count: u32,
    pub siblings: Vec<Hash32>,
}

impl MembershipPath {
    pub fn verify(&self, account: &AccountValue, expected_root: &Hash32) -> bool {
        if self.leaf_count == 0
            || self.leaf_count as usize > MAX_STATE_ACCOUNTS
            || self.leaf_index >= self.leaf_count
        {
            return false;
        }
        let width = self.leaf_count.next_power_of_two();
        if self.siblings.len() != width.trailing_zeros() as usize {
            return false;
        }
        let mut current = account.leaf(self.leaf_index);
        let mut index = self.leaf_index;
        for (level, sibling) in self.siblings.iter().enumerate() {
            current = if index.is_multiple_of(2) {
                node_hash(level as u32, &current, sibling)
            } else {
                node_hash(level as u32, sibling, &current)
            };
            index /= 2;
        }
        root_hash(self.leaf_count, &current) == *expected_root
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestWitness {
    pub version: u8,
    pub er_slot: u64,
    pub opening: StateOpening,
    pub account: AccountValue,
    pub data: Vec<u8>,
    pub path: MembershipPath,
}

impl RequestWitness {
    pub fn verify(&self, session_context: &Hash32, accepted_state_root: &Hash32) -> bool {
        self.version == 1
            && self.er_slot > 0
            && self.data.len() <= MAX_REQUEST_BYTES
            && self.opening.session_context == *session_context
            && self.opening.commitment() == Some(*accepted_state_root)
            && self.account.data_hash == hash(&[&self.data])
            && !self.account.executable
            && self.path.verify(&self.account, &self.opening.account_root)
    }
}

fn typed_hash(label: &[u8], bytes: &[u8]) -> Hash32 {
    hash(&[
        TREE_DOMAIN,
        b"state",
        label,
        &(bytes.len() as u64).to_le_bytes(),
        bytes,
    ])
}

fn node_hash(level: u32, left: &Hash32, right: &Hash32) -> Hash32 {
    hash(&[
        TREE_DOMAIN,
        b"state",
        b"node",
        &level.to_le_bytes(),
        left,
        right,
    ])
}

fn root_hash(count: u32, inner: &Hash32) -> Hash32 {
    let mut root = hash(&[TREE_DOMAIN, b"state", b"root", &count.to_le_bytes(), inner]);
    root[0] &= 0x1f;
    root
}

fn layers(accounts: &[AccountValue]) -> Option<Vec<Vec<Hash32>>> {
    if accounts.len() > MAX_STATE_ACCOUNTS
        || accounts
            .windows(2)
            .any(|pair| pair[0].address >= pair[1].address)
        || accounts.iter().any(|account| account.data_hash == [0; 32])
    {
        return None;
    }
    let width = accounts.len().max(1).next_power_of_two();
    let mut leaves: Vec<_> = accounts
        .iter()
        .enumerate()
        .map(|(i, a)| a.leaf(i as u32))
        .collect();
    for index in leaves.len()..width {
        leaves.push(typed_hash(b"empty-leaf", &(index as u32).to_le_bytes()));
    }
    let mut layers = vec![leaves];
    let mut level = 0;
    while layers.last()?.len() > 1 {
        let next = layers
            .last()?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| node_hash(level, &pair[0], &pair[1]))
            .collect();
        layers.push(next);
        level += 1;
    }
    Some(layers)
}

pub fn account_root(accounts: &[AccountValue]) -> Option<Hash32> {
    Some(root_hash(
        accounts.len() as u32,
        layers(accounts)?.last()?.first()?,
    ))
}

pub fn membership_path(accounts: &[AccountValue], index: usize) -> Option<MembershipPath> {
    if index >= accounts.len() {
        return None;
    }
    let layers = layers(accounts)?;
    let mut current = index;
    let mut siblings = Vec::with_capacity(layers.len() - 1);
    for layer in layers.iter().take(layers.len() - 1) {
        siblings.push(layer[current ^ 1]);
        current /= 2;
    }
    Some(MembershipPath {
        leaf_index: index as u32,
        leaf_count: accounts.len() as u32,
        siblings,
    })
}
