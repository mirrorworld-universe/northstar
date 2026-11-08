use {
    super::*,
    crate::{
        continuity_v2::hash_parts,
        lineage_store_v2::StartupModeV2,
        portal_state::{try_parse_raw_portal_account, PortalAccount},
    },
    northstar_portal::{SessionV2, SettlementStatus},
    northstar_zk_types::continuity::{IngressPayloadV2, IngressRecordV2, SessionIdentityV2},
};

struct Fixture {
    parent: Arc<Bank>,
    _forks: Arc<std::sync::RwLock<solana_runtime::bank_forks::BankForks>>,
    manager: Arc<Keypair>,
    payer: Keypair,
    session: Pubkey,
    identity: SessionIdentityV2,
}

impl Fixture {
    fn new() -> Self {
        agave_logger::setup_with_default("info");
        let (parent, forks, _, authority) =
            crate::portal_e2e_tests::setup_bank_with_portal_id(PORTAL_PROGRAM_ID);
        let manager = Arc::new(authority);
        let payer = Keypair::new();
        let (session, _) = northstar_portal::find_session_pda(&PORTAL_PROGRAM_ID);
        let (vault, _) = northstar_portal::find_fee_vault_pda(&PORTAL_PROGRAM_ID);
        let (epoch, _) = northstar_portal::find_session_epoch_v2_pda(&PORTAL_PROGRAM_ID);
        let open = Instruction {
            program_id: PORTAL_PROGRAM_ID,
            accounts: vec![
                solana_instruction::AccountMeta::new(manager.pubkey(), true),
                solana_instruction::AccountMeta::new(session, false),
                solana_instruction::AccountMeta::new(vault, false),
                solana_instruction::AccountMeta::new_readonly(system_program::id(), false),
                solana_instruction::AccountMeta::new(epoch, false),
            ],
            data: borsh::to_vec(&northstar_portal::PortalInstruction::OpenSessionV2(
                northstar_portal::OpenSessionV2 {
                    session: northstar_portal::OpenSession {
                        grid_id: 0,
                        ttl_slots: 10_000,
                        fee_cap: 1_000,
                        validator: manager.pubkey(),
                        settlement_interval_slots: 10,
                    },
                    l1_genesis: [1; 32],
                    profile: [2; 32],
                },
            ))
            .unwrap(),
        };
        parent
            .process_transaction(&Transaction::new_signed_with_payer(
                &[open],
                Some(&manager.pubkey()),
                &[manager.as_ref()],
                parent.last_blockhash(),
            ))
            .unwrap();
        let frame: SessionV2 =
            borsh::from_slice(parent.get_account(&session).unwrap().data()).unwrap();
        let identity = frame.identity(&PORTAL_PROGRAM_ID, &session);
        let context = identity.commitment(hash_parts).unwrap();
        let (receipt, _) = northstar_portal::find_deposit_receipt_v2_pda(
            &PORTAL_PROGRAM_ID,
            &context,
            &payer.pubkey(),
        );
        let (record, _) =
            northstar_portal::find_ingress_record_v2_pda(&PORTAL_PROGRAM_ID, &context, 1);
        let deposit = Instruction {
            program_id: PORTAL_PROGRAM_ID,
            accounts: vec![
                solana_instruction::AccountMeta::new(manager.pubkey(), true),
                solana_instruction::AccountMeta::new(session, false),
                solana_instruction::AccountMeta::new(receipt, false),
                solana_instruction::AccountMeta::new_readonly(payer.pubkey(), false),
                solana_instruction::AccountMeta::new_readonly(system_program::id(), false),
                solana_instruction::AccountMeta::new(record, false),
            ],
            data: borsh::to_vec(&northstar_portal::PortalInstruction::DepositFeeV2 {
                lamports: 100_000_000,
            })
            .unwrap(),
        };
        parent
            .process_transaction(&Transaction::new_signed_with_payer(
                &[deposit],
                Some(&manager.pubkey()),
                &[manager.as_ref()],
                parent.last_blockhash(),
            ))
            .unwrap();
        parent.freeze();
        Self {
            parent,
            _forks: forks,
            manager,
            payer,
            session,
            identity,
        }
    }

    fn runtime(&self, path: &std::path::Path, mode: StartupModeV2) -> EphemeralRuntime {
        let mut runtime = EphemeralRuntime::new(
            self.parent.clone(),
            create_test_cluster_info(),
            EphemeralRollupSettings {
                session_pda: self.session,
                grid_id: 0,
                ttl_slots: 10_000,
                fee_cap: 1_000,
                er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
                delegated_accounts: vec![],
            },
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            PORTAL_PROGRAM_ID,
            self.manager.clone(),
        )
        .unwrap();
        runtime.set_session_pda(self.session);
        runtime
            .configure_checkpoint_v2(path, self.identity, mode)
            .unwrap();
        runtime.activate();
        runtime
    }

