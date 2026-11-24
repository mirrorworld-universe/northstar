use {
    super::*,
    crate::checkpoint::ReadonlyL1ValueV1,
    northstar_portal::{SessionBridge, SessionBridgeV2},
    northstar_token_bridge::instruction::TokenBridgeInstruction,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_transaction::versioned::VersionedTransaction,
};

fn registration(state: &CommittedStateV2, t: ErTokenAccount) -> ReadonlyL1ValueV1 {
    let portal: Pubkey = state.identity().portal.into();
    let mint = t.mint.into();
    let bridge = northstar_token_bridge::id();
    let context = state.opening().unwrap().session_context;
    let (account, bump) = northstar_portal::find_session_bridge_v2_pda(&portal, &context, &mint);
    let frame = SessionBridgeV2 {
        discriminator: SessionBridgeV2::DISCRIMINATOR,
        session_context: context,
        base: SessionBridge {
            discriminator: SessionBridge::DISCRIMINATOR,
            session: state.identity().session.into(),
            mint,
            bridge_program: bridge,
            token_program: spl_token_interface::id(),
            vault: northstar_token_bridge::find_token_vault_pda(&bridge, &account).0,
            bump,
        },
    };
    ReadonlyL1ValueV1 {
        account,
        owner: portal,
        lamports: 1_000_000,
        data_hash: hash_parts(&[&borsh::to_vec(&frame).unwrap()]),
        observed_l1_slot: 1,
    }
}
fn call(
    state: &CommittedStateV2,
    key: Hash32,
    t: ErTokenAccount,
    destination: Pubkey,
    amount: u64,
) -> Instruction {
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new_readonly(t.owner.into(), true),
            AccountMeta::new(key.into(), false),
            AccountMeta::new_readonly(t.session_bridge.into(), false),
            AccountMeta::new_readonly(state.identity().portal.into(), false),
            AccountMeta::new_readonly(destination, false),
            AccountMeta::new_readonly(spl_token_interface::id(), false),
            AccountMeta::new_readonly(t.mint.into(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::StartWithdrawal {
            amount,
            decimals: 6,
        })
        .unwrap(),
    }
}
fn signed(calls: &[Instruction], payer: &Keypair) -> VersionedTransaction {
    VersionedTransaction::try_new(
        VersionedMessage::Legacy(Message::new(calls, Some(&payer.pubkey()))),
        &[payer],
    )
    .unwrap()
}
fn put(
    record: &mut RecordedTransitionV2,
    tx: &VersionedTransaction,
    inner: Vec<RecordedInnerInstructionV2>,
    status: Result<(), solana_transaction::TransactionError>,
) {
    let TransitionPayloadV2::Execution {
        transaction,
        execution_result,
        ..
    } = &mut record.payload
    else {
        unreachable!()
    };
    *transaction = bincode::serialize(tx).unwrap();
    *execution_result = borsh::to_vec(&RecordedExecutionResultV2 {
        status: bincode::serialize(&status).unwrap(),
        executed_units: 1,
        inner,
        logs: vec![],
        return_data: None,
    })
    .unwrap();
}
pub(super) fn sign(
    record: &mut RecordedTransitionV2,
    state: &CommittedStateV2,
    key: Hash32,
    t: ErTokenAccount,
) {
    let owner = Keypair::new_from_array([10; 32]);
    assert_eq!(owner.pubkey().to_bytes(), t.owner);
    record.readonly_l1_values = vec![registration(state, t)];
    put(
        record,
        &signed(&[call(state, key, t, [64; 32].into(), 3)], &owner),
        vec![],
        Ok(()),
    );
}
fn fixture() -> (
    CommittedStateV2,
    Hash32,
    ErTokenAccount,
    RecordedTransitionV2,
) {
    let mut state = CommittedStateV2::new(identity()).unwrap();
    let (key, t) = holding(&state, 10, 1, 12);
    seed_token(&mut state, key, t);
    let mut record = execution(
        &state,
        vec![(key, ErTokenAccount { amount: 9, ..t })],
        vec![event(key, t, 3)],
    );
    sign(&mut record, &state, key, t);
    (state, key, t, record)
}
fn rejected(record: &RecordedTransitionV2, state: &CommittedStateV2) {
    let mut s = state.clone();
    assert_eq!(record.apply(&mut s), Err(StateErrorV2::TokenProvenance));
    assert_eq!(s, *state);
}

