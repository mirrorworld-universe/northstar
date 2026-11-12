use northstar_vrf_protocol::{
    account_root, hash, membership_path, AccountValue, MembershipPath, RequestWitness, StateOpening,
};

fn accounts() -> Vec<AccountValue> {
    (1..=3)
        .map(|id| AccountValue {
            address: [id; 32],
            owner: [2; 32],
            lamports: 7,
            executable: false,
            rent_epoch: 9,
            data_hash: hash(&[b"request"]),
        })
        .collect()
}

fn witness(values: &[AccountValue], index: usize) -> RequestWitness {
    RequestWitness {
        version: 1,
        er_slot: 11,
        opening: StateOpening {
            session_context: [3; 32],
            account_root: account_root(values).unwrap(),
            origin_root: [4; 32],
            ingress_count: 0,
            ingress_root: [0; 32],
        },
        account: values[index].clone(),
        data: b"request".to_vec(),
        path: membership_path(values, index).unwrap(),
    }
}

#[test]
fn native_checkpoint_single_account_reference_vector() {
    let values = accounts();
    let value = witness(&values[..1], 0);
    assert_eq!(
        hex::encode(value.opening.account_root),
        "0592e5eb3a18a79da22e22405c5145ddbc765c24b5be75ac019cc97be02f4527"
    );
    assert_eq!(
        hex::encode(value.opening.commitment().unwrap()),
        "1fae018c6c928e7a74b479cd24efb93fbd22fd21f774a3e1fc044566ab60cdfd"
    );
    assert!(value.verify(&[3; 32], &value.opening.commitment().unwrap()));
}

#[test]
fn every_leaf_verifies_in_padded_tree() {
    let values = accounts();
    for index in 0..values.len() {
        let value = witness(&values, index);
        assert!(value.verify(&[3; 32], &value.opening.commitment().unwrap()));
    }
}

#[test]
fn membership_binds_metadata_data_context_and_root() {
    let values = accounts();
    let original = witness(&values, 1);
    let root = original.opening.commitment().unwrap();
    let mut variants = Vec::new();
    let mut changed = original.clone();
    changed.account.address[0] ^= 1;
    variants.push(changed);
    let mut changed = original.clone();
    changed.account.owner[0] ^= 1;
    variants.push(changed);
    let mut changed = original.clone();
    changed.account.lamports += 1;
    variants.push(changed);
    let mut changed = original.clone();
    changed.account.rent_epoch += 1;
    variants.push(changed);
    let mut changed = original.clone();
    changed.account.executable = true;
    variants.push(changed);
    let mut changed = original.clone();
    changed.data.push(0);
    variants.push(changed);
    let mut changed = original.clone();
    changed.opening.origin_root[0] ^= 1;
    variants.push(changed);
    let mut changed = original.clone();
    changed.opening.session_context[0] ^= 1;
    variants.push(changed);
    let mut changed = original.clone();
    changed.version = 2;
    variants.push(changed);
    let mut changed = original.clone();
    changed.path.leaf_count = u32::MAX;
    variants.push(changed);
    let mut changed = original.clone();
    changed.path.siblings.push([0; 32]);
    variants.push(changed);
    let mut changed = original.clone();
    changed.path.leaf_index = changed.path.leaf_count;
    variants.push(changed);
    for changed in variants {
        assert!(!changed.verify(&[3; 32], &root));
    }
    assert!(!original.verify(&[9; 32], &root));
    assert!(!original.verify(&[3; 32], &[9; 32]));
}

#[test]
fn construction_rejects_unbounded_unsorted_or_duplicate_accounts() {
    let mut values = accounts();
    values.swap(0, 1);
    assert!(account_root(&values).is_none());
    values[1] = values[0].clone();
    assert!(account_root(&values).is_none());
    assert!(membership_path(&[], 0).is_none());
    assert!(membership_path(&accounts(), 3).is_none());
    let path = MembershipPath {
        leaf_index: 0,
        leaf_count: 0,
        siblings: Vec::new(),
    };
    assert!(!path.verify(&accounts()[0], &[0; 32]));
}