    fn transfer(&self, runtime: &EphemeralRuntime, recipient: Pubkey, amount: u64) {
        let transaction = Transaction::new_signed_with_payer(
            &[transfer(&self.payer.pubkey(), &recipient, amount)],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            runtime.bank().last_blockhash(),
        );
        TransactionClient::send_transactions_in_batch(
            &runtime._tx_client,
            vec![bincode::serialize(&VersionedTransaction::from(transaction)).unwrap()],
            &SendTransactionServiceStats::default(),
        );
    }
}

#[test]
fn regression_v2_sol_payout_chains_after_restart_and_later_topup() {
    let fixture = Fixture::new();
    let mut root = fixture.parent.clone();
    let recipient = Pubkey::new_unique();
    let directory = tempfile::tempdir().unwrap();
    let config = crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: fixture.manager.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    };
    let mut manager = crate::Manager::new(config.clone());
    let result = (|| -> std::result::Result<(), String> {
        manager
            .init_runtime(
                root.clone(),
                create_test_cluster_info(),
                find_free_addr(),
                find_free_addr(),
                find_free_addr(),
            )
            .map_err(|error| error.to_string())?;
        manager.activate_session(root.clone(), fixture.session, 0, 10_000, 1_000);
        manager.process_finalized_l1_inputs(&root);
        let mut previous = None;
        for (index, (amount, paid)) in [(10_000_000, 10_000_000), (3_000_000, 13_000_000)]
            .into_iter()
            .enumerate()
        {
            if index == 1 {
                manager.shutdown_runtime();
                manager = crate::Manager::new(config.clone());
                manager
                    .init_runtime(
                        root.clone(),
                        create_test_cluster_info(),
                        find_free_addr(),
                        find_free_addr(),
                        find_free_addr(),
                    )
                    .map_err(|error| error.to_string())?;
                if !manager.resume_active_session_from_l1(root.clone()) {
                    return Err("SOL session did not resume".into());
                }
                let bank =
                    Bank::new_from_parent(root.clone(), SlotLeader::default(), root.slot() + 1);
                let context = fixture
                    .identity
                    .commitment(hash_parts)
                    .map_err(|error| error.to_string())?;
                let receipt = northstar_portal::find_deposit_receipt_v2_pda(
                    &PORTAL_PROGRAM_ID,
                    &context,
                    &fixture.payer.pubkey(),
                )
                .0;
                let record =
                    northstar_portal::find_ingress_record_v2_pda(&PORTAL_PROGRAM_ID, &context, 2).0;
                let instruction = Instruction {
                    program_id: PORTAL_PROGRAM_ID,
                    accounts: vec![
                        solana_instruction::AccountMeta::new(fixture.manager.pubkey(), true),
                        solana_instruction::AccountMeta::new(fixture.session, false),
                        solana_instruction::AccountMeta::new(receipt, false),
                        solana_instruction::AccountMeta::new_readonly(
                            fixture.payer.pubkey(),
                            false,
                        ),
                        solana_instruction::AccountMeta::new_readonly(system_program::id(), false),
                        solana_instruction::AccountMeta::new(record, false),
                    ],
                    data: borsh::to_vec(&northstar_portal::PortalInstruction::DepositFeeV2 {
                        lamports: 7_000_000,
                    })
                    .unwrap(),
                };
                let transaction = Transaction::new_signed_with_payer(
                    &[instruction],
                    Some(&fixture.manager.pubkey()),
                    &[fixture.manager.as_ref()],
                    bank.last_blockhash(),
                );
                bank.process_transaction(&transaction)
                    .map_err(|error| format!("SOL topup: {error:?}"))?;
                bank.freeze();
                root = Arc::new(bank);
                manager.reanchor_to_l1_parent(root.clone());
                manager.process_finalized_l1_inputs(&root);
            }
            let runtime = manager.runtime.as_ref().ok_or("runtime missing")?;
            let before = runtime.bank().get_balance(&fixture.payer.pubkey());
            let instruction = Instruction {
                program_id: PORTAL_PROGRAM_ID,
                accounts: vec![
                    solana_instruction::AccountMeta::new(fixture.payer.pubkey(), true),
                    solana_instruction::AccountMeta::new_readonly(recipient, false),
                    solana_instruction::AccountMeta::new(crate::WITHDRAWAL_SINK, false),
                    solana_instruction::AccountMeta::new_readonly(system_program::id(), false),
                    solana_instruction::AccountMeta::new_readonly(
                        solana_sdk_ids::sysvar::clock::id(),
                        false,
                    ),
                ],
                data: borsh::to_vec(&northstar_portal::PortalInstruction::StartWithdrawal {
                    lamports: amount,
                })
                .unwrap(),
            };
            let transaction = Transaction::new_signed_with_payer(
                &[instruction],
                Some(&fixture.payer.pubkey()),
                &[&fixture.payer],
                runtime.bank().last_blockhash(),
            );
            TransactionClient::send_transactions_in_batch(
                &runtime._tx_client,
                vec![bincode::serialize(&VersionedTransaction::from(transaction)).unwrap()],
                &SendTransactionServiceStats::default(),
            );
            if before.checked_sub(amount)
                != Some(runtime.bank().get_balance(&fixture.payer.pubkey()))
            {
                return Err("SOL withdrawal did not execute".into());
            }
            let artifact =
                super::token_settlement_v2::settle_checkpoint(&mut manager, &mut root, |bank| {
                    Ok(bank.get_balance(&recipient) == paid)
                })?;
            if previous.is_some_and(|expected| expected != artifact.checkpoint.previous_state_root)
            {
                return Err("SOL checkpoint root did not chain".into());
            }
            previous = Some(artifact.checkpoint.new_state_root);
        }
        if manager
            .runtime
            .as_ref()
            .ok_or("runtime missing")?
            .bank()
            .get_balance(&fixture.payer.pubkey())
            != 94_000_000
        {
            return Err("final ER SOL balance differs".into());
        }
        Ok(())
    })();
    manager.shutdown_runtime();
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn regression_v2_manager_executes_cumulative_token_inputs_before_user_transactions() {
    let mut world =
        crate::portal_e2e_tests::token_bridge_v2::delegated_token_world(PORTAL_PROGRAM_ID, true);
    let source = world.funded_tokens(1_000);
    world.send(&[world.deposit(source, 42, 3)], &[]).unwrap();
    world.send(&[world.deposit(source, 8, 4)], &[]).unwrap();
    world.bank.freeze();
    let directory = tempfile::tempdir().unwrap();
    let config = crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    };
    let mut manager = crate::Manager::new(config.clone());
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
    manager.process_finalized_l1_inputs(&world.bank);
    let active = manager.has_active_runtime();
    let runtime = manager.runtime.as_ref().unwrap();
    let amount = runtime
        .bank()
        .get_account(&world.er)
        .and_then(|value| {
            borsh::from_slice::<northstar_token_bridge::state::ErTokenAccount>(value.data()).ok()
        })
        .map(|value| value.amount);
    let store = runtime._tx_client.lineage_store_v2().unwrap();
    let (prefix, token_executions) = {
        let store = store.lock().unwrap();
        (
            store.working_state().unwrap().ingress_count(),
            store
                .unsealed()
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.payload,
                        crate::continuity_v2::TransitionPayloadV2::TokenIngress { .. }
                    )
                })
                .count(),
        )
    };
    drop(store);
    manager.shutdown_runtime();
    assert_eq!(
        amount,
        Some(50),
        "authenticated token inputs must execute the real bridge program, not remain pending"
    );
    assert!(active);
    assert_eq!(prefix, 4);
    assert_eq!(token_executions, 2);
    assert_eq!(
        world.bank.get_account(&world.er).unwrap().owner(),
        &PORTAL_PROGRAM_ID
    );
    let l1: northstar_token_bridge::state::ErTokenAccount =
        borsh::from_slice(world.bank.get_account(&world.er).unwrap().data()).unwrap();
    assert_eq!(l1.amount, 0);
    let mut manager = crate::Manager::new(config);
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    let resumed = manager.resume_active_session_from_l1(world.bank.clone());
    manager.process_finalized_l1_inputs(&world.bank);
    let restored = manager
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.bank().get_account(&world.er))
        .and_then(|value| {
            borsh::from_slice::<northstar_token_bridge::state::ErTokenAccount>(value.data()).ok()
        })
        .map(|value| value.amount);
    let retained = manager
        .runtime
        .as_ref()
        .and_then(|runtime| runtime._tx_client.lineage_store_v2())
        .map(|store| {
            let store = store.lock().unwrap();
            (
                store.working_state().unwrap().ingress_count(),
                store.unsealed().len(),
            )
        });
    world.bank = Arc::new(Bank::new_from_parent(
        world.bank.clone(),
        SlotLeader::default(),
        world.bank.slot() + 1,
    ));
    world.send(&[world.deposit(source, 7, 5)], &[]).unwrap();
    world.bank.freeze();
    manager.reanchor_to_l1_parent(world.bank.clone());
    manager.process_finalized_l1_inputs(&world.bank);
    let topped_up = manager
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.bank().get_account(&world.er))
        .and_then(|value| {
            borsh::from_slice::<northstar_token_bridge::state::ErTokenAccount>(value.data()).ok()
        })
        .map(|value| value.amount);
    let final_prefix = manager
        .runtime
        .as_ref()
        .and_then(|runtime| runtime._tx_client.lineage_store_v2())
        .map(|store| {
            store
                .lock()
                .unwrap()
                .working_state()
                .unwrap()
                .ingress_count()
        });
    manager.shutdown_runtime();
    assert!(resumed);
    assert_eq!(restored, Some(50));
    assert_eq!(topped_up, Some(57));
    assert_eq!(final_prefix, Some(5));
    assert_eq!(
        retained,
        Some((4, 4)),
        "restart must not execute retained input records twice"
    );
}

