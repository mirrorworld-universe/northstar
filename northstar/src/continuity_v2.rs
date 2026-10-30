//! Committed state for an already-authenticated L1 prefix and captured SVM results.
//! This model checks transition structure, not L1 account authenticity or SVM execution.

use {
    crate::checkpoint::{state_root_v1, StateAccountValueV1},
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_zk_types::continuity::{
        commitment, ContinuityError, IngressPayloadV2, IngressRecordV2, SessionIdentityV2,
        StateRootOpeningV2,
    },
    solana_account::{AccountSharedData, ReadableAccount},
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hashv,
    std::collections::BTreeMap,
    thiserror::Error,
};

pub const MAX_COMMITTED_ACCOUNTS_V2: usize = 4_096;
pub const MAX_COMMITTED_STATE_BYTES_V2: usize = 1_048_576;
pub type Hash32 = [u8; 32];

pub fn hash_parts(parts: &[&[u8]]) -> Hash32 {
    hashv(parts).to_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CommittedAccountV2 {
    pub owner: Hash32,
    pub lamports: u64,
    pub data: Vec<u8>,
    pub executable: bool,
    pub rent_epoch: u64,
}

impl CommittedAccountV2 {
    pub fn from_account(account: &AccountSharedData) -> Self {
        Self {
            owner: account.owner().to_bytes(),
            lamports: account.lamports(),
            data: account.data().to_vec(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
        }
    }

    pub fn to_account(&self) -> AccountSharedData {
        solana_account::Account {
            owner: Pubkey::new_from_array(self.owner),
            lamports: self.lamports,
            data: self.data.clone(),
            executable: self.executable,
            rent_epoch: self.rent_epoch,
        }
        .into()
    }

    pub fn empty() -> Self {
        Self {
            owner: [0; 32],
            lamports: 0,
            data: vec![],
            executable: false,
            rent_epoch: 0,
        }
    }

    fn state_value(&self, account: Hash32) -> StateAccountValueV1 {
        StateAccountValueV1 {
            account: Pubkey::new_from_array(account),
            owner: Pubkey::new_from_array(self.owner),
            lamports: self.lamports,
            executable: self.executable,
            rent_epoch: self.rent_epoch,
            data_hash: hash_parts(&[&self.data]),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum AccountOriginV2 {
    ErCreated,
    Delegated { record: Hash32, generation: u64 },
    SolReceipt { receipt: Hash32 },
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CommittedStateV2 {
    identity: SessionIdentityV2,
    accounts: BTreeMap<Hash32, CommittedAccountV2>,
    origins: BTreeMap<Hash32, AccountOriginV2>,
    credited: BTreeMap<Hash32, u64>,
    deposit_origins: BTreeMap<Hash32, (u64, u64)>,
    withdrawn: BTreeMap<Hash32, u64>,
    frozen: BTreeMap<Hash32, Hash32>,
    ingress_count: u64,
    ingress_root: Hash32,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum StateErrorV2 {
    #[error("invalid versioned checkpoint commitment: {0:?}")]
    Commitment(ContinuityError),
    #[error("account source conflicts with committed ER state")]
    OriginConflict,
    #[error("execution prestate differs from committed ER state")]
    InvalidPrestate,
    #[error("account bytes differ from the authenticated L1 input")]
    InvalidAccount,
    #[error("deposit progression is not contiguous")]
    InvalidCredit,
    #[error("token ingress requires authenticated SVM execution")]
    TokenExecutionRequired,
    #[error("execution writes a frozen account")]
    Frozen,
    #[error("execution accounts are not canonical")]
    InvalidAccounts,
    #[error("committed account limit exceeded")]
    Capacity,
}

impl From<ContinuityError> for StateErrorV2 {
    fn from(value: ContinuityError) -> Self {
        Self::Commitment(value)
    }
}

impl CommittedStateV2 {
    pub fn new(identity: SessionIdentityV2) -> Result<Self, StateErrorV2> {
        identity.commitment(hash_parts)?;
        Ok(Self {
            identity,
            accounts: BTreeMap::new(),
            origins: BTreeMap::new(),
            credited: BTreeMap::new(),
            deposit_origins: BTreeMap::new(),
            withdrawn: BTreeMap::new(),
            frozen: BTreeMap::new(),
            ingress_count: 0,
            ingress_root: [0; 32],
        })
    }

    pub fn identity(&self) -> &SessionIdentityV2 {
        &self.identity
    }
    pub fn accounts(&self) -> &BTreeMap<Hash32, CommittedAccountV2> {
        &self.accounts
    }
    pub fn origin(&self, account: Hash32) -> Option<AccountOriginV2> {
        self.origins.get(&account).copied()
    }
    pub fn is_frozen(&self, account: Hash32) -> bool {
        self.frozen.contains_key(&account)
    }

    pub fn ingress_count(&self) -> u64 {
        self.ingress_count
    }
    pub fn ingress_root(&self) -> Hash32 {
        self.ingress_root
    }

    fn validate_capacity(&self) -> Result<(), StateErrorV2> {
        if self.accounts.len() > MAX_COMMITTED_ACCOUNTS_V2
            || borsh::object_length(self).map_err(|_| StateErrorV2::InvalidAccounts)?
                > MAX_COMMITTED_STATE_BYTES_V2
        {
            return Err(StateErrorV2::Capacity);
        }
        Ok(())
    }

    pub fn opening(&self) -> Result<StateRootOpeningV2, StateErrorV2> {
        self.validate_capacity()?;
        let values = self
            .accounts
            .iter()
            .map(|(key, value)| value.state_value(*key))
            .collect::<Vec<_>>();
        let account_root = state_root_v1(&values).map_err(|_| StateErrorV2::InvalidAccounts)?;
        let metadata = borsh::to_vec(&(
            &self.origins,
            &self.credited,
            &self.deposit_origins,
            &self.withdrawn,
            &self.frozen,
        ))
        .map_err(|_| StateErrorV2::Commitment(ContinuityError::Encoding))?;
        let origin_root = commitment(b"origins", &hash_parts(&[&metadata]), hash_parts)?;
        Ok(StateRootOpeningV2 {
            session_context: self.identity.commitment(hash_parts)?,
            account_root,
            origin_root,
            ingress_count: self.ingress_count,
            ingress_root: self.ingress_root,
        })
    }

    pub fn withdrawn(&self, account: Hash32) -> u64 {
        self.withdrawn.get(&account).copied().unwrap_or_default()
    }

    pub fn apply_settlement_effects(&mut self, effects: &[Vec<u8>]) -> Result<(), StateErrorV2> {
        let mut next = self.clone();
        for encoded in effects {
            let effect = SettlementEffectV2::try_from_slice(encoded)
                .map_err(|_| StateErrorV2::InvalidAccounts)?;
            let (account, amount, cumulative) = match effect {
                SettlementEffectV2::SolWithdrawal {
                    source,
                    amount,
                    cumulative_withdrawn,
                    ..
                } => {
                    let Some(AccountOriginV2::SolReceipt { receipt }) = next.origins.get(&source)
                    else {
                        return Err(StateErrorV2::OriginConflict);
                    };
                    if cumulative_withdrawn
                        > next.credited.get(receipt).copied().unwrap_or_default()
                    {
                        return Err(StateErrorV2::InvalidCredit);
                    }
                    (source, amount, cumulative_withdrawn)
                }
                SettlementEffectV2::TokenWithdrawal {
                    account,
                    amount,
                    cumulative_withdrawn,
                    ..
                } => {
                    if !matches!(
                        next.origins.get(&account),
                        Some(AccountOriginV2::Delegated { .. })
                    ) {
                        return Err(StateErrorV2::OriginConflict);
                    }
                    (account, amount, cumulative_withdrawn)
                }
            };
            if amount == 0 || next.withdrawn(account).checked_add(amount) != Some(cumulative) {
                return Err(StateErrorV2::InvalidCredit);
            }
            next.withdrawn.insert(account, cumulative);
        }
        next.validate_capacity()?;
        *self = next;
        Ok(())
    }

    pub fn root(&self) -> Result<Hash32, StateErrorV2> {
        Ok(self.opening()?.commitment(hash_parts)?)
    }

    pub fn apply_ingress(
        &mut self,
        record: &IngressRecordV2,
        account_data: &[u8],
    ) -> Result<(), StateErrorV2> {
        record.validate_next(
            self.identity.commitment(hash_parts)?,
            self.ingress_count,
            self.ingress_root,
            hash_parts,
        )?;
        let mut next = self.clone();
        match record.payload {
            IngressPayloadV2::Delegation {
                account,
                record: source,
                owner,
                lamports,
                data_len,
                data_hash,
                executable,
                rent_epoch,
            } => {
                if next.accounts.contains_key(&account) {
                    return Err(StateErrorV2::OriginConflict);
                }
                if usize::try_from(data_len).ok() != Some(account_data.len())
                    || hash_parts(&[account_data]) != data_hash
                {
                    return Err(StateErrorV2::InvalidAccount);
                }
                next.accounts.insert(
                    account,
                    CommittedAccountV2 {
                        owner,
                        lamports,
                        data: account_data.to_vec(),
                        executable,
                        rent_epoch,
                    },
                );
                next.origins.insert(
                    account,
                    AccountOriginV2::Delegated {
                        record: source,
                        generation: record.sequence,
                    },
                );
            }
            IngressPayloadV2::SolDeposit {
                recipient,
                receipt,
                amount,
                cumulative_deposited,
            } => {
                let previous = next.credited.get(&receipt).copied().unwrap_or_default();
                if previous.checked_add(amount) != Some(cumulative_deposited) {
                    return Err(StateErrorV2::InvalidCredit);
                }
                match next.origins.get(&recipient) {
                    None | Some(AccountOriginV2::ErCreated) => {}
                    Some(AccountOriginV2::SolReceipt { receipt: existing })
                        if *existing == receipt => {}
                    _ => return Err(StateErrorV2::OriginConflict),
                }
                let value = next
                    .accounts
                    .entry(recipient)
                    .or_insert_with(CommittedAccountV2::empty);
                if value.owner != [0; 32] || !value.data.is_empty() || value.executable {
                    return Err(StateErrorV2::InvalidAccount);
                }
                value.lamports = value
                    .lamports
                    .checked_add(amount)
                    .ok_or(StateErrorV2::InvalidCredit)?;
                next.origins
                    .insert(recipient, AccountOriginV2::SolReceipt { receipt });
                next.credited.insert(receipt, cumulative_deposited);
            }
            IngressPayloadV2::TokenDeposit { .. } => {
                return Err(StateErrorV2::TokenExecutionRequired)
            }
            IngressPayloadV2::Freeze {
                account, request, ..
            } => {
                if !matches!(
                    next.origins.get(&account),
                    Some(AccountOriginV2::Delegated { .. })
                ) {
                    return Err(StateErrorV2::OriginConflict);
                }
                if next.frozen.insert(account, request).is_some() {
                    return Err(StateErrorV2::OriginConflict);
                }
            }
            IngressPayloadV2::Release { account, request } => {
                if next.frozen.get(&account) != Some(&request) {
                    return Err(StateErrorV2::OriginConflict);
                }
                next.frozen.remove(&account);
                next.accounts.remove(&account);
                next.origins.remove(&account);
            }
        }
        next.validate_capacity()?;
        next.ingress_count = record.sequence;
        next.ingress_root = record.root;
        *self = next;
        Ok(())
    }

    pub fn apply_token_ingress_execution(
        &mut self,
        record: &IngressRecordV2,
        pre: &[(Hash32, CommittedAccountV2)],
        post: &[(Hash32, CommittedAccountV2)],
    ) -> Result<(), StateErrorV2> {
        record.validate_next(
            self.identity.commitment(hash_parts)?,
            self.ingress_count,
            self.ingress_root,
            hash_parts,
        )?;
        let IngressPayloadV2::TokenDeposit {
            bridge,
            session_bridge,
            account,
            receipt,
            mint,
            origin_deposited,
            amount,
            cumulative_deposited,
            ..
        } = record.payload
        else {
            return Err(StateErrorV2::InvalidCredit);
        };
        let Some(AccountOriginV2::Delegated { generation, .. }) = self.origin(account) else {
            return Err(StateErrorV2::OriginConflict);
        };
        let value = self
            .accounts
            .get(&account)
            .ok_or(StateErrorV2::InvalidAccount)?;
        let before = northstar_token_bridge::state::ErTokenAccount::try_from_slice(&value.data)
            .map_err(|_| StateErrorV2::InvalidAccount)?;
        let after_value = post
            .iter()
            .find(|(key, _)| *key == account)
            .map(|(_, value)| value)
            .ok_or(StateErrorV2::InvalidAccount)?;
        let after =
            northstar_token_bridge::state::ErTokenAccount::try_from_slice(&after_value.data)
                .map_err(|_| StateErrorV2::InvalidAccount)?;
        let mut expected = before;
        expected.amount = expected
            .amount
            .checked_add(amount)
            .ok_or(StateErrorV2::InvalidCredit)?;
        if !before.is_valid()
            || before.session_bridge != session_bridge
            || before.mint != mint
            || value.owner != bridge
            || after_value.owner != bridge
            || after != expected
        {
            return Err(StateErrorV2::InvalidAccount);
        }
        let prior = match self.deposit_origins.get(&receipt) {
            Some((old_generation, origin)) if *old_generation == generation => {
                if *origin != origin_deposited {
                    return Err(StateErrorV2::InvalidCredit);
                }
                self.credited
                    .get(&receipt)
                    .copied()
                    .ok_or(StateErrorV2::InvalidCredit)?
            }
            _ => {
                if self
                    .credited
                    .get(&receipt)
                    .is_some_and(|prior| *prior > origin_deposited)
                {
                    return Err(StateErrorV2::InvalidCredit);
                }
                origin_deposited
            }
        };
        if prior.checked_add(amount) != Some(cumulative_deposited) {
            return Err(StateErrorV2::InvalidCredit);
        }
        let mut next = self.clone();
        next.apply_execution(pre, post)?;
        next.credited.insert(receipt, cumulative_deposited);
        next.deposit_origins
            .insert(receipt, (generation, origin_deposited));
        next.ingress_count = record.sequence;
        next.ingress_root = record.root;
        next.validate_capacity()?;
        *self = next;
        Ok(())
    }

    pub fn apply_execution(
        &mut self,
        pre: &[(Hash32, CommittedAccountV2)],
        post: &[(Hash32, CommittedAccountV2)],
    ) -> Result<(), StateErrorV2> {
        if pre.len() != post.len()
            || pre.windows(2).any(|pair| pair[0].0 >= pair[1].0)
            || pre
                .iter()
                .zip(post)
                .any(|(before, after)| before.0 != after.0)
        {
            return Err(StateErrorV2::InvalidAccounts);
        }
        let mut next = self.clone();
        for ((key, before), (_, after)) in pre.iter().zip(post) {
            if next.frozen.contains_key(key) {
                return Err(StateErrorV2::Frozen);
            }
            if next
                .accounts
                .get(key)
                .unwrap_or(&CommittedAccountV2::empty())
                != before
            {
                return Err(StateErrorV2::InvalidPrestate);
            }
            if !next.origins.contains_key(key) {
                next.origins.insert(*key, AccountOriginV2::ErCreated);
            }
            next.accounts.insert(*key, after.clone());
        }
        next.validate_capacity()?;
        *self = next;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RecordedInnerInstructionV2 {
    pub outer_index: u32,
    pub stack_height: u32,
    pub program_id_index: u8,
    pub accounts: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RecordedExecutionResultV2 {
    pub status: Vec<u8>,
    pub executed_units: u64,
    pub inner: Vec<RecordedInnerInstructionV2>,
    pub logs: Vec<String>,
    pub return_data: Option<(Hash32, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum SettlementEffectV2 {
    SolWithdrawal {
        source: Hash32,
        recipient: Hash32,
        amount: u64,
        cumulative_withdrawn: u64,
    },
    TokenWithdrawal {
        bridge: Hash32,
        session_bridge: Hash32,
        account: Hash32,
        destination: Hash32,
        amount: u64,
        decimals: u8,
        cumulative_withdrawn: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum TransitionPayloadV2 {
    TokenIngress {
        record: IngressRecordV2,
        transaction: Vec<u8>,
        pre: Vec<(Hash32, CommittedAccountV2)>,
        post: Vec<(Hash32, CommittedAccountV2)>,
        execution_result: Vec<u8>,
        replay: Vec<u8>,
    },
    Ingress {
        record: IngressRecordV2,
        account_data: Vec<u8>,
    },
    Execution {
        transaction: Vec<u8>,
        pre: Vec<(Hash32, CommittedAccountV2)>,
        post: Vec<(Hash32, CommittedAccountV2)>,
        execution_result: Vec<u8>,
        replay: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RecordedTransitionV2 {
    pub er_slot: u64,
    pub before: StateRootOpeningV2,
    pub after: StateRootOpeningV2,
    pub payload: TransitionPayloadV2,
    pub readonly_l1_values: Vec<crate::checkpoint::ReadonlyL1ValueV1>,
    pub settlement_effects: Vec<Vec<u8>>,
}

impl RecordedTransitionV2 {
    pub fn apply(&self, state: &mut CommittedStateV2) -> Result<(), StateErrorV2> {
        if self.er_slot == 0 || state.opening()? != self.before {
            return Err(StateErrorV2::InvalidPrestate);
        }
        let mut candidate = state.clone();
        match &self.payload {
            TransitionPayloadV2::TokenIngress {
                record,
                transaction,
                pre,
                post,
                ..
            } => {
                if transaction.is_empty()
                    || transaction.len() > 1_232
                    || !self.settlement_effects.is_empty()
                {
                    return Err(StateErrorV2::InvalidAccounts);
                }
                candidate.apply_token_ingress_execution(record, pre, post)?;
            }
            TransitionPayloadV2::Ingress {
                record,
                account_data,
            } => {
                if !self.settlement_effects.is_empty() {
                    return Err(StateErrorV2::InvalidAccounts);
                }
                candidate.apply_ingress(record, account_data)?;
            }
            TransitionPayloadV2::Execution {
                transaction,
                pre,
                post,
                ..
            } => {
                if transaction.is_empty() || transaction.len() > 1_232 {
                    return Err(StateErrorV2::InvalidAccounts);
                }
                candidate.apply_execution(pre, post)?;
            }
        }
        candidate.apply_settlement_effects(&self.settlement_effects)?;
        if candidate.opening()? != self.after {
            return Err(StateErrorV2::InvalidPrestate);
        }
        *state = candidate;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        northstar_zk_types::{continuity::CHECKPOINT_FORMAT_VERSION_V2, FrBytes},
    };

    fn identity() -> SessionIdentityV2 {
        SessionIdentityV2 {
            version: CHECKPOINT_FORMAT_VERSION_V2,
            l1_genesis: [1; 32],
            portal: [2; 32],
            session: [3; 32],
            nonce: 1,
            grid_id: 0,
            validator: [4; 32],
            profile: [5; 32],
        }
    }

    fn ingress(state: &CommittedStateV2, payload: IngressPayloadV2) -> IngressRecordV2 {
        IngressRecordV2::new(
            state.identity.commitment(hash_parts).unwrap(),
            state.ingress_count + 1,
            10,
            state.ingress_root,
            payload,
            1,
            hash_parts,
        )
        .unwrap()
    }

    fn credit(state: &CommittedStateV2, amount: u64, total: u64) -> IngressRecordV2 {
        ingress(
            state,
            IngressPayloadV2::SolDeposit {
                recipient: [20; 32],
                receipt: [21; 32],
                amount,
                cumulative_deposited: total,
            },
        )
    }

    #[test]
    fn regression_v2_nonzero_inputs_require_a_committed_ingress_transition() {
        let mut state = CommittedStateV2::new(identity()).unwrap();
        let first = state.root().unwrap();
        assert_eq!(
            state.opening().unwrap(),
            northstar_zk_types::continuity::empty_state_opening_v2(
                state.identity.commitment(hash_parts).unwrap(),
                hash_parts,
            )
            .unwrap()
        );
        let value = CommittedAccountV2 {
            lamports: 100_000_000,
            ..CommittedAccountV2::empty()
        };
        assert_eq!(
            state.apply_execution(&[([20; 32], value.clone())], &[([20; 32], value)]),
            Err(StateErrorV2::InvalidPrestate),
        );
        assert_eq!(state.root().unwrap(), first);
        let record = credit(&state, 100_000_000, 100_000_000);
        state.apply_ingress(&record, &[]).unwrap();
        let credited_root = state.root().unwrap();
        assert_ne!(credited_root, first);
        let before = state.accounts.get(&[20; 32]).unwrap().clone();
        let after = CommittedAccountV2 {
            lamports: 99_000_000,
            ..before.clone()
        };
        state
            .apply_execution(&[([20; 32], before)], &[([20; 32], after)])
            .unwrap();
        assert_eq!(state.ingress_count(), 1);
        assert_eq!(state.ingress_root(), record.root);
        FrBytes::new(state.root().unwrap()).unwrap();
    }

    #[test]
    fn regression_v2_ingress_rejects_replay_and_preserves_er_values() {
        let mut state = CommittedStateV2::new(identity()).unwrap();
        let record = credit(&state, 100, 100);
        state.apply_ingress(&record, &[]).unwrap();
        let before = state.accounts.get(&[20; 32]).unwrap().clone();
        state
            .apply_execution(
                &[([20; 32], before.clone())],
                &[(
                    [20; 32],
                    CommittedAccountV2 {
                        lamports: 60,
                        ..before
                    },
                )],
            )
            .unwrap();
        let stable = state.clone();
        assert!(state.apply_ingress(&record, &[]).is_err());
        assert_eq!(state, stable);
        let topup = credit(&state, 50, 150);
        state.apply_ingress(&topup, &[]).unwrap();
        assert_eq!(state.accounts.get(&[20; 32]).unwrap().lamports, 110);
        let wrong = credit(&state, 1, 149);
        let stable = state.clone();
        assert_eq!(
            state.apply_ingress(&wrong, &[]),
            Err(StateErrorV2::InvalidCredit)
        );
        assert_eq!(state, stable);
    }

    #[test]
    fn regression_v2_delegation_binds_bytes_and_rejects_origin_aliases() {
        let mut state = CommittedStateV2::new(identity()).unwrap();
        let record = ingress(
            &state,
            IngressPayloadV2::Delegation {
                account: [30; 32],
                record: [31; 32],
                owner: [32; 32],
                lamports: 1_000_000,
                data_len: 3,
                data_hash: hash_parts(&[b"abc"]),
                executable: false,
                rent_epoch: 0,
            },
        );
        assert_eq!(
            state.apply_ingress(&record, b"abd"),
            Err(StateErrorV2::InvalidAccount)
        );
        state.apply_ingress(&record, b"abc").unwrap();
        let alias = ingress(&state, record.payload);
        let stable = state.clone();
        assert_eq!(
            state.apply_ingress(&alias, b"abc"),
            Err(StateErrorV2::OriginConflict)
        );
        assert_eq!(state, stable);
        let freeze = ingress(
            &state,
            IngressPayloadV2::Freeze {
                account: [30; 32],
                request: [33; 32],
                requested_l1_slot: 10,
            },
        );
        state.apply_ingress(&freeze, &[]).unwrap();
        let account = state.accounts.get(&[30; 32]).unwrap().clone();
        assert_eq!(
            state.apply_execution(&[([30; 32], account.clone())], &[([30; 32], account)]),
            Err(StateErrorV2::Frozen)
        );
        let wrong_release = ingress(
            &state,
            IngressPayloadV2::Release {
                account: [30; 32],
                request: [34; 32],
            },
        );
        assert_eq!(
            state.apply_ingress(&wrong_release, &[]),
            Err(StateErrorV2::OriginConflict)
        );
        let release = ingress(
            &state,
            IngressPayloadV2::Release {
                account: [30; 32],
                request: [33; 32],
            },
        );
        state.apply_ingress(&release, &[]).unwrap();
        assert!(!state.accounts.contains_key(&[30; 32]));
    }

    #[test]
    fn regression_v2_equal_account_bytes_do_not_alias_distinct_origins() {
        let mut sol = CommittedStateV2::new(identity()).unwrap();
        sol.apply_ingress(&credit(&sol, 100, 100), &[]).unwrap();
        let mut delegated = CommittedStateV2::new(identity()).unwrap();
        let record = ingress(
            &delegated,
            IngressPayloadV2::Delegation {
                account: [20; 32],
                record: [31; 32],
                owner: [0; 32],
                lamports: 100,
                data_len: 0,
                data_hash: hash_parts(&[&[]]),
                executable: false,
                rent_epoch: 0,
            },
        );
        delegated.apply_ingress(&record, &[]).unwrap();
        assert_eq!(sol.accounts(), delegated.accounts());
        assert_eq!(
            sol.opening().unwrap().account_root,
            delegated.opening().unwrap().account_root
        );
        assert_ne!(
            sol.opening().unwrap().origin_root,
            delegated.opening().unwrap().origin_root
        );
    }

    #[test]
    fn regression_v2_capacity_rejection_preserves_the_committed_state() {
        let mut state = CommittedStateV2::new(identity()).unwrap();
        let stable = state.clone();
        let data = vec![0; MAX_COMMITTED_STATE_BYTES_V2];
        let record = ingress(
            &state,
            IngressPayloadV2::Delegation {
                account: [20; 32],
                record: [31; 32],
                owner: [32; 32],
                lamports: 100,
                data_len: u32::try_from(data.len()).unwrap(),
                data_hash: hash_parts(&[&data]),
                executable: false,
                rent_epoch: 0,
            },
        );
        assert_eq!(
            state.apply_ingress(&record, &data),
            Err(StateErrorV2::Capacity)
        );
        assert_eq!(state, stable);
        let oversized = CommittedAccountV2 {
            data,
            ..CommittedAccountV2::empty()
        };
        assert_eq!(
            state.apply_execution(
                &[([20; 32], CommittedAccountV2::empty())],
                &[([20; 32], oversized)]
            ),
            Err(StateErrorV2::Capacity)
        );
        assert_eq!(state, stable);
    }

    #[test]
    fn regression_v2_context_order_and_payload_mutations_are_rejected() {
        let state = CommittedStateV2::new(identity()).unwrap();
        let record = credit(&state, 10, 10);
        for variant in 0..5 {
            let mut changed = record;
            match variant {
                0 => changed.sequence += 1,
                1 => changed.session_context[0] ^= 1,
                2 => changed.previous_root[0] ^= 1,
                3 => changed.root[0] ^= 1,
                _ => changed.payload = credit(&state, 11, 11).payload,
            }
            let mut candidate = state.clone();
            assert!(candidate.apply_ingress(&changed, &[]).is_err());
            assert_eq!(candidate, state);
        }
    }
}