#[test]
fn invariant_v2_direct_provenance_binds_signature_destination_and_complete_writes() {
    let (state, key, t, good) = fixture();
    good.apply(&mut state.clone()).unwrap();
    let owner = Keypair::new_from_array([10; 32]);
    let ix = call(&state, key, t, [64; 32].into(), 3);
    let mut readonly = ix.clone();
    readonly.accounts[1].is_writable = false;
    let mut bad_write = good.clone();
    put(&mut bad_write, &signed(&[readonly], &owner), vec![], Ok(()));
    rejected(&bad_write, &state);
    let mut redirected = good.clone();
    put(
        &mut redirected,
        &signed(&[call(&state, key, t, [65; 32].into(), 3)], &owner),
        vec![],
        Ok(()),
    );
    rejected(&redirected, &state);
    let mut invalid = good.clone();
    let mut tx = signed(std::slice::from_ref(&ix), &owner);
    tx.signatures[0] = solana_signature::Signature::default();
    put(&mut invalid, &tx, vec![], Ok(()));
    rejected(&invalid, &state);
    let mut wrong = good.clone();
    let mut not_signed = ix.clone();
    not_signed.accounts[0].is_signer = false;
    put(
        &mut wrong,
        &signed(&[not_signed], &Keypair::new()),
        vec![],
        Ok(()),
    );
    rejected(&wrong, &state);
    let mut missing = good.clone();
    missing.settlement_effects.clear();
    missing.after = missing.before;
    if let TransitionPayloadV2::Execution { pre, post, .. } = &mut missing.payload {
        pre.clear();
        post.clear();
    }
    rejected(&missing, &state);
    let mut no_registry = good.clone();
    no_registry.readonly_l1_values.clear();
    rejected(&no_registry, &state);
    let mut failed = good;
    put(
        &mut failed,
        &signed(&[ix], &owner),
        vec![],
        Err(solana_transaction::TransactionError::CommitCancelled),
    );
    rejected(&failed, &state);
}

#[test]
fn invariant_v2_only_call_bound_withdrawals_can_enter_a_durable_checkpoint() {
    use crate::checkpoint_v2::build_checkpoint_artifact_v2;
    let (state, key, t, good) = fixture();
    let mut genesis = CommittedStateV2::new(identity()).unwrap();
    let delegation = seed_token(&mut genesis, key, ErTokenAccount { amount: 12, ..t });
    assert_eq!(genesis, state);
    let mut different = good.clone();
    put(
        &mut different,
        &signed(
            &[call(&state, key, t, [70; 32].into(), 3)],
            &Keypair::new_from_array([10; 32]),
        ),
        vec![],
        Ok(()),
    );
    let root = tempfile::tempdir().unwrap();
    let mut store =
        CheckpointLineageStoreV2::open(root.path(), identity(), None, StartupModeV2::InitializeNew)
            .unwrap();
    store.append(delegation).unwrap();
    assert!(build_checkpoint_artifact_v2(&state, 2, std::slice::from_ref(&different)).is_err());
    assert!(store.append(different).is_err());
    assert_eq!(store.working_state().unwrap(), state);
    store.append(good).unwrap();
    let artifact =
        build_checkpoint_artifact_v2(store.accepted_state(), 2, store.unsealed()).unwrap();
    store
        .seal([70; 32], 2, borsh::to_vec(&artifact).unwrap())
        .unwrap();
    let retained = store.working_state().unwrap();
    drop(store);
    let store =
        CheckpointLineageStoreV2::open(root.path(), identity(), None, StartupModeV2::Resume)
            .unwrap();
    assert_eq!(store.working_state().unwrap(), retained);
}

#[test]
fn invariant_v2_nested_provenance_uses_the_immediate_signed_frame() {
    let (state, key, t, mut record) = fixture();
    let inner = call(&state, key, t, [64; 32].into(), 3);
    let payer = Keypair::new();
    let mut accounts = inner.accounts.clone();
    accounts[0].is_signer = false;
    accounts.push(AccountMeta::new_readonly(inner.program_id, false));
    let outer = Instruction {
        program_id: Pubkey::new_unique(),
        accounts,
        data: vec![],
    };
    let tx = signed(&[outer], &payer);
    let keys = tx.message.static_account_keys();
    let id = |k: &Pubkey| u8::try_from(keys.iter().position(|x| x == k).unwrap()).unwrap();
    let indices = inner
        .accounts
        .iter()
        .map(|m| id(&m.pubkey))
        .collect::<Vec<_>>();
    let frame = RecordedInnerInstructionV2 {
        outer_index: 0,
        stack_height: 2,
        program_id_index: id(&inner.program_id),
        accounts: indices.clone(),
        data: vec![],
    };
    let request = RecordedInnerInstructionV2 {
        stack_height: 3,
        data: inner.data,
        ..frame.clone()
    };
    put(
        &mut record,
        &tx,
        vec![frame.clone(), request.clone()],
        Ok(()),
    );
    record.apply(&mut state.clone()).unwrap();
    let mut hidden = frame.clone();
    hidden.accounts.remove(4);
    for trace in [
        vec![hidden, request.clone()],
        vec![request.clone()],
        vec![
            frame,
            RecordedInnerInstructionV2 {
                outer_index: 1,
                ..request
            },
        ],
    ] {
        put(&mut record, &tx, trace, Ok(()));
        rejected(&record, &state);
    }
}