#[test]
fn regression_v2_token_input_persistence_failure_cancels_svm_credit() {
    let world =
        crate::portal_e2e_tests::token_bridge_v2::delegated_token_world(PORTAL_PROGRAM_ID, true);
    let source = world.funded_tokens(1_000);
    world.send(&[world.deposit(source, 42, 3)], &[]).unwrap();
    world.bank.freeze();
    let directory = tempfile::tempdir().unwrap();
    let plans = directory.path().join("plans");
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(plans.clone()),
    });
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
    let runtime = manager.runtime.as_ref().unwrap();
    let record = runtime.process_l1_ingress_v2(&world.bank).unwrap().unwrap();
    let before = runtime.bank().get_account(&world.er).unwrap();
    let payer_before = runtime.bank().get_account(&world.payer.pubkey()).unwrap();
    let cursor = Pubkey::find_program_address(
        &[
            northstar_token_bridge::state::TokenDepositProgress::CURSOR_SEED,
            world.er.as_ref(),
            &0u64.to_le_bytes(),
        ],
        &world.bridge,
    )
    .0;
    let lineage = plans
        .join("lineage-v2")
        .join(solana_hash::Hash::new_from_array(world.context).to_string());
    let saved = directory.path().join("retained-lineage");
    std::fs::rename(&lineage, &saved).unwrap();
    std::fs::write(&lineage, b"unavailable directory").unwrap();
    let failed = runtime
        .execute_token_ingress_v2(&world.bank, record)
        .is_err();
    let after = runtime.bank().get_account(&world.er).unwrap();
    let payer_after = runtime.bank().get_account(&world.payer.pubkey()).unwrap();
    let cursor_empty = runtime
        .bank()
        .get_account(&cursor)
        .is_none_or(|value| value.lamports() == 0 && value.data().is_empty());
    let active = runtime.is_active();
    let store = runtime._tx_client.lineage_store_v2().unwrap();
    let poisoned = store.lock().unwrap().is_poisoned();
    drop(store);
    manager.shutdown_runtime();
    std::fs::remove_file(&lineage).unwrap();
    std::fs::rename(&saved, &lineage).unwrap();
    assert!(failed);
    assert!(!active);
    assert!(poisoned);
    assert!(cursor_empty);
    assert_eq!(payer_before, payer_after);
    assert_eq!(
        before, after,
        "SVM credit must not commit ahead of durable input capture"
    );
}

