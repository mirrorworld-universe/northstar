use {
    super::{live_checkpoint::PORTAL, tests::supported_sbf_checkpoint_with_target_offset},
    crate::{
        checkpoint::CheckpointArtifactV1,
        settlement::{SettlementChunk, SettlementPlan},
    },
    base64::{engine::general_purpose::STANDARD, Engine},
    borsh::BorshDeserialize,
    solana_account::{AccountSharedData, ReadableAccount},
    solana_commitment_config::CommitmentConfig,
    solana_keypair::Keypair,
    solana_message::VersionedMessage,
    solana_pubkey::Pubkey,
    solana_rpc::er_history::ErHistoryStore,
    solana_rpc_client::rpc_client::RpcClient,
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::{
        collections::BTreeMap,
        env, fs,
        path::Path,
        thread::sleep,
        time::{Duration, Instant},
    },
};

fn changed_accounts(
    artifact: &CheckpointArtifactV1,
    history: &ErHistoryStore,
) -> BTreeMap<Pubkey, (AccountSharedData, AccountSharedData)> {
    let mut accounts = BTreeMap::new();
    for page in &artifact.da.pages {
        let transaction: VersionedTransaction = bincode::deserialize(&page.transaction).unwrap();
        let capture = history
            .get_replay_capture(&transaction.signatures[0], CommitmentConfig::finalized())
            .unwrap();
        for account in &capture.accounts {
            if account.pre_account == account.post_account {
                continue;
            }
            assert_eq!(
                account.pre_account.lamports(),
                account.post_account.lamports()
            );
            assert_eq!(account.pre_account.owner(), account.post_account.owner());
            assert_eq!(
                account.pre_account.data().len(),
                account.post_account.data().len()
            );
            if account.pre_account.data() == account.post_account.data() {
                continue;
            }
            assert!(!account.pre_account.executable());
            assert!(
                accounts
                    .insert(
                        account.key,
                        (account.pre_account.clone(), account.post_account.clone())
                    )
                    .is_none(),
                "fixture changes each target exactly once"
            );
        }
    }
    assert_eq!(accounts.len(), artifact.da.pages.len());
    accounts
}

