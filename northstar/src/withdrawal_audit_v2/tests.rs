use {
    super::*,
    crate::{
        checkpoint::ReadonlyL1ValueV1,
        checkpoint_v2::build_checkpoint_artifact_v2,
        continuity_v2::{CommittedStateV2, RecordedTransitionV2},
    },
    northstar_portal::{SessionBridge, SessionBridgeV2},
    northstar_token_bridge::{instruction::TokenBridgeInstruction, state::ErTokenAccount},
    northstar_zk_types::continuity::{IngressPayloadV2, IngressRecordV2, SessionIdentityV2},
    solana_account::{Account, AccountSharedData, WritableAccount},
    solana_address_lookup_table_interface::state::{AddressLookupTable, LookupTableMeta},
    solana_genesis_config::GenesisConfig,
    solana_instruction::{AccountMeta, Instruction},
    solana_leader_schedule::SlotLeader,
    solana_message::{v0, AddressLookupTableAccount, VersionedMessage},
    solana_program_pack::Pack,
    solana_rent::Rent,
    solana_runtime::{bank_forks::BankForks, genesis_utils::create_genesis_config_with_leader},
    solana_signer::Signer,
    solana_transaction::Transaction,
    std::{
        path::PathBuf,
        sync::{Arc, RwLock},
    },
};

mod archive;

fn complete(bank: &Bank) {
    while !bank.is_complete() {
        bank.register_tick_for_test(&solana_sha256_hasher::hash(
            &bank.tick_height().to_le_bytes(),
        ));
    }
    bank.set_block_id(Some(Default::default()));
    bank.freeze();
}

fn earlier_bank(
    genesis: &GenesisConfig,
    allocations: &BTreeMap<Pubkey, Account>,
    payer: &Pubkey,
) -> Bank {
    let root = Arc::new(Bank::new_for_tests(genesis));
    complete(&root);
    let leader = *root.leader();
    let parent = Bank::new_from_parent(root, leader, 1);
    let mut funds = parent.get_account(payer).unwrap();
    for (key, a) in allocations {
        assert!(parent.get_account(key).is_none());
        funds.checked_sub_lamports(a.lamports).unwrap();
        parent.store_account(key, &AccountSharedData::from(a.clone()));
    }
    parent.store_account(payer, &funds);
    complete(&parent);
    parent
}

fn bank_at_cut(parent: Bank) -> (Arc<Bank>, Arc<RwLock<BankForks>>) {
    let slot = parent.slot() + (1u64 << 40);
    let mut b =
        Bank::new_from_parent_ephemeral_isolated(Arc::new(parent), SlotLeader::default(), slot);
    b.configure_er(
        &solana_fee_structure::FeeStructure {
            lamports_per_signature: 0,
            lamports_per_write_lock: 0,
            compute_fee_bins: vec![],
        },
        crate::er_recent_blockhash_max_age_for_slot_duration(crate::DEFAULT_ER_SLOT_DURATION),
    );
    b.set_tick_height(b.max_tick_height() - b.ticks_per_slot());
    let forks = BankForks::new_rw_arc_ephemeral(b);
    let bank = forks.read().unwrap().root_bank();
    (bank, forks)
}
fn account(data: Vec<u8>, owner: Pubkey, executable: bool) -> Account {
    Account {
        lamports: Rent::default().minimum_balance(data.len()).max(1),
        data,
        owner,
        executable,
        rent_epoch: u64::MAX,
    }
}

#[test]
fn invariant_v2_audit_reexecutes_pda_signed_calls_on_a_separate_frozen_bank() {
    exercise_cut(&[]);
}

#[test]
fn invariant_v2_full_snapshot_reconstructs_the_independent_cpi_withdrawal_cut() {
    exercise_cut(&[1, 2]);
}