#[test]
fn regression_v2_token_execution_requires_committed_payer_funding_before_admission() {
    let world =
        crate::portal_e2e_tests::token_bridge_v2::delegated_token_world(PORTAL_PROGRAM_ID, false);
    let source = world.funded_tokens(1_000);
    world.send(&[world.deposit(source, 42, 2)], &[]).unwrap();
    world.bank.freeze();
    let directory = tempfile::tempdir().unwrap();
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    });
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
    let runtime = manager.runtime.as_ref().unwrap();
    let record = runtime.process_l1_ingress_v2(&world.bank).unwrap().unwrap();
    let payer = world.payer.pubkey();
    let admitted_before = runtime.touched_accounts.read().unwrap().contains(&payer);
    let overlay_before = runtime
        .er_account_overlay
        .read()
        .unwrap()
        .get(&payer)
        .cloned();
    let rejected = runtime
        .execute_token_ingress_v2(&world.bank, record)
        .is_err();
    let admitted_after = runtime.touched_accounts.read().unwrap().contains(&payer);
    let overlay_after = runtime
        .er_account_overlay
        .read()
        .unwrap()
        .get(&payer)
        .cloned();
    let token: northstar_token_bridge::state::ErTokenAccount =
        borsh::from_slice(runtime.bank().get_account(&world.er).unwrap().data()).unwrap();
    manager.shutdown_runtime();
    assert!(rejected);
    assert_eq!(
        admitted_before, admitted_after,
        "rejected token input must not admit an unfunded L1 payer"
    );
    assert_eq!(overlay_before, overlay_after);
    assert_eq!(token.amount, 0);
}