#[test]
#[ignore = "exports trusted genesis delegation fixtures for the real SBF live settlement test"]
fn export_live_settlement_genesis() {
    let directory = env::var("NORTHSTAR_LIVE_GENESIS_DIR").unwrap();
    fs::create_dir(&directory).unwrap();
    let session = northstar_portal::find_session_pda(&PORTAL).0;
    let steps = env::var("NORTHSTAR_LIVE_STEP_COUNT")
        .map(|value| value.parse().unwrap())
        .unwrap_or(16);
    let (artifact, history) = supported_sbf_checkpoint_with_target_offset(0, session, steps, 128);
    for (key, (pre, _)) in changed_accounts(&artifact, &history) {
        let (record_key, bump) = northstar_portal::find_delegation_record_pda(&PORTAL, &key);
        let record = northstar_portal::DelegationRecord {
            discriminator: northstar_portal::DelegationRecord::DISCRIMINATOR,
            owner_program: *pre.owner(),
            grid_id: 1,
            bump,
        };
        for (key, lamports, data) in [
            (key, pre.lamports(), pre.data().to_vec()),
            (record_key, 10_000_000, borsh::to_vec(&record).unwrap()),
        ] {
            let account = serde_json::json!({"pubkey":key.to_string(), "account":{
                "lamports":lamports, "data":[STANDARD.encode(data), "base64"],
                "owner":PORTAL.to_string(), "executable":false, "rentEpoch":u64::MAX
            }});
            fs::write(
                Path::new(&directory).join(format!("{key}.json")),
                serde_json::to_vec_pretty(&account).unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
#[ignore = "requires the compiled test-only replay owner SBF"]
fn fresh_delegation_owner_preserves_frozen_replay_relation() {
    agave_logger::setup_with_default("warn");
    let owner = env::var("NORTHSTAR_LIVE_OWNER_SBF").unwrap();
    let session = northstar_portal::find_session_pda(&PORTAL).0;
    let (artifact, _) =
        super::tests::supported_sbf_checkpoint_with_owner(0, session, 3, 128, Some(&owner));
    assert_eq!(artifact.checkpoint.step_count, 3);
}

#[test]
#[ignore = "requires a fresh live validator with Portal and replay owner SBF"]
fn live_fresh_delegation_cpi() {
    use {
        solana_instruction::AccountMeta, solana_keypair::read_keypair_file,
        solana_sdk_ids::system_program,
    };
    let payer = read_keypair_file(env::var("NORTHSTAR_LIVE_PAYER").unwrap()).unwrap();
    let rpc = RpcClient::new_with_commitment(
        env::var("NORTHSTAR_LIVE_RPC_URL").unwrap(),
        CommitmentConfig::confirmed(),
    );
    let session = northstar_portal::find_session_pda(&PORTAL).0;
    let fee_vault = northstar_portal::find_fee_vault_pda(&PORTAL).0;
    super::live_checkpoint::send(
        &rpc,
        &payer,
        &[&payer],
        &[super::live_checkpoint::instruction(
            vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new(session, false),
                AccountMeta::new(fee_vault, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            northstar_portal::PortalInstruction::OpenSession(northstar_portal::OpenSession {
                grid_id: 1,
                ttl_slots: 20_000,
                fee_cap: 1_000_000_000,
                validator: payer.pubkey(),
                settlement_interval_slots: 75,
            }),
        )],
    );
    let owner = env::var("NORTHSTAR_LIVE_OWNER_SBF").unwrap();
    let (artifact, history) =
        super::tests::supported_sbf_checkpoint_with_owner(0, session, 3, 128, Some(&owner));
    delegate_fresh_fixture(&rpc, &payer, &artifact, &history);
}

pub(super) fn delegate_fresh_fixture(
    rpc: &RpcClient,
    payer: &Keypair,
    artifact: &CheckpointArtifactV1,
    history: &ErHistoryStore,
) {
    use {
        solana_instruction::{AccountMeta, Instruction},
        solana_keypair::keypair_from_seed,
        solana_sdk_ids::system_program,
    };
    let accounts = changed_accounts(artifact, history);
    for index in 0..accounts.len() {
        let target = keypair_from_seed(&[index as u8 + 128; 32]).unwrap();
        let buffer = Keypair::new();
        let (pre, _) = &accounts[&target.pubkey()];
        let owner = *pre.owner();
        let record = northstar_portal::find_delegation_record_pda(&PORTAL, &target.pubkey()).0;
        assert!(rpc
            .get_account_with_commitment(&record, CommitmentConfig::confirmed())
            .unwrap()
            .value
            .is_none());
        assert!(rpc
            .get_account_with_commitment(&target.pubkey(), CommitmentConfig::confirmed())
            .unwrap()
            .value
            .is_none());
        super::live_checkpoint::send(
            rpc,
            payer,
            &[payer, &target, &buffer],
            &[
                solana_system_interface::instruction::create_account(
                    &payer.pubkey(),
                    &target.pubkey(),
                    pre.lamports(),
                    pre.data().len() as u64,
                    &owner,
                ),
                solana_system_interface::instruction::create_account(
                    &payer.pubkey(),
                    &buffer.pubkey(),
                    rpc.get_minimum_balance_for_rent_exemption(pre.data().len())
                        .unwrap(),
                    pre.data().len() as u64,
                    &owner,
                ),
            ],
        );
        let mut data = vec![0];
        data.extend_from_slice(&1u64.to_le_bytes());
        super::live_checkpoint::send(
            rpc,
            payer,
            &[payer, &target],
            &[Instruction::new_with_bytes(
                owner,
                &data,
                vec![
                    AccountMeta::new(target.pubkey(), true),
                    AccountMeta::new(buffer.pubkey(), false),
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new_readonly(artifact.checkpoint.session, false),
                    AccountMeta::new(record, false),
                    AccountMeta::new_readonly(owner, false),
                    AccountMeta::new_readonly(PORTAL, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                ],
            )],
        );
        let delegated = rpc.get_account(&target.pubkey()).unwrap();
        assert_eq!(delegated.owner, PORTAL);
        assert_eq!(delegated.data, pre.data());
        assert_eq!(delegated.lamports, pre.lamports());
        let record = northstar_portal::DelegationRecord::try_from_slice(
            &rpc.get_account(&record).unwrap().data,
        )
        .unwrap();
        assert_eq!(record.owner_program.to_bytes(), owner.to_bytes());
        assert_eq!(record.grid_id, 1);
        assert_eq!(rpc.get_account(&buffer.pubkey()).unwrap().data, pre.data());
    }
    println!(
        "NORTHSTAR_TIMING {}",
        serde_json::json!({"phase":"fresh_delegation", "accounts":accounts.len(), "owner_program_cpi":true, "genesis_delegation_fixtures":false})
    );
}

pub(super) fn settle_resolved_fixture(
    rpc: &RpcClient,
    payer: &Keypair,
    artifact: &CheckpointArtifactV1,
    history: &ErHistoryStore,
    directory: &Path,
) {
    let started = Instant::now();
    let accounts = changed_accounts(artifact, history);
    let session = artifact.checkpoint.session;
    let slot = artifact.checkpoint.er_slot;
    let checkpoint_key = northstar_portal::find_checkpoint_pda(&PORTAL, &session, slot).0;
    let cursor_key = northstar_portal::find_checkpoint_cursor_pda(&PORTAL, &session).0;
    let checkpoint = northstar_portal::Checkpoint::try_from_slice(
        &rpc.get_account(&checkpoint_key).unwrap().data,
    )
    .unwrap();
    assert!(checkpoint.challenge_resolved);
    assert_eq!(
        checkpoint.status,
        northstar_portal::CheckpointStatus::Pending
    );
    let mut plan = SettlementPlan {
        er_slot: slot,
        checksum: [0; 32],
        chunks: accounts
            .iter()
            .map(|(key, (_, post))| SettlementChunk {
                account: *key,
                account_data_offset: 0,
                data: post.data().to_vec(),
            })
            .collect(),
        owner_changes: vec![],
        lamport_changes: vec![],
        receipt_balances: vec![],
        token_withdrawals: vec![],
        unsupported_changes: vec![],
    };
    plan.checksum = plan.recomputed_checksum();
    let mut before = BTreeMap::new();
    for (key, (pre, _)) in &accounts {
        let account = rpc
            .get_account(key)
            .expect("load delegated fixture accounts");
        assert_eq!(account.owner, PORTAL);
        assert_eq!(account.data, pre.data());
        assert_eq!(account.lamports, pre.lamports());
        before.insert(*key, account);
    }
    if let Ok(plan_directory) = env::var("NORTHSTAR_LIVE_MANAGER_PLAN_DIR") {
        let plan_directory = Path::new(&plan_directory);
        fs::create_dir_all(plan_directory).unwrap();
        let plan_path = plan_directory.join(format!(
            "{PORTAL}-{session}-{}-{slot}.borsh",
            payer.pubkey()
        ));
        assert!(!plan_path.exists());
        // The running manager removes malformed plans, so never expose a partial write.
        let temporary = plan_path.with_extension(format!("borsh.tmp.{}", std::process::id()));
        let bytes = borsh::to_vec(&crate::DurableSettlementPlan::from(&plan)).unwrap();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.sync_all().unwrap();
        fs::rename(temporary, plan_path).unwrap();
        fs::File::open(plan_directory).unwrap().sync_all().unwrap();
        let expected = accounts
            .iter()
            .map(|(key, (_, post))| {
                let mut account = before[key].clone();
                account.data = post.data().to_vec();
                (*key, account)
            })
            .collect::<Vec<_>>();
        let checkpoint_lamports = rpc.get_account(&checkpoint_key).unwrap().lamports;
        fs::write(
            directory.join("manager-observation.bin"),
            bincode::serialize(&(
                session,
                slot,
                checkpoint_lamports,
                checkpoint.bond_lamports,
                expected,
            ))
            .unwrap(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let rooted = rpc
                .get_account_with_commitment(&checkpoint_key, CommitmentConfig::finalized())
                .unwrap();
            if rooted
                .value
                .as_ref()
                .is_some_and(|account| account.data == borsh::to_vec(&checkpoint).unwrap())
            {
                fs::write(
                    env::var("NORTHSTAR_LIVE_MANAGER_RESTART_READY").unwrap(),
                    rooted.context.slot.to_string(),
                )
                .unwrap();
                break;
            }
            assert!(
                Instant::now() < deadline,
                "resolved checkpoint must finalize before restart"
            );
            sleep(Duration::from_millis(200));
        }
        println!(
            "NORTHSTAR_TIMING {}",
            serde_json::json!({"phase":"manager_handoff", "driver_exits_before_restart":true, "genesis_delegation_fixtures":false})
        );
        return;
    }
    while rpc.get_slot().unwrap() < checkpoint.challenge_deadline_l1_slot {
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "natural checkpoint deadline"
        );
        sleep(Duration::from_millis(200));
    }
    let payer_before = rpc.get_balance(&payer.pubkey()).unwrap();
    let checkpoint_lamports_before = rpc.get_account(&checkpoint_key).unwrap().lamports;
    let commit = plan.checkpoint_commit_transaction(
        PORTAL,
        session,
        payer,
        rpc.get_latest_blockhash().unwrap(),
    );
    let mut fees = rpc
        .get_fee_for_versioned_message(&VersionedMessage::Legacy(commit.message.clone()))
        .unwrap();
    rpc.send_and_confirm_transaction(&commit).unwrap();
    let plan_path = directory.join("settlement-plan.borsh");
    fs::write(
        &plan_path,
        borsh::to_vec(&crate::DurableSettlementPlan::from(&plan)).unwrap(),
    )
    .unwrap();
    let initial = plan.portal_transactions_with_effect_commitment(
        PORTAL,
        session,
        payer,
        rpc.get_latest_blockhash().unwrap(),
        artifact.checkpoint.effect_commitment,
        None,
    );
    assert!(initial.len() > 1);
    fees += rpc
        .get_fee_for_versioned_message(&VersionedMessage::Legacy(initial[0].message.clone()))
        .unwrap();
    rpc.send_and_confirm_transaction(&initial[0]).unwrap();
    restart_after_account(rpc, &session, "SETTLEMENT");
    let restored: crate::DurableSettlementPlan =
        borsh::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    let restored = SettlementPlan::from(restored);
    assert_eq!(restored, plan);
    // Reapply already-acknowledged chunks, as a restart with a lost acknowledgement would.
    let retries = restored.portal_transactions_with_effect_commitment(
        PORTAL,
        session,
        payer,
        rpc.get_latest_blockhash().unwrap(),
        artifact.checkpoint.effect_commitment,
        Some(
            northstar_portal::Session::try_from_slice(&rpc.get_account(&session).unwrap().data)
                .unwrap()
                .settlement_accumulator,
        ),
    );
    for transaction in retries {
        fees += rpc
            .get_fee_for_versioned_message(&VersionedMessage::Legacy(transaction.message.clone()))
            .unwrap();
        rpc.send_and_confirm_transaction(&transaction).unwrap();
    }
    let terminal = northstar_portal::Checkpoint::try_from_slice(
        &rpc.get_account(&checkpoint_key).unwrap().data,
    )
    .unwrap();
    assert_eq!(terminal.status, northstar_portal::CheckpointStatus::Settled);
    assert_eq!(
        terminal.bond_status,
        northstar_portal::CheckpointBondStatus::Released
    );
    assert_eq!(
        rpc.get_account(&checkpoint_key).unwrap().lamports + checkpoint.bond_lamports,
        checkpoint_lamports_before
    );
    assert_eq!(
        rpc.get_balance(&payer.pubkey()).unwrap() + fees,
        payer_before + checkpoint.bond_lamports
    );
    let session_state =
        northstar_portal::Session::try_from_slice(&rpc.get_account(&session).unwrap().data)
            .unwrap();
    assert_eq!(session_state.last_settled_er_slot, slot);
    let cursor = northstar_portal::CheckpointCursor::try_from_slice(
        &rpc.get_account(&cursor_key).unwrap().data,
    )
    .unwrap();
    assert_eq!(cursor.active_er_slot, 0);
    assert_eq!(cursor.latest_finalized_er_slot, slot);
    assert_eq!(
        cursor.latest_finalized_state_root,
        artifact.checkpoint.new_state_root
    );
    for (key, (_, post)) in &accounts {
        let mut expected = before[key].clone();
        expected.data = post.data().to_vec();
        assert_eq!(rpc.get_account(key).unwrap(), expected);
    }
    let summary = serde_json::json!({"schema_version":1, "phase":"proof_to_settlement", "wall_ms":started.elapsed().as_millis(), "slot_warps":false, "settled":true, "bond_released":true, "changed_accounts":accounts.len(), "genesis_delegation_fixtures":env::var_os("NORTHSTAR_LIVE_OWNER_SBF").is_none()});
    fs::write(
        directory.join("settlement.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("NORTHSTAR_TIMING {summary}");
}

#[test]
#[ignore = "observes automatic settlement after the proof driver exits and the validator restarts"]
fn observe_manager_recovery_after_proof() {
    let directory = env::var("NORTHSTAR_LIVE_PROOF_DIR").unwrap();
    let directory = Path::new(&directory);
    let (session, slot, original_lamports, bond, accounts): (
        Pubkey,
        u64,
        u64,
        u64,
        Vec<(Pubkey, solana_account::Account)>,
    ) = bincode::deserialize(&fs::read(directory.join("manager-observation.bin")).unwrap())
        .unwrap();
    let rpc = RpcClient::new_with_commitment(
        env::var("NORTHSTAR_LIVE_RPC_URL").unwrap(),
        CommitmentConfig::finalized(),
    );
    let checkpoint_key = northstar_portal::find_checkpoint_pda(&PORTAL, &session, slot).0;
    let cursor_key = northstar_portal::find_checkpoint_cursor_pda(&PORTAL, &session).0;
    let started = Instant::now();
    let settled = loop {
        let account = rpc.get_account(&checkpoint_key).unwrap();
        let checkpoint = northstar_portal::Checkpoint::try_from_slice(&account.data).unwrap();
        if checkpoint.status == northstar_portal::CheckpointStatus::Settled {
            assert_eq!(
                checkpoint.bond_status,
                northstar_portal::CheckpointBondStatus::Released
            );
            assert!(checkpoint.challenge_resolved);
            assert_eq!(account.lamports + bond, original_lamports);
            break checkpoint;
        }
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "manager must finish without a transaction-submitting test driver"
        );
        sleep(Duration::from_millis(200));
    };
    let session_state =
        northstar_portal::Session::try_from_slice(&rpc.get_account(&session).unwrap().data)
            .unwrap();
    assert_eq!(session_state.last_settled_er_slot, slot);
    let cursor = northstar_portal::CheckpointCursor::try_from_slice(
        &rpc.get_account(&cursor_key).unwrap().data,
    )
    .unwrap();
    assert_eq!(cursor.active_er_slot, 0);
    assert_eq!(cursor.latest_finalized_er_slot, slot);
    assert_eq!(cursor.latest_finalized_state_root, settled.new_state_root);
    for (key, expected) in accounts {
        assert_eq!(rpc.get_account(&key).unwrap(), expected);
    }
    let summary = serde_json::json!({"phase":"automatic_manager_recovery", "wall_ms":started.elapsed().as_millis(), "settled":true, "bond_released":true, "observer_submits_transactions":false, "genesis_delegation_fixtures":false, "slot_warps":false});
    fs::write(
        directory.join("manager-recovery.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("NORTHSTAR_TIMING {summary}");
}

pub(super) fn restart_after_account(rpc: &RpcClient, account: &Pubkey, phase: &str) {
    let Ok(ready) = env::var(format!("NORTHSTAR_LIVE_{phase}_RESTART_READY")) else {
        return;
    };
    let resume = env::var(format!("NORTHSTAR_LIVE_{phase}_RESTART_RESUME")).unwrap();
    let applied = rpc.get_account(account).unwrap();
    let wait_started = Instant::now();
    loop {
        let response = rpc
            .get_account_with_commitment(account, CommitmentConfig::finalized())
            .unwrap();
        if response.value.as_ref() == Some(&applied) {
            fs::write(&ready, response.context.slot.to_string()).unwrap();
            break;
        }
        assert!(wait_started.elapsed() < Duration::from_secs(60));
        sleep(Duration::from_millis(200));
    }
    while !Path::new(&resume).exists() {
        assert!(wait_started.elapsed() < Duration::from_secs(240));
        sleep(Duration::from_millis(200));
    }
    let rpc_started = Instant::now();
    while rpc.get_account(account).ok().as_ref() != Some(&applied) {
        assert!(rpc_started.elapsed() < Duration::from_secs(60));
        sleep(Duration::from_millis(200));
    }
    let summary = serde_json::json!({"schema_version":1, "phase":format!("{}_recovery",phase.to_lowercase()), "wall_ms":wait_started.elapsed().as_millis(), "account_unchanged":true, "slot_warps":false});
    let directory = env::var("NORTHSTAR_LIVE_PROOF_DIR").unwrap();
    fs::write(
        Path::new(&directory).join(format!("{}-recovery.json", phase.to_lowercase())),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("NORTHSTAR_TIMING {summary}");
}
