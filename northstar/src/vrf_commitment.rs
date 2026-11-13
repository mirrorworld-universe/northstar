//! Public request witnesses from the accepted continuity lineage, not signing authority.
use {
    crate::{
        continuity_v2::{hash_parts, CommittedStateV2},
        lineage_store_v2::{CheckpointLineageStoreV2, L1CheckpointHeadV2},
    },
    northstar_vrf_protocol::{
        AccountValue, MembershipPath, RequestWitness, StateOpening, MAX_REQUEST_BYTES,
    },
    northstar_zk_types::continuity::SessionIdentityV2,
    solana_pubkey::Pubkey,
    std::{
        io,
        sync::{Mutex, Weak},
    },
};

pub fn request_witness(
    state: &CommittedStateV2,
    head: &L1CheckpointHeadV2,
    address: &Pubkey,
) -> io::Result<Option<RequestWitness>> {
    let opening = state.opening().map_err(io::Error::other)?;
    if head.er_slot == 0
        || opening.commitment(hash_parts).map_err(io::Error::other)? != head.state_root
    {
        return Err(io::Error::other(
            "request witness does not match the accepted lineage head",
        ));
    }
    let Some(account) = state.accounts().get(&address.to_bytes()) else {
        return Ok(None);
    };
    if account.executable || account.data.len() > MAX_REQUEST_BYTES {
        return Ok(None);
    }
    let values = state
        .accounts()
        .iter()
        .map(|(key, value)| AccountValue {
            address: *key,
            owner: value.owner,
            lamports: value.lamports,
            executable: value.executable,
            rent_epoch: value.rent_epoch,
            data_hash: hash_parts(&[&value.data]),
        })
        .collect::<Vec<_>>();
    let index = values
        .iter()
        .position(|value| value.address == address.to_bytes())
        .ok_or_else(|| io::Error::other("request membership index is missing"))?;
    let native_values = values
        .iter()
        .map(|value| crate::checkpoint::StateAccountValueV1 {
            account: Pubkey::new_from_array(value.address),
            owner: Pubkey::new_from_array(value.owner),
            lamports: value.lamports,
            executable: value.executable,
            rent_epoch: value.rent_epoch,
            data_hash: value.data_hash,
        })
        .collect::<Vec<_>>();
    let native_path = crate::checkpoint::state_membership_path_v1(&native_values, index)
        .map_err(io::Error::other)?;
    let path = MembershipPath {
        leaf_index: native_path.leaf_index,
        leaf_count: native_path.leaf_count,
        siblings: native_path.siblings,
    };
    let witness = RequestWitness {
        version: 1,
        er_slot: head.er_slot,
        opening: StateOpening {
            session_context: opening.session_context,
            account_root: opening.account_root,
            origin_root: opening.origin_root,
            ingress_count: opening.ingress_count,
            ingress_root: opening.ingress_root,
        },
        account: values[index].clone(),
        data: account.data.clone(),
        path,
    };
    if !verify_witness(&witness, &opening.session_context, &head.state_root) {
        return Err(io::Error::other(
            "request membership encoding differs from the native state commitment",
        ));
    }
    Ok(Some(witness))
}

pub fn verify_witness(witness: &RequestWitness, context: &[u8; 32], root: &[u8; 32]) -> bool {
    if witness.version != 1
        || witness.er_slot == 0
        || witness.data.len() > MAX_REQUEST_BYTES
        || witness.opening.session_context != *context
        || witness.opening.commitment() != Some(*root)
        || witness.account.executable
        || witness.account.data_hash != hash_parts(&[&witness.data])
        || witness.path.leaf_count as usize > crate::continuity_v2::MAX_COMMITTED_ACCOUNTS_V2
    {
        return false;
    }
    let value = crate::checkpoint::StateAccountValueV1 {
        account: Pubkey::new_from_array(witness.account.address),
        owner: Pubkey::new_from_array(witness.account.owner),
        lamports: witness.account.lamports,
        executable: witness.account.executable,
        rent_epoch: witness.account.rent_epoch,
        data_hash: witness.account.data_hash,
    };
    let path = crate::checkpoint::MerkleAuthenticationPathV1 {
        leaf_index: witness.path.leaf_index,
        leaf_count: witness.path.leaf_count,
        siblings: witness.path.siblings.clone(),
    };
    crate::checkpoint::verify_state_membership_v1(&value, &path, witness.opening.account_root)
}

#[derive(Clone)]
pub struct VrfCommitmentSource {
    // A stopped relay must not keep the runtime's lineage writer alive across restart.
    store: Weak<Mutex<CheckpointLineageStoreV2>>,
}

impl VrfCommitmentSource {
    pub(crate) fn new(store: Weak<Mutex<CheckpointLineageStoreV2>>) -> Self {
        Self { store }
    }

    pub fn identity(&self) -> io::Result<Option<SessionIdentityV2>> {
        let Some(store) = self.store.upgrade() else {
            return Ok(None);
        };
        let store = store
            .lock()
            .map_err(|_| io::Error::other("lineage writer lock is poisoned"))?;
        if store.is_poisoned() {
            return Err(io::Error::other("lineage persistence is quarantined"));
        }
        Ok(Some(*store.accepted_state().identity()))
    }