#[test]
fn regression_v2_legacy_token_hints_cannot_skip_authenticated_inputs() {
    let world =
        crate::portal_e2e_tests::token_bridge_v2::delegated_token_world(PORTAL_PROGRAM_ID, true);
    let source = world.funded_tokens(1_000);
    world.send(&[world.deposit(source, 42, 3)], &[]).unwrap();
    world.bank.freeze();
    let directory = tempfile::tempdir().unwrap();
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    });
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
    let runtime = manager.runtime.as_ref().unwrap();
    assert!(runtime
        .process_l1_ingress_v2(&world.bank)
        .unwrap()
        .is_some());
    let before = runtime.bank().get_account(&world.er).unwrap();
    let state: northstar_token_bridge::state::ErTokenAccount =
        borsh::from_slice(before.data()).unwrap();
    let accepted = runtime.credit_token_deposit(
        &world.bridge,
        &Pubkey::new_from_array(state.session_bridge),
        &world.er,
        42,
    );
    runtime.process_token_deposits();
    let after = runtime.bank().get_account(&world.er).unwrap();
    let store = runtime._tx_client.lineage_store_v2().unwrap();
    let prefix = store
        .lock()
        .unwrap()
        .working_state()
        .unwrap()
        .ingress_count();
    drop(store);
    manager.shutdown_runtime();
    assert!(!accepted);
    assert_eq!(before, after);
    assert_eq!(prefix, 2);
}

#[test]
fn regression_v2_session_is_visible_to_l1_lifecycle_discovery() {
    let fixture = Fixture::new();
    let account = fixture.parent.get_account(&fixture.session).unwrap();
    let Some(PortalAccount::Session(session)) = try_parse_raw_portal_account(account.data()) else {
        panic!("authenticated continuity session must reach native lifecycle discovery");
    };
    assert_eq!(session.validator, fixture.manager.pubkey());
    assert_eq!(session.nonce, fixture.identity.nonce);
}

#[test]
fn regression_v2_manager_activation_installs_durable_lineage() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: fixture.manager.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    });
    manager
        .init_runtime(
            fixture.parent.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(fixture.parent.clone(), fixture.session, 0, 10_000, 1_000);
    let installed = manager
        .runtime
        .as_ref()
        .unwrap()
        ._tx_client
        .lineage_store_v2()
        .is_some();
    manager.shutdown_runtime();
    assert!(
        installed,
        "continuity activation must not fall back to the legacy capture journal"
    );
}

#[test]
fn invariant_v2_shutdown_releases_rpc_lineage_writer_before_return() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    for attempt in 0..4 {
        let path = directory.path().join(attempt.to_string());
        let mut runtime = fixture.runtime(&path, StartupModeV2::InitializeNew);
        let lineage = runtime.lineage_store_v2().unwrap();
        let retained = Arc::downgrade(&lineage);
        drop(lineage);
        runtime.shutdown();
        drop(runtime);
        assert!(
            retained.upgrade().is_none(),
            "a stopped RPC worker still owns the lineage writer"
        );
    }
}

#[test]
fn regression_v2_manager_resume_preserves_finalized_ingress() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let config = crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: fixture.manager.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    };
    let mut manager = crate::Manager::new(config.clone());
    manager
        .init_runtime(
            fixture.parent.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(fixture.parent.clone(), fixture.session, 0, 10_000, 1_000);
    manager.process_finalized_l1_inputs(&fixture.parent);
    let runtime = manager.runtime.as_ref().unwrap();
    assert_eq!(
        runtime.bank().get_balance(&fixture.payer.pubkey()),
        100_000_000
    );
    let expected = runtime
        ._tx_client
        .lineage_store_v2()
        .unwrap()
        .lock()
        .unwrap()
        .working_state()
        .unwrap();
    manager.deactivate_session();
    manager.shutdown_runtime();
    drop(manager);
    let mut manager = crate::Manager::new(config);
    manager
        .init_runtime(
            fixture.parent.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    assert!(manager.resume_active_session_from_l1(fixture.parent.clone()));
    manager.process_finalized_l1_inputs(&fixture.parent);
    let runtime = manager.runtime.as_ref().unwrap();
    assert_eq!(
        runtime.bank().get_balance(&fixture.payer.pubkey()),
        100_000_000
    );
    assert_eq!(
        runtime
            ._tx_client
            .lineage_store_v2()
            .unwrap()
            .lock()
            .unwrap()
            .working_state()
            .unwrap(),
        expected
    );
    manager.shutdown_runtime();
}

#[test]
fn regression_v2_manager_resume_never_initializes_missing_lineage() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let plans = directory.path().join("plans");
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: fixture.manager.clone(),
        checkpoint_plan_dir: Some(plans.clone()),
    });
    manager
        .init_runtime(
            fixture.parent.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    assert!(!manager.resume_active_session_from_l1(fixture.parent.clone()));
    assert!(!manager.has_active_runtime());
    assert!(!plans.exists());
    manager.shutdown_runtime();
}