#[cfg(feature = "checkpoint-continuity-v2")]
#[test]
fn invariant_v2_resolves_signed_lookup_indices_against_retained_table_bytes() {
    use {
        solana_address_lookup_table_interface::state::{AddressLookupTable, LookupTableMeta},
        solana_message::{v0, AddressLookupTableAccount},
        solana_runtime::bank::Bank,
        std::{borrow::Cow, sync::Arc},
    };
    let (state, key, t, mut record) = fixture();
    let owner = Keypair::new_from_array([10; 32]);
    let destination = Pubkey::from([64; 32]);
    let tables = [
        AddressLookupTableAccount {
            key: Pubkey::new_unique(),
            addresses: vec![destination, key.into()],
        },
        AddressLookupTableAccount {
            key: Pubkey::new_unique(),
            addresses: vec![t.mint.into(), t.session_bridge.into()],
        },
    ];
    let bank = Bank::new_for_tests(&solana_genesis_config::GenesisConfig::new(&[], &[]));
    for table in &tables {
        let bytes = AddressLookupTable {
            meta: LookupTableMeta::default(),
            addresses: Cow::Borrowed(&table.addresses),
        }
        .serialize_for_tests()
        .unwrap();
        let value = AccountSharedData::create_from_existing_shared_data(
            1_000_000,
            Arc::new(bytes),
            solana_sdk_ids::address_lookup_table::id(),
            false,
            0,
        );
        bank.store_account(&table.key, &value);
    }
    bank.freeze();
    let bank = Bank::new_from_parent(
        Arc::new(bank),
        solana_leader_schedule::SlotLeader::new_unique(),
        1,
    );
    let ix = call(&state, key, t, destination, 3);
    let msg =
        v0::Message::try_compile(&owner.pubkey(), &[ix], &tables, bank.last_blockhash()).unwrap();
    assert_eq!(msg.address_table_lookups.len(), 2);
    assert!(!msg.account_keys.contains(&destination) && !msg.account_keys.contains(&key.into()));
    let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[&owner]).unwrap();
    put(&mut record, &tx, vec![], Ok(()));
    rejected(&record, &state); // Pre-upgrade record without historical tables.
    let TransitionPayloadV2::Execution { replay, .. } = &mut record.payload else {
        unreachable!()
    };
    token_provenance::capture(&bank, &tx, &[], 1, replay, &mut record.readonly_l1_values).unwrap();
    drop(bank); // Reopen/replay no longer has the bank that resolved the lookup indices.
    record.apply(&mut state.clone()).unwrap();
    let mut changed = record.clone();
    if let TransitionPayloadV2::Execution { replay, .. } = &mut changed.payload {
        *replay.last_mut().unwrap() ^= 1;
    }
    rejected(&changed, &state);
    record.readonly_l1_values.pop();
    rejected(&record, &state);
}

#[test]
fn invariant_v2_real_instruction_codec_matches_proved_withdrawal_layout() {
    use northstar_token_bridge::trace_origin::withdrawal;
    for amount in [0, 1, 256, 0x0123_4567_89ab_cdef, u64::MAX] {
        for decimals in [0, 6, u8::MAX] {
            let data = borsh::to_vec(&TokenBridgeInstruction::StartWithdrawal { amount, decimals })
                .unwrap();
            assert_eq!(withdrawal(&data), Some((amount, decimals)));
            assert_eq!(withdrawal(&data[..9]), None);
            let mut extra = data;
            extra.push(0);
            assert_eq!(withdrawal(&extra), None);
            assert_eq!(
                withdrawal(
                    &borsh::to_vec(&TokenBridgeInstruction::Withdraw { amount, decimals }).unwrap()
                ),
                None
            );
        }
    }
}