    pub fn candidates(
        &self,
        coordinator: &Pubkey,
        discriminator: &[u8; 8],
    ) -> io::Result<Vec<Pubkey>> {
        let Some(store) = self.store.upgrade() else {
            return Ok(Vec::new());
        };
        let store = store
            .lock()
            .map_err(|_| io::Error::other("lineage writer lock is poisoned"))?;
        if store.is_poisoned() {
            return Err(io::Error::other("lineage persistence is quarantined"));
        }
        if store.accepted_head().is_none() {
            return Ok(Vec::new());
        }
        Ok(store
            .accepted_state()
            .accounts()
            .iter()
            .filter(|(_, account)| {
                account.owner == coordinator.to_bytes()
                    && !account.executable
                    && account.data.starts_with(discriminator)
                    && account.data.get(9) == Some(&0)
                    && account.data.len() <= MAX_REQUEST_BYTES
            })
            .map(|(key, _)| Pubkey::new_from_array(*key))
            .collect())
    }

    pub fn witness(&self, address: &Pubkey) -> io::Result<Option<RequestWitness>> {
        let Some(store) = self.store.upgrade() else {
            return Ok(None);
        };
        let store = store
            .lock()
            .map_err(|_| io::Error::other("lineage writer lock is poisoned"))?;
        if store.is_poisoned() {
            return Err(io::Error::other("lineage persistence is quarantined"));
        }
        let Some(head) = store.accepted_head() else {
            return Ok(None);
        };
        request_witness(store.accepted_state(), head, address)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            continuity_v2::{CommittedAccountV2, CommittedStateV2},
            lineage_store_v2::L1CheckpointHeadV2,
        },
        northstar_zk_types::continuity::SessionIdentityV2,
        solana_pubkey::Pubkey,
    };

    fn state() -> CommittedStateV2 {
        let mut state = CommittedStateV2::new(SessionIdentityV2 {
            version: 2,
            l1_genesis: [1; 32],
            portal: [2; 32],
            session: [3; 32],
            nonce: 1,
            grid_id: 0,
            validator: [4; 32],
            profile: [5; 32],
        })
        .unwrap();
        let pre = (1..=3)
            .map(|id| ([id; 32], CommittedAccountV2::empty()))
            .collect::<Vec<_>>();
        let post = (1..=3)
            .map(|id| {
                (
                    [id; 32],
                    CommittedAccountV2 {
                        owner: [9; 32],
                        lamports: 100,
                        data: b"pending request".to_vec(),
                        executable: false,
                        rent_epoch: u64::MAX,
                    },
                )
            })
            .collect::<Vec<_>>();
        state.apply_execution(&pre, &post).unwrap();
        state
    }
    fn head(state: &CommittedStateV2) -> L1CheckpointHeadV2 {
        L1CheckpointHeadV2 {
            checkpoint: [6; 32],
            er_slot: 11,
            state_root: state.root().unwrap(),
            checkpoint_hash: [7; 32],
        }
    }

    #[test]
    fn witness_matches_native_state_tree_and_opening_for_every_leaf() {
        let state = state();
        let head = head(&state);
        for id in 1..=3 {
            let witness = request_witness(&state, &head, &Pubkey::new_from_array([id; 32]))
                .unwrap()
                .unwrap();
            let opening = state.opening().unwrap();
            assert_eq!(witness.opening.account_root, opening.account_root);
            assert_eq!(witness.opening.commitment(), Some(head.state_root));
            assert!(verify_witness(
                &witness,
                &opening.session_context,
                &head.state_root
            ));
            assert_eq!(witness.data, b"pending request");
        }
    }

    #[test]
    fn relay_source_does_not_retain_the_lineage_writer_across_runtime_drop() {
        use {crate::lineage_store_v2::StartupModeV2, std::sync::Arc};
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("lineage");
        let identity = *state().identity();
        let store = Arc::new(Mutex::new(
            CheckpointLineageStoreV2::open(
                &directory,
                identity,
                None,
                StartupModeV2::InitializeNew,
            )
            .unwrap(),
        ));
        let source = VrfCommitmentSource::new(Arc::downgrade(&store));
        assert_eq!(source.identity().unwrap(), Some(identity));
        assert!(source
            .candidates(&Pubkey::new_from_array([9; 32]), &[1; 8])
            .unwrap()
            .is_empty());
        drop(store);
        assert!(source.identity().unwrap().is_none());
        assert!(source
            .witness(&Pubkey::new_from_array([1; 32]))
            .unwrap()
            .is_none());
        CheckpointLineageStoreV2::open(&directory, identity, None, StartupModeV2::Resume).unwrap();
    }

    #[test]
    fn witness_rejects_unaccepted_head_and_missing_or_oversized_request() {
        let mut state = state();
        let mut head = head(&state);
        head.state_root[0] ^= 1;
        assert!(request_witness(&state, &head, &Pubkey::new_from_array([1; 32])).is_err());
        head.state_root = state.root().unwrap();
        assert!(
            request_witness(&state, &head, &Pubkey::new_from_array([99; 32]))
                .unwrap()
                .is_none()
        );
        let before = state.accounts().get(&[1; 32]).unwrap().clone();
        let mut after = before.clone();
        after.data = vec![0; 1025];
        state
            .apply_execution(&[([1; 32], before)], &[([1; 32], after)])
            .unwrap();
        head.state_root = state.root().unwrap();
        assert!(
            request_witness(&state, &head, &Pubkey::new_from_array([1; 32]))
                .unwrap()
                .is_none()
        );
    }
}