#[test]
fn regression_v2_manager_proposes_the_durable_continuity_artifact() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: fixture.manager.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    });
    manager
        .init_runtime(
            fixture.parent.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(fixture.parent.clone(), fixture.session, 0, 10_000, 1_000);
    manager.process_finalized_l1_inputs(&fixture.parent);
    assert!(manager
        .runtime
        .as_ref()
        .unwrap()
        .seal_checkpoint_if_nonempty());
    let bank = Bank::new_from_parent(fixture.parent.clone(), SlotLeader::default(), 100);
    let first = manager
        .runtime
        .as_ref()
        .unwrap()
        .checkpoint_artifact_v2()
        .unwrap();
    let proposed = manager.settlement_transactions_if_due(&bank, bank.last_blockhash());
    let (_, _, transactions) =
        proposed.expect("sealed continuity state must produce a versioned checkpoint proposal");
    assert_eq!(transactions.len(), 1);
    let instruction: northstar_portal::PortalInstruction =
        borsh::from_slice(&transactions[0].message.instructions[0].data).unwrap();
    assert!(matches!(
        instruction,
        northstar_portal::PortalInstruction::ProposeCheckpointV2(_)
    ));
    for transaction in transactions {
        bank.process_transaction(&transaction).unwrap();
    }
    let bank = Bank::new_from_parent(
        Arc::new(bank),
        SlotLeader::default(),
        101 + crate::DEFAULT_CHECKPOINT_CHALLENGE_WINDOW_SLOTS,
    );
    let (_, _, transactions) = manager
        .settlement_transactions_if_due(&bank, bank.last_blockhash())
        .unwrap();
    for transaction in transactions {
        bank.process_transaction(&transaction).unwrap();
    }
    let slot = bank.slot() + 1;
    let bank = Bank::new_from_parent(Arc::new(bank), SlotLeader::default(), slot);
    let (_, _, transactions) = manager
        .settlement_transactions_if_due(&bank, bank.last_blockhash())
        .unwrap();
    for transaction in transactions {
        bank.process_transaction(&transaction).unwrap();
    }
    manager.process_finalized_l1_inputs(&bank);
    {
        let store = manager
            .runtime
            .as_ref()
            .unwrap()
            ._tx_client
            .lineage_store_v2()
            .unwrap();
        let store = store.lock().unwrap();
        assert!(store.sealed().is_none());
        assert_eq!(
            store.accepted_head().unwrap().state_root,
            first.checkpoint.new_state_root
        );
    }
    let frame: SessionV2 =
        borsh::from_slice(bank.get_account(&fixture.session).unwrap().data()).unwrap();
    assert_eq!(frame.base.settlement_status, SettlementStatus::Idle);
    let slot = bank.slot() + 20;
    let bank = Bank::new_from_parent(Arc::new(bank), SlotLeader::default(), slot);
    let context = fixture.identity.commitment(hash_parts).unwrap();
    let (receipt, _) = northstar_portal::find_deposit_receipt_v2_pda(
        &PORTAL_PROGRAM_ID,
        &context,
        &fixture.payer.pubkey(),
    );
    let (input, _) = northstar_portal::find_ingress_record_v2_pda(&PORTAL_PROGRAM_ID, &context, 2);
    let deposit = Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts: vec![
            solana_instruction::AccountMeta::new(fixture.manager.pubkey(), true),
            solana_instruction::AccountMeta::new(fixture.session, false),
            solana_instruction::AccountMeta::new(receipt, false),
            solana_instruction::AccountMeta::new_readonly(fixture.payer.pubkey(), false),
            solana_instruction::AccountMeta::new_readonly(system_program::id(), false),
            solana_instruction::AccountMeta::new(input, false),
        ],
        data: borsh::to_vec(&northstar_portal::PortalInstruction::DepositFeeV2 {
            lamports: 20_000_000,
        })
        .unwrap(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[deposit],
        Some(&fixture.manager.pubkey()),
        &[fixture.manager.as_ref()],
        bank.last_blockhash(),
    ))
    .unwrap();
    bank.freeze();
    let parent = Arc::new(bank);
    manager.reanchor_to_l1_parent(parent.clone());
    manager.process_finalized_l1_inputs(&parent);
    assert_eq!(
        manager
            .runtime
            .as_ref()
            .unwrap()
            .bank()
            .get_balance(&fixture.payer.pubkey()),
        120_000_000
    );
    let bank = Bank::new_from_parent(parent, SlotLeader::default(), slot + 1);
    let (_, _, transactions) = manager
        .settlement_transactions_if_due(&bank, bank.last_blockhash())
        .unwrap();
    let second = manager
        .runtime
        .as_ref()
        .unwrap()
        .checkpoint_artifact_v2()
        .unwrap();
    assert_eq!(
        second.checkpoint.previous_state_root,
        first.checkpoint.new_state_root
    );
    for transaction in transactions {
        bank.process_transaction(&transaction).unwrap();
    }
    manager.shutdown_runtime();
}

