use {
    super::*,
    crate::lineage_store_v2::{CheckpointLineageStoreV2, StartupModeV2},
    northstar_token_bridge::state::ErTokenAccount,
};

fn holding(state: &CommittedStateV2, owner: u8, mint: u8, amount: u64) -> (Hash32, ErTokenAccount) {
    let portal = Pubkey::from(state.identity().portal);
    let context = state.identity().commitment(hash_parts).unwrap();
    let mint = Pubkey::from([mint; 32]);
    let registry = northstar_portal::find_session_bridge_v2_pda(&portal, &context, &mint).0;
    let owner = Pubkey::from([owner; 32]);
    let (key, bump) = northstar_token_bridge::find_er_token_account_pda(
        &northstar_token_bridge::id(),
        &registry,
        &owner,
    );
    (
        key.to_bytes(),
        ErTokenAccount {
            discriminator: ErTokenAccount::DISCRIMINATOR,
            session_bridge: registry.to_bytes(),
            mint: mint.to_bytes(),
            owner: owner.to_bytes(),
            amount,
            bump,
        },
    )
}

fn seed_token(
    state: &mut CommittedStateV2,
    key: Hash32,
    token: ErTokenAccount,
) -> RecordedTransitionV2 {
    let data = borsh::to_vec(&token).unwrap();
    let record = ingress(
        state,
        IngressPayloadV2::Delegation {
            account: key,
            record: [52; 32],
            owner: northstar_token_bridge::id().to_bytes(),
            lamports: 1_000_000,
            data_len: data.len() as u32,
            data_hash: hash_parts(&[&data]),
            executable: false,
            rent_epoch: u64::MAX,
        },
    );
    let before = state.opening().unwrap();
    state.apply_ingress(&record, &data).unwrap();
    RecordedTransitionV2 {
        er_slot: 1,
        before,
        after: state.opening().unwrap(),
        payload: TransitionPayloadV2::Ingress {
            record,
            account_data: data,
        },
        readonly_l1_values: vec![],
        settlement_effects: vec![],
    }
}

fn event(key: Hash32, token: ErTokenAccount, amount: u64) -> Vec<u8> {
    borsh::to_vec(&SettlementEffectV2::TokenWithdrawal {
        bridge: northstar_token_bridge::id().to_bytes(),
        session_bridge: token.session_bridge,
        account: key,
        destination: [64; 32],
        amount,
        decimals: 6,
        cumulative_withdrawn: amount,
    })
    .unwrap()
}

// A self-consistent state opening is necessary but not sufficient for a ledger transition.
fn execution(
    state: &CommittedStateV2,
    mut writes: Vec<(Hash32, ErTokenAccount)>,
    effects: Vec<Vec<u8>>,
) -> RecordedTransitionV2 {
    writes.sort_by_key(|(key, _)| *key);
    let pre: Vec<_> = writes
        .iter()
        .map(|(key, _)| (*key, state.accounts()[key].clone()))
        .collect();
    let post: Vec<_> = pre
        .iter()
        .zip(writes)
        .map(|((key, old), (_, t))| {
            (
                *key,
                CommittedAccountV2 {
                    data: borsh::to_vec(&t).unwrap(),
                    ..old.clone()
                },
            )
        })
        .collect();
    let mut next = state.clone();
    next.apply_execution(&pre, &post).unwrap();
    next.apply_settlement_effects(&effects).unwrap();
    RecordedTransitionV2 {
        er_slot: 2,
        before: state.opening().unwrap(),
        after: next.opening().unwrap(),
        payload: TransitionPayloadV2::Execution {
            transaction: vec![1],
            pre,
            post,
            execution_result: vec![],
            replay: vec![],
        },
        readonly_l1_values: vec![],
        settlement_effects: effects,
    }
}

