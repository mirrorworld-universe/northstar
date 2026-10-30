use {
    super::*,
    crate::{
        checkpoint_v2::build_checkpoint_artifact_v2,
        continuity_v2::{hash_parts, CommittedStateV2, RecordedTransitionV2, TransitionPayloadV2},
    },
    borsh::BorshDeserialize,
    northstar_zk_types::continuity::IngressRecordV2,
};

#[test]
fn regression_v2_portal_authenticates_prefix_and_commits_its_consumed_head() {
    setup();
    let (bank, _forks, program_id, payer) = setup_bank_with_portal();
    let payer_key = payer.pubkey();
    let (session, _) = find_session_pda(&program_id);
    let (fee_vault, _) = find_fee_vault_pda(&program_id);
    let (epoch, _) = northstar_portal::find_session_epoch_v2_pda(&program_id);
    let open = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(payer_key, true),
            AccountMeta::new(session, false),
            AccountMeta::new(fee_vault, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(epoch, false),
        ],
        data: borsh::to_vec(&PortalInstruction::OpenSessionV2(
            northstar_portal::OpenSessionV2 {
                session: OpenSession {
                    grid_id: 0,
                    ttl_slots: 10_000,
                    fee_cap: 1_000,
                    validator: payer_key,
                    settlement_interval_slots: 10,
                },
                l1_genesis: [1; 32],
                profile: [2; 32],
            },
        ))
        .unwrap(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[open],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let frame =
        northstar_portal::SessionV2::try_from_slice(bank.get_account(&session).unwrap().data())
            .unwrap();
    assert!(frame.is_valid());
    assert_eq!(frame.base.nonce, 1);
    let identity = frame.identity(&program_id, &session);
    let context = identity.commitment(hash_parts).unwrap();
    let recipient = Pubkey::new_unique();
    let (receipt, _) =
        northstar_portal::find_deposit_receipt_v2_pda(&program_id, &context, &recipient);
    let deposit = |sequence, amount| {
        let (ingress, _) =
            northstar_portal::find_ingress_record_v2_pda(&program_id, &context, sequence);
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(payer_key, true),
                AccountMeta::new(session, false),
                AccountMeta::new(receipt, false),
                AccountMeta::new_readonly(recipient, false),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new(ingress, false),
            ],
            data: borsh::to_vec(&PortalInstruction::DepositFeeV2 { lamports: amount }).unwrap(),
        }
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[deposit(1, 10_000_000)],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let (prefix, _) = northstar_portal::find_ingress_record_v2_pda(&program_id, &context, 1);
    let record =
        IngressRecordV2::try_from_slice(bank.get_account(&prefix).unwrap().data()).unwrap();
    record
        .validate_next(context, 0, [0; 32], hash_parts)
        .unwrap();
    let receipt_state = northstar_portal::DepositReceiptV2::try_from_slice(
        bank.get_account(&receipt).unwrap().data(),
    )
    .unwrap();
    assert_eq!(receipt_state.total_deposited, 10_000_000);
    assert_eq!(receipt_state.base.balance, 0);
    let base = CommittedStateV2::new(identity).unwrap();
    let mut post = base.clone();
    post.apply_ingress(&record, &[]).unwrap();
    let er_slot = 100;
    let step = RecordedTransitionV2 {
        er_slot,
        before: base.opening().unwrap(),
        after: post.opening().unwrap(),
        payload: TransitionPayloadV2::Ingress {
            record,
            account_data: vec![],
        },
        readonly_l1_values: vec![],
        settlement_effects: vec![],
    };
    let artifact = build_checkpoint_artifact_v2(&base, er_slot, &[step]).unwrap();
    let header = artifact.checkpoint;
    let (checkpoint, _) = find_checkpoint_pda(&program_id, &session, er_slot);
    let directory = tempfile::tempdir().unwrap();
    let state_path = directory.path().join("lineage");
    let mut journal = crate::lineage_store_v2::CheckpointLineageStoreV2::open(
        &state_path,
        identity,
        None,
        crate::lineage_store_v2::StartupModeV2::InitializeNew,
    )
    .unwrap();
    journal
        .append(artifact.pages[0].transition.clone())
        .unwrap();
    journal
        .seal(
            checkpoint.to_bytes(),
            er_slot,
            borsh::to_vec(&artifact).unwrap(),
        )
        .unwrap();
    let (cursor, _) = find_checkpoint_cursor_pda(&program_id, &session);
    let (binding, _) = northstar_portal::find_checkpoint_binding_v2_pda(&program_id, &checkpoint);
    let proposal = northstar_portal::ProposeCheckpoint {
        er_slot,
        step_count: u64::from(header.step_count),
        previous_state_root: header.previous_state_root,
        new_state_root: header.new_state_root,
        trace_root: header.trace_root,
        tx_effect_root: header.transaction_effect_root,
        readonly_l1_root: header.readonly_l1_root,
        da_commitment: header.da_commitment,
        effect_commitment: header.effect_commitment,
        challenge_window_slots: 1,
    };
    let propose = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(payer_key, true),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(checkpoint, false),
            AccountMeta::new(cursor, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(binding, false),
            AccountMeta::new_readonly(prefix, false),
        ],
        data: borsh::to_vec(&PortalInstruction::ProposeCheckpointV2(
            northstar_portal::ProposeCheckpointV2 {
                proposal,
                pre: artifact.pre,
                post: artifact.post,
                readonly_l1_root: artifact.readonly_l1_root,
            },
        ))
        .unwrap(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[propose],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[deposit(2, 5_000_000)],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let bank = Bank::new_from_parent(bank, SlotLeader::default(), 10);
    let commit_accounts = vec![
        AccountMeta::new(payer_key, true),
        AccountMeta::new(session, false),
        AccountMeta::new(checkpoint, false),
        AccountMeta::new(cursor, false),
        AccountMeta::new(payer_key, false),
    ];
    let legacy = Instruction {
        program_id,
        accounts: commit_accounts.clone(),
        data: borsh::to_vec(&PortalInstruction::CommitCheckpoint(CommitCheckpoint {
            er_slot,
        }))
        .unwrap(),
    };
    assert!(bank
        .process_transaction(&Transaction::new_signed_with_payer(
            &[legacy],
            Some(&payer_key),
            &[&payer],
            bank.last_blockhash()
        ))
        .is_err());
    let mut accounts = commit_accounts;
    accounts.push(AccountMeta::new_readonly(binding, false));
    let commit = Instruction {
        program_id,
        accounts,
        data: borsh::to_vec(&PortalInstruction::CommitCheckpointV2(CommitCheckpoint {
            er_slot,
        }))
        .unwrap(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[commit],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let cursor_state = northstar_portal::CheckpointCursor::try_from_slice(
        bank.get_account(&cursor).unwrap().data(),
    )
    .unwrap();
    let frame =
        northstar_portal::SessionV2::try_from_slice(bank.get_account(&session).unwrap().data())
            .unwrap();
    assert_eq!(
        cursor_state.latest_finalized_state_root,
        header.new_state_root
    );
    assert_eq!(frame.consumed_ingress_count, 1);
    assert_eq!(frame.consumed_ingress_root, record.root);
    assert_eq!(frame.ingress_count, 2);
    assert_ne!(frame.ingress_root, frame.consumed_ingress_root);
    let receipt_state = northstar_portal::DepositReceiptV2::try_from_slice(
        bank.get_account(&receipt).unwrap().data(),
    )
    .unwrap();
    assert_eq!(receipt_state.total_deposited, 15_000_000);
    let (head, _) =
        crate::lineage_l1_v2::checkpoint_head_from_l1_v2(&bank, &program_id, &session, &frame)
            .unwrap()
            .unwrap();
    assert_eq!(head.checkpoint_hash, artifact.binding_hash().unwrap());
    journal.record_accepted(head.clone()).unwrap();
    assert_eq!(journal.accepted_state(), &post);
    let mut plan = SettlementPlan {
        er_slot,
        checksum: [0; 32],
        chunks: vec![],
        owner_changes: vec![],
        lamport_changes: vec![],
        receipt_balances: vec![],
        token_withdrawals: vec![],
        unsupported_changes: vec![],
    };
    plan.checksum = plan.recomputed_checksum();
    let bank = Bank::new_from_parent(Arc::new(bank), SlotLeader::default(), 11);
    for transaction in plan.portal_transactions_with_effect_commitment(
        program_id,
        session,
        &payer,
        bank.last_blockhash(),
        header.effect_commitment,
        None,
    ) {
        bank.process_transaction(&transaction).unwrap();
    }
    let frame =
        northstar_portal::SessionV2::try_from_slice(bank.get_account(&session).unwrap().data())
            .unwrap();
    let (settled_head, checkpoint_state) =
        crate::lineage_l1_v2::checkpoint_head_from_l1_v2(&bank, &program_id, &session, &frame)
            .unwrap()
            .unwrap();
    assert_eq!(settled_head, head);
    assert_eq!(
        checkpoint_state.status,
        northstar_portal::CheckpointStatus::Settled
    );
    assert_eq!(frame.base.last_settled_er_slot, er_slot);
    assert_eq!(frame.consumed_ingress_count, 1);
    assert_eq!(frame.ingress_count, 2);
    journal.record_settled(&head).unwrap();
    drop(journal);
    let journal = crate::lineage_store_v2::CheckpointLineageStoreV2::open(
        &state_path,
        identity,
        Some(head),
        crate::lineage_store_v2::StartupModeV2::Resume,
    )
    .unwrap();
    assert!(journal.sealed().is_none());
    assert_eq!(journal.working_state().unwrap(), post);
}