fn exercise_cut(earlier_withdrawals: &[u64]) {
    let setup =
        create_genesis_config_with_leader(100_000_000_000, &Pubkey::new_unique(), 10_000_000_000);
    let payer = setup.mint_keypair;
    let genesis = setup.genesis_config;
    let mut allocations = BTreeMap::new();
    let out = PathBuf::from(std::env::var_os("BPF_OUT_DIR").expect("V2 SBF artifacts required"));
    let bridge = northstar_token_bridge::id();
    let portal = Pubkey::new_unique();
    let caller = Pubkey::new_unique();
    let session = northstar_portal::find_session_pda(&portal).0;
    let identity = SessionIdentityV2 {
        version: 2,
        l1_genesis: genesis.hash().to_bytes(),
        portal: portal.to_bytes(),
        session: session.to_bytes(),
        nonce: 1,
        grid_id: 0,
        validator: payer.pubkey().to_bytes(),
        profile: [3; 32],
    };
    let mut start = CommittedStateV2::new(identity).unwrap();
    let context = start.opening().unwrap().session_context;
    let token = spl_token_interface::id();
    let mint = Pubkey::new_unique();
    let dest = Pubkey::new_unique();
    let (registry, reg_bump) =
        northstar_portal::find_session_bridge_v2_pda(&portal, &context, &mint);
    let (owner, owner_bump) =
        Pubkey::find_program_address(&[b"audit-owner", payer.pubkey().as_ref()], &caller);
    let (source, bump) =
        northstar_token_bridge::find_er_token_account_pda(&bridge, &registry, &owner);
    let table = AddressLookupTableAccount {
        key: Pubkey::new_unique(),
        addresses: vec![source, registry, dest],
    };
    let table_data = AddressLookupTable {
        meta: LookupTableMeta::default(),
        addresses: std::borrow::Cow::Borrowed(&table.addresses),
    }
    .serialize_for_tests()
    .unwrap();
    allocations.insert(
        table.key,
        account(
            table_data,
            solana_sdk_ids::address_lookup_table::id(),
            false,
        ),
    );
    let image = std::fs::read(out.join("northstar_token_bridge.so")).unwrap();
    let reviewed = hash_parts(&[&image]);
    for (k, bytes) in [
        (bridge, image),
        (
            caller,
            std::fs::read(out.join("northstar_continuity_fixture.so")).unwrap(),
        ),
        (
            portal,
            std::fs::read(out.join("northstar_portal.so")).unwrap(),
        ),
    ] {
        allocations.insert(k, account(bytes, bpf_loader::id(), true));
    }
    let frame = SessionBridgeV2 {
        discriminator: SessionBridgeV2::DISCRIMINATOR,
        session_context: context,
        base: SessionBridge {
            discriminator: SessionBridge::DISCRIMINATOR,
            session,
            mint,
            bridge_program: bridge,
            vault: northstar_token_bridge::find_token_vault_pda(&bridge, &registry).0,
            token_program: token,
            bump: reg_bump,
        },
    };
    allocations.insert(registry, account(encode(&frame).unwrap(), portal, false));
    let mut mint_data = vec![0; spl_token_interface::state::Mint::LEN];
    spl_token_interface::state::Mint::pack(
        spl_token_interface::state::Mint {
            is_initialized: true,
            decimals: 6,
            ..Default::default()
        },
        &mut mint_data,
    )
    .unwrap();
    allocations.insert(mint, account(mint_data, token, false));
    let mut peer = vec![0; spl_token_interface::state::Account::LEN];
    spl_token_interface::state::Account::pack(
        spl_token_interface::state::Account {
            mint,
            owner: payer.pubkey(),
            state: spl_token_interface::state::AccountState::Initialized,
            ..Default::default()
        },
        &mut peer,
    )
    .unwrap();
    allocations.insert(dest, account(peer, token, false));
    let holding = ErTokenAccount {
        discriminator: ErTokenAccount::DISCRIMINATOR,
        session_bridge: registry.to_bytes(),
        mint: mint.to_bytes(),
        owner: owner.to_bytes(),
        amount: 42 + earlier_withdrawals.iter().sum::<u64>(),
        bump,
    };
    let data = encode(&holding).unwrap();
    let holding_account = account(data.clone(), bridge, false);
    let delegation = IngressRecordV2::new(
        context,
        1,
        1,
        [0; 32],
        IngressPayloadV2::Delegation {
            account: source.to_bytes(),
            record: [19; 32],
            owner: bridge.to_bytes(),
            lamports: holding_account.lamports,
            data_len: data.len() as u32,
            data_hash: hash_parts(&[&data]),
            executable: false,
            rent_epoch: holding_account.rent_epoch,
        },
        1,
        hash_parts,
    )
    .unwrap();
    start.apply_ingress(&delegation, &data).unwrap();
    allocations.insert(source, holding_account);
    let base = || earlier_bank(&genesis, &allocations, &payer.pubkey());
    let (auditor, _a_forks) = bank_at_cut(base());
    let (producer, _p_forks) = bank_at_cut(base());
    // Bootstrap the test cut before either bank has run the withdrawal. No read of producer history.
    let payer_key = payer.pubkey().to_bytes();
    start
        .apply_execution(
            &[(payer_key, CommittedAccountV2::empty())],
            &[(
                payer_key,
                CommittedAccountV2::from_account(&auditor.get_account(&payer.pubkey()).unwrap()),
            )],
        )
        .unwrap();
    let instruction = |amount| {
        let mut data = vec![1, owner_bump];
        data.extend(
            encode(&TokenBridgeInstruction::StartWithdrawal {
                amount,
                decimals: 6,
            })
            .unwrap(),
        );
        Instruction {
            program_id: caller,
            accounts: vec![
                AccountMeta::new_readonly(bridge, false),
                AccountMeta::new_readonly(owner, false),
                AccountMeta::new(source, false),
                AccountMeta::new_readonly(registry, false),
                AccountMeta::new_readonly(portal, false),
                AccountMeta::new_readonly(dest, false),
                AccountMeta::new_readonly(token, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(payer.pubkey(), true),
            ],
            data,
        }
    };
    let effect = |amount, cumulative_withdrawn| {
        encode(&SettlementEffectV2::TokenWithdrawal {
            bridge: bridge.to_bytes(),
            session_bridge: registry.to_bytes(),
            account: source.to_bytes(),
            destination: dest.to_bytes(),
            amount,
            decimals: 6,
            cumulative_withdrawn,
        })
        .unwrap()
    };
    let mut prefix = vec![];
    let mut cumulative = 0;
    for (i, &amount) in earlier_withdrawals.iter().enumerate() {
        let ix = [instruction(amount)];
        let prior = if i == 0 {
            VersionedTransaction::from(Transaction::new_signed_with_payer(
                &ix,
                Some(&payer.pubkey()),
                &[&payer],
                producer.last_blockhash(),
            ))
        } else {
            let msg = v0::Message::try_compile(
                &payer.pubkey(),
                &ix,
                std::slice::from_ref(&table),
                producer.last_blockhash(),
            )
            .unwrap();
            assert_eq!(msg.address_table_lookups.len(), 1);
            assert!(!msg.account_keys.contains(&source));
            VersionedTransaction::try_new(VersionedMessage::V0(msg), &[&payer]).unwrap()
        };
        let prefix_writes = || {
            let mut keys = [source, payer.pubkey()];
            keys.sort_unstable();
            keys.map(|k| {
                (
                    k.to_bytes(),
                    CommittedAccountV2::from_account(&auditor.get_account(&k).unwrap()),
                )
            })
        };
        let before = prefix_writes();
        for bank in [&auditor, &producer] {
            bank.process_transaction_with_metadata(prior.clone())
                .unwrap()
                .status
                .unwrap();
        }
        start.apply_execution(&before, &prefix_writes()).unwrap();
        cumulative += amount;
        start
            .apply_settlement_effects(&[effect(amount, cumulative)])
            .unwrap();
        prefix.push(bincode::serialize(&prior).unwrap());
    }
    auditor.freeze();
    let bank_hash = auditor.hash().to_bytes();
    let pre_root = start.root().unwrap();
    let tx = VersionedTransaction::from(Transaction::new_signed_with_payer(
        &[instruction(3), instruction(3)],
        Some(&payer.pubkey()),
        &[&payer],
        producer.last_blockhash(),
    ));
    assert_eq!(tx.signatures.len(), 1);
    assert_ne!(owner, payer.pubkey());
    let mut writes = BTreeMap::new();
    let batch = producer.prepare_entry_batch([tx.clone()]).unwrap();
    let (mut results, _) = producer
        .load_execute_and_commit_transactions_with_pre_commit_callback(
            &batch,
            ExecutionRecordingConfig::new_single_setting(true),
            &mut ExecuteTimings::default(),
            None,
            |r| {
                let x = r[0].as_ref().unwrap().executed_transaction().unwrap();
                assert_eq!(
                    x.execution_details.status,
                    Ok(()),
                    "{:?}",
                    x.execution_details.log_messages
                );
                for ((key, a), touched) in x
                    .loaded_transaction
                    .accounts
                    .iter()
                    .zip(&x.loaded_transaction.touched_flags)
                {
                    if *touched
                        && !agave_reserved_account_keys::ReservedAccountKeys::all_keys_iter()
                            .any(|k| k == key)
                    {
                        writes.insert(
                            key.to_bytes(),
                            (
                                CommittedAccountV2::from_account(
                                    &producer.get_account(key).unwrap_or_default(),
                                ),
                                CommittedAccountV2::from_account(a),
                            ),
                        );
                    }
                }
                Ok(())
            },
        )
        .unwrap();
    drop(batch);
    let committed = results.remove(0).unwrap();
    assert_eq!(committed.status, Ok(()));
    let mut inner = vec![];
    for (i, g) in committed
        .inner_instructions
        .unwrap()
        .into_iter()
        .enumerate()
    {
        for ix in g {
            inner.push(RecordedInnerInstructionV2 {
                outer_index: i as u32,
                stack_height: u32::from(ix.stack_height),
                program_id_index: ix.instruction.program_id_index,
                accounts: ix.instruction.accounts,
                data: ix.instruction.data,
            });
        }
    }
    assert_eq!(inner.len(), 2);
    let result = RecordedExecutionResultV2 {
        status: bincode::serialize(&committed.status).unwrap(),
        executed_units: committed.executed_units,
        inner,
        logs: committed.log_messages.unwrap(),
        return_data: committed
            .return_data
            .map(|v| (v.program_id.to_bytes(), v.data)),
    };
    let (pre, post): (Vec<_>, Vec<_>) = writes
        .into_iter()
        .map(|(k, (a, b))| ((k, a), (k, b)))
        .unzip();
    let effects = [3, 6].map(|delta| effect(3, cumulative + delta)).to_vec();
    let mut end = start.clone();
    end.apply_execution(&pre, &post).unwrap();
    end.apply_settlement_effects(&effects).unwrap();
    let registry_account = producer.get_account(&registry).unwrap();
    let record = RecordedTransitionV2 {
        er_slot: producer.slot(),
        before: start.opening().unwrap(),
        after: end.opening().unwrap(),
        payload: TransitionPayloadV2::Execution {
            transaction: bincode::serialize(&tx).unwrap(),
            pre,
            post,
            execution_result: encode(&result).unwrap(),
            replay: vec![],
        },
        readonly_l1_values: vec![ReadonlyL1ValueV1 {
            account: registry,
            owner: portal,
            lamports: registry_account.lamports(),
            data_hash: hash_parts(&[registry_account.data()]),
            observed_l1_slot: 1,
        }],
        settlement_effects: effects,
    };
    let build = |r: &RecordedTransitionV2| {
        build_checkpoint_artifact_v2(&start, producer.slot(), std::slice::from_ref(r)).unwrap()
    };
    let artifact = build(&record);
    let pins = AuditPins {
        bank_slot: auditor.slot(),
        bank_hash,
        checkpoint_binding: artifact.binding_hash().unwrap(),
        step: 0,
        pre_state_root: pre_root,
        bridge,
        bridge_image: reviewed,
    };
    let observed = || audit(Some(&auditor), &artifact, &pins);
    let verified = observed().unwrap();
    assert_eq!(verified.signature, tx.signatures[0]);
    assert_eq!((verified.withdrawals, verified.cpi_withdrawals), (2, 2));
    if !prefix.is_empty() {
        archive::check(&genesis, base(), &prefix, &artifact, &pins, &verified);
    }
    assert_eq!(observed(), Ok(verified));
    assert!(!auditor.has_signature(&tx.signatures[0]));
    let amount = |b: &Bank| {
        borsh::from_slice::<ErTokenAccount>(b.get_account(&source).unwrap().data())
            .unwrap()
            .amount
    };
    assert_eq!((amount(&producer), amount(&auditor)), (36, 42));
    assert_eq!(auditor.hash().to_bytes(), bank_hash);
    assert!(matches!(audit(None, &artifact, &pins), Err(Unavailable(_))));
    for other in [
        AuditPins {
            bank_hash: [0; 32],
            ..pins
        },
        AuditPins {
            bank_slot: 2,
            ..pins
        },
        AuditPins { step: 1, ..pins },
        AuditPins {
            pre_state_root: [0; 32],
            ..pins
        },
        AuditPins {
            bridge_image: [0; 32],
            ..pins
        },
        AuditPins {
            checkpoint_binding: [0; 32],
            ..pins
        },
    ] {
        assert!(matches!(
            audit(Some(&auditor), &artifact, &other),
            Err(Unavailable(_))
        ));
    }
    // A structurally valid archive can disagree with a fresh run's diagnostics. Binding it
    // as the artifact under review does not make its execution result an independent witness.
    let mut changed = record;
    let mut other = result;
    other.executed_units += 1;
    if let TransitionPayloadV2::Execution {
        execution_result, ..
    } = &mut changed.payload
    {
        *execution_result = encode(&other).unwrap();
    }
    let different = build(&changed);
    let different_pin = AuditPins {
        checkpoint_binding: different.binding_hash().unwrap(),
        ..pins
    };
    assert_eq!(
        audit(Some(&auditor), &different, &different_pin),
        Err(Mismatch("execution result or invocation trace"))
    );
}