#[test]
fn liability_replay_requires_an_offset_for_each_vault_and_pending_effect() {
    let mut state = CommittedStateV2::new(identity()).unwrap();
    let (a, t) = holding(&state, 10, 1, 12);
    let (b, u) = holding(&state, 11, 1, 4);
    let (c, v) = holding(&state, 12, 2, 3);
    seed_token(&mut state, a, t);
    seed_token(&mut state, b, u);
    seed_token(&mut state, c, v);
    for (label, writes, effects, valid) in [
        (
            "transfer and withdrawal",
            vec![
                (a, ErTokenAccount { amount: 5, ..t }),
                (b, ErTokenAccount { amount: 8, ..u }),
            ],
            vec![event(a, t, 3)],
            true,
        ),
        (
            "debit with no payout effect",
            vec![(a, ErTokenAccount { amount: 9, ..t })],
            vec![],
            false,
        ),
        (
            "payout without a debit",
            vec![(a, t)],
            vec![event(a, t, 3)],
            false,
        ),
        (
            "unrelated vaults cannot offset",
            vec![
                (a, ErTokenAccount { amount: 9, ..t }),
                (c, ErTokenAccount { amount: 6, ..v }),
            ],
            vec![],
            false,
        ),
        (
            "effect bound to wrong registry",
            vec![(a, ErTokenAccount { amount: 9, ..t })],
            vec![event(a, v, 3)],
            false,
        ),
    ] {
        let tx = execution(&state, writes, effects);
        let mut copy = state.clone();
        assert_eq!(tx.apply(&mut copy).is_ok(), valid, "{label}");
        if !valid {
            assert_eq!(copy, state, "{label}");
        }
    }
}

#[test]
fn liability_ingress_credits_only_its_authenticated_budget() {
    let mut state = CommittedStateV2::new(identity()).unwrap();
    let (a, t) = holding(&state, 10, 1, 12);
    let (b, u) = holding(&state, 11, 1, 4);
    seed_token(&mut state, a, t);
    seed_token(&mut state, b, u);
    let receipt = northstar_token_bridge::find_token_deposit_receipt_pda(
        &northstar_token_bridge::id(),
        &Pubkey::from(t.session_bridge),
        &Pubkey::from(a),
    )
    .0;
    let input = ingress(
        &state,
        IngressPayloadV2::TokenDeposit {
            bridge: northstar_token_bridge::id().to_bytes(),
            session_bridge: t.session_bridge,
            account: a,
            receipt: receipt.to_bytes(),
            mint: t.mint,
            token_program: spl_token_interface::id().to_bytes(),
            origin_deposited: 0,
            amount: 3,
            cumulative_deposited: 3,
        },
    );
    for (other, valid) in [(4, true), (5, false)] {
        let tx = execution(
            &state,
            vec![
                (a, ErTokenAccount { amount: 15, ..t }),
                (b, ErTokenAccount { amount: other, ..u }),
            ],
            vec![],
        );
        let TransitionPayloadV2::Execution { pre, post, .. } = tx.payload else {
            unreachable!()
        };
        let mut next = state.clone();
        next.apply_token_ingress_execution(&input, &pre, &post)
            .unwrap();
        let tx = RecordedTransitionV2 {
            after: next.opening().unwrap(),
            payload: TransitionPayloadV2::TokenIngress {
                record: input,
                transaction: vec![1],
                pre,
                post,
                execution_result: vec![],
                replay: vec![],
            },
            ..tx
        };
        let mut copy = state.clone();
        assert_eq!(tx.apply(&mut copy).is_ok(), valid);
        if !valid {
            assert_eq!(copy, state);
        }
    }
}

#[test]
fn liability_record_rejection_preserves_durable_head() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state");
    let mut store =
        CheckpointLineageStoreV2::open(&path, identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    let mut state = store.working_state().unwrap();
    let (a, t) = holding(&state, 10, 1, 12);
    let init = seed_token(&mut state, a, t);
    store.append(init).unwrap();
    let tx = execution(
        &state,
        vec![(a, ErTokenAccount { amount: 11, ..t })],
        vec![],
    );
    assert!(
        store.append(tx).is_err(),
        "unbalanced head must not be installed"
    );
    assert_eq!(store.working_state().unwrap(), state);
    drop(store);
    let store =
        CheckpointLineageStoreV2::open(&path, identity(), None, StartupModeV2::Resume).unwrap();
    assert_eq!(store.working_state().unwrap(), state);
}
