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
