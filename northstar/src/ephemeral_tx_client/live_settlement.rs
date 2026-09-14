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
            .expect("load exported genesis delegation fixtures");
        assert_eq!(account.owner, PORTAL);
        assert_eq!(account.data, pre.data());
        assert_eq!(account.lamports, pre.lamports());
        before.insert(*key, account);
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
    let mut fees = rpc.get_fee_for_message(&commit.message).unwrap();
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
        true,
    );
    assert!(initial.len() > 1);
    fees += rpc.get_fee_for_message(&initial[0].message).unwrap();
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
        false,
    );
    for transaction in retries {
        fees += rpc.get_fee_for_message(&transaction.message).unwrap();
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
    let summary = serde_json::json!({"schema_version":1, "phase":"proof_to_settlement", "wall_ms":started.elapsed().as_millis(), "slot_warps":false, "settled":true, "bond_released":true, "changed_accounts":accounts.len(), "genesis_delegation_fixtures":true});
    fs::write(
        directory.join("settlement.json"),
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