#[test]
fn regression_v2_ingress_and_execution_restore_without_l1_reimport() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lineage");
    let recipient = Pubkey::new_unique();
    let mut runtime = fixture.runtime(&path, StartupModeV2::InitializeNew);
    runtime.process_l1_ingress_v2(&fixture.parent).unwrap();
    assert_eq!(
        runtime.bank().get_balance(&fixture.payer.pubkey()),
        100_000_000
    );
    fixture.transfer(&runtime, recipient, 1_000_000);
    assert_eq!(runtime.bank().get_balance(&recipient), 1_000_000);
    let before = runtime
        ._tx_client
        .lineage_store_v2()
        .unwrap()
        .lock()
        .unwrap()
        .working_state()
        .unwrap();
    runtime.shutdown();
    drop(runtime);
    let mut runtime = fixture.runtime(&path, StartupModeV2::Resume);
    runtime.process_l1_ingress_v2(&fixture.parent).unwrap();
    assert_eq!(runtime.bank().get_balance(&recipient), 1_000_000);
    assert_eq!(
        runtime.bank().get_balance(&fixture.payer.pubkey()),
        99_000_000
    );
    assert_eq!(
        runtime
            ._tx_client
            .lineage_store_v2()
            .unwrap()
            .lock()
            .unwrap()
            .working_state()
            .unwrap(),
        before
    );
    fixture.transfer(&runtime, recipient, 2_000_000);
    assert_eq!(
        runtime.bank().get_balance(&recipient),
        3_000_000,
        "resumed bank slot={} retained slots={:?}",
        runtime.bank().slot(),
        runtime
            ._tx_client
            .lineage_store_v2()
            .unwrap()
            .lock()
            .unwrap()
            .unsealed()
            .iter()
            .map(|transition| transition.er_slot)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        runtime
            ._tx_client
            .lineage_store_v2()
            .unwrap()
            .lock()
            .unwrap()
            .unsealed()
            .len(),
        3
    );
    runtime.shutdown();
}

pub(super) fn assert_late_delegation_checkpoint_continuity() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let mut runtime = fixture.runtime(
        &directory.path().join("lineage"),
        StartupModeV2::InitializeNew,
    );
    runtime.process_l1_ingress_v2(&fixture.parent).unwrap();
    let recipient = Pubkey::new_unique();
    fixture.transfer(&runtime, recipient, 1_000_000);
    assert_eq!(runtime.bank().get_balance(&recipient), 1_000_000);
    assert!(runtime.seal_checkpoint_if_nonempty());
    let first = runtime.checkpoint_artifact_v2().unwrap();
    let head = crate::lineage_store_v2::L1CheckpointHeadV2 {
        checkpoint: northstar_portal::find_checkpoint_pda(
            &PORTAL_PROGRAM_ID,
            &fixture.session,
            first.checkpoint.er_slot,
        )
        .0
        .to_bytes(),
        er_slot: first.checkpoint.er_slot,
        state_root: first.checkpoint.new_state_root,
        checkpoint_hash: first.binding_hash().unwrap(),
    };
    {
        let store = runtime._tx_client.lineage_store_v2().unwrap();
        let mut store = store.lock().unwrap();
        store.record_accepted(head.clone()).unwrap();
        store.record_settled(&head).unwrap();
    }
    let delegated = Keypair::new();
    let parent = Bank::new_from_parent(fixture.parent.clone(), SlotLeader::default(), 2);
    let mut frame: SessionV2 =
        borsh::from_slice(parent.get_account(&fixture.session).unwrap().data()).unwrap();
    let context = fixture.identity.commitment(hash_parts).unwrap();
    let (record, bump) =
        northstar_portal::find_delegation_record_pda(&PORTAL_PROGRAM_ID, &delegated.pubkey());
    let (input, input_bump) =
        northstar_portal::find_ingress_record_v2_pda(&PORTAL_PROGRAM_ID, &context, 2);
    let ingress = IngressRecordV2::new(
        context,
        2,
        parent.slot(),
        frame.ingress_root,
        IngressPayloadV2::Delegation {
            account: delegated.pubkey().to_bytes(),
            record: record.to_bytes(),
            owner: system_program::id().to_bytes(),
            lamports: 100_000_000,
            data_len: 0,
            data_hash: hash_parts(&[&[]]),
            executable: false,
            rent_epoch: 0,
        },
        input_bump,
        hash_parts,
    )
    .unwrap();
    let binding = northstar_portal::DelegationRecord {
        discriminator: northstar_portal::DelegationRecord::DISCRIMINATOR,
        owner_program: system_program::id(),
        grid_id: 0,
        bump,
    };
    frame.consumed_ingress_count = 1;
    frame.consumed_ingress_root = frame.ingress_root;
    frame.ingress_count = 2;
    frame.ingress_root = ingress.root;
    for (key, bytes) in [
        (fixture.session, borsh::to_vec(&frame).unwrap()),
        (record, borsh::to_vec(&binding).unwrap()),
        (input, borsh::to_vec(&ingress).unwrap()),
    ] {
        parent.store_account(
            &key,
            &AccountSharedData::from(Account {
                lamports: parent.get_minimum_balance_for_rent_exemption(bytes.len()),
                data: bytes,
                owner: PORTAL_PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            }),
        );
    }
    parent.store_account(
        &delegated.pubkey(),
        &AccountSharedData::new(100_000_000, 0, &PORTAL_PROGRAM_ID),
    );
    parent.freeze();
    let parent = Arc::new(parent);
    runtime.reanchor_to_l1_parent(parent.clone());
    runtime.process_l1_ingress_v2(&parent).unwrap();
    let transaction = Transaction::new_signed_with_payer(
        &[transfer(&delegated.pubkey(), &recipient, 1_000_000)],
        Some(&delegated.pubkey()),
        &[&delegated],
        runtime.bank().last_blockhash(),
    );
    TransactionClient::send_transactions_in_batch(
        &runtime._tx_client,
        vec![bincode::serialize(&VersionedTransaction::from(transaction)).unwrap()],
        &SendTransactionServiceStats::default(),
    );
    assert_eq!(runtime.bank().get_balance(&recipient), 2_000_000);
    assert!(runtime.seal_checkpoint_if_nonempty());
    let second = runtime.checkpoint_artifact_v2().unwrap();
    runtime.shutdown();
    assert_eq!(
        second.checkpoint.previous_state_root,
        first.checkpoint.new_state_root
    );
    assert_eq!(second.pre.ingress_count, 1);
    assert_eq!(second.post.ingress_count, 2);
}

#[test]
fn regression_v2_legacy_deposit_hint_cannot_mutate_committed_state() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let mut runtime = fixture.runtime(
        &directory.path().join("lineage"),
        StartupModeV2::InitializeNew,
    );
    runtime.credit_deposit(&fixture.payer.pubkey(), 100_000_000);
    assert_eq!(
        runtime.bank().get_balance(&fixture.payer.pubkey()),
        0,
        "deposit discovery must wait for its authenticated ingress transition"
    );
    runtime.shutdown();
}

#[test]
fn regression_v2_legacy_delegation_hint_cannot_mutate_committed_state() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let mut runtime = fixture.runtime(
        &directory.path().join("lineage"),
        StartupModeV2::InitializeNew,
    );
    runtime.handle_delegation_inner(
        &fixture.payer.pubkey(),
        AccountSharedData::new(100_000_000, 0, &PORTAL_PROGRAM_ID),
        Some(system_program::id()),
        Some(&fixture.parent),
    );
    assert_eq!(
        runtime.bank().get_balance(&fixture.payer.pubkey()),
        0,
        "delegation discovery must wait for its authenticated ingress transition"
    );
    assert!(!runtime
        .delegated_accounts()
        .contains(&fixture.payer.pubkey()));
    runtime.shutdown();
}

#[test]
fn regression_v2_persistence_failure_cancels_svm_commit() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lineage");
    let mut runtime = fixture.runtime(&path, StartupModeV2::InitializeNew);
    runtime.process_l1_ingress_v2(&fixture.parent).unwrap();
    let capsule = path.join("lineage-v2.borsh");
    std::fs::rename(&capsule, path.join("retained.borsh")).unwrap();
    std::fs::create_dir(&capsule).unwrap();
    let recipient = Pubkey::new_unique();
    fixture.transfer(&runtime, recipient, 1_000_000);
    assert_eq!(runtime.bank().get_balance(&recipient), 0);
    assert_eq!(
        runtime.bank().get_balance(&fixture.payer.pubkey()),
        100_000_000
    );
    assert!(!runtime.is_active());
    runtime.shutdown();
}
