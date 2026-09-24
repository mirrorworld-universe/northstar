use {
    super::live_checkpoint::{instruction, send, PORTAL},
    borsh::BorshDeserialize,
    northstar_portal::{
        Checkpoint, CheckpointBondStatus, CheckpointCursor, CheckpointStatus, OpenSession,
        PortalInstruction, Session, SettlementStatus,
    },
    solana_commitment_config::CommitmentConfig,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::{read_keypair_file, Keypair},
    solana_pubkey::Pubkey,
    solana_rpc_client::rpc_client::RpcClient,
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_transaction::Transaction,
    std::{
        env, fs,
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    },
};

const OWNER: Pubkey = solana_pubkey::pubkey!("FpuSfMKs3Bf5bxFZJ8UDDVYTbCGnZURDuLBmhjb5u9XC");

fn poll<T>(label: &str, seconds: u64, mut action: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = action() {
            return value;
        }
        assert!(
            started.elapsed() < Duration::from_secs(seconds),
            "timed out: {label}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn rpc() -> RpcClient {
    RpcClient::new_with_commitment(
        env::var("NORTHSTAR_LIVE_RPC_URL").unwrap(),
        CommitmentConfig::confirmed(),
    )
}
fn directory() -> PathBuf {
    PathBuf::from(env::var_os("NORTHSTAR_COORDINATOR_EVIDENCE").unwrap())
}

#[test]
#[ignore = "requires live opt-in coordinator, owner program, and a separate challenger key"]
fn live_coordinator_drive_er_checkpoint() {
    let rpc = rpc();
    let er = RpcClient::new_with_commitment(
        env::var("NORTHSTAR_LIVE_ER_RPC_URL").unwrap(),
        CommitmentConfig::confirmed(),
    );
    let payer = read_keypair_file(env::var_os("NORTHSTAR_LIVE_PAYER").unwrap()).unwrap();
    let challenger =
        read_keypair_file(env::var_os("NORTHSTAR_PROOF_CHALLENGER_KEYPAIR").unwrap()).unwrap();
    let session = northstar_portal::find_session_pda(&PORTAL).0;
    let receipt = northstar_portal::find_deposit_receipt_pda(&PORTAL, &session, &payer.pubkey()).0;
    let target = Keypair::new();
    let buffer = Keypair::new();
    let delegation = northstar_portal::find_delegation_record_pda(&PORTAL, &target.pubkey()).0;
    let smoke = env::var_os("NORTHSTAR_COORDINATOR_SMOKE").is_some();
    send(
        &rpc,
        &payer,
        &[&payer],
        &[
            solana_system_interface::instruction::transfer(
                &payer.pubkey(),
                &challenger.pubkey(),
                100_000_000,
            ),
            instruction(
                vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new(session, false),
                    AccountMeta::new(northstar_portal::find_fee_vault_pda(&PORTAL).0, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                ],
                PortalInstruction::OpenSession(OpenSession {
                    grid_id: 1,
                    ttl_slots: 20_000,
                    fee_cap: 1_000_000_000,
                    validator: rpc.get_identity().unwrap(),
                    settlement_interval_slots: if smoke { 20 } else { 75 },
                }),
            ),
        ],
    );
    send(
        &rpc,
        &payer,
        &[&payer],
        &[instruction(
            vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new(receipt, false),
                AccountMeta::new_readonly(payer.pubkey(), false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            PortalInstruction::DepositFee {
                lamports: 4_000_000,
            },
        )],
    );
    send(
        &rpc,
        &payer,
        &[&payer, &target, &buffer],
        &[
            solana_system_interface::instruction::create_account(
                &payer.pubkey(),
                &target.pubkey(),
                10_000_000,
                8,
                &OWNER,
            ),
            solana_system_interface::instruction::create_account(
                &payer.pubkey(),
                &buffer.pubkey(),
                10_000_000,
                8,
                &OWNER,
            ),
        ],
    );
    let mut delegate = vec![0];
    delegate.extend(1u64.to_le_bytes());
    send(
        &rpc,
        &payer,
        &[&payer, &target],
        &[Instruction::new_with_bytes(
            OWNER,
            &delegate,
            vec![
                AccountMeta::new(target.pubkey(), true),
                AccountMeta::new(buffer.pubkey(), false),
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new(delegation, false),
                AccountMeta::new_readonly(OWNER, false),
                AccountMeta::new_readonly(PORTAL, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
        )],
    );
    assert_eq!(rpc.get_account(&target.pubkey()).unwrap().owner, PORTAL);
    poll("fresh delegation and deposit in ER", 120, || {
        (er.get_account(&target.pubkey()).ok()?.owner == OWNER
            && er.get_balance(&payer.pubkey()).ok()? == 4_000_000)
            .then_some(())
    });
    if let Some(gate) = env::var_os("NORTHSTAR_COORDINATOR_ER_GATE") {
        fs::write(
            directory().join("er-ready"),
            b"fresh delegation and funded ER ready",
        )
        .unwrap();
        poll("colocated proof workload", 900, || {
            PathBuf::from(&gate).exists().then_some(())
        });
    }
    let transaction = Transaction::new_signed_with_payer(
        &[Instruction::new_with_bytes(
            OWNER,
            &[1],
            vec![AccountMeta::new(target.pubkey(), false)],
        )],
        Some(&payer.pubkey()),
        &[&payer],
        er.get_latest_blockhash().unwrap(),
    );
    let signature = er.send_and_confirm_transaction(&transaction).unwrap();
    assert_eq!(
        er.get_account(&target.pubkey()).unwrap().data,
        vec![100, 0, 0, 0, 0, 0, 0, 0]
    );
    let cursor_key = northstar_portal::find_checkpoint_cursor_pda(&PORTAL, &session).0;
    let cursor = poll("coordinator-prepared real ER checkpoint", 480, || {
        let cursor =
            CheckpointCursor::try_from_slice(&rpc.get_account(&cursor_key).ok()?.data).ok()?;
        (cursor.active_checkpoint != Pubkey::default()).then_some(cursor)
    });
    let checkpoint =
        Checkpoint::try_from_slice(&rpc.get_account(&cursor.active_checkpoint).unwrap().data)
            .unwrap();
    assert_eq!(checkpoint.step_count, 1);
    let plan_dir = PathBuf::from(env::var_os("NORTHSTAR_CHECKPOINT_PLAN_DIR").unwrap());
    let artifact = fs::read_dir(plan_dir)
        .unwrap()
        .filter_map(Result::ok)
        .find_map(|entry| {
            if !entry
                .file_name()
                .to_string_lossy()
                .ends_with(".da-v1.borsh")
            {
                return None;
            }
            crate::checkpoint::CheckpointArtifactV1::decode_verified(&fs::read(entry.path()).ok()?)
                .ok()
        })
        .expect("manager's persisted runtime checkpoint");
    assert_eq!(artifact.checkpoint.er_slot, checkpoint.er_slot);
    assert_eq!(
        artifact.da.pages[0].transaction,
        bincode::serialize(&transaction).unwrap()
    );
    fs::write(
        directory().join("checkpoint-artifact.borsh"),
        artifact.canonical_bytes().unwrap(),
    )
    .unwrap();
    fs::write(
        directory().join("observation.bin"),
        bincode::serialize(&(
            cursor.active_checkpoint,
            checkpoint.er_slot,
            target.pubkey(),
            signature,
        ))
        .unwrap(),
    )
    .unwrap();
    if smoke {
        return;
    }
    let challenge = northstar_portal::find_challenge_pda(&PORTAL, &cursor.active_checkpoint).0;
    let challenge_started_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    fs::write(
        directory().join("challenge-start.json"),
        serde_json::to_vec(&serde_json::json!({"challenge_started_unix_ms":challenge_started_ms}))
            .unwrap(),
    )
    .unwrap();
    send(
        &rpc,
        &payer,
        &[&payer, &challenger],
        &[instruction(
            vec![
                AccountMeta::new(challenger.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new(cursor.active_checkpoint, false),
                AccountMeta::new(challenge, false),
                AccountMeta::new(
                    northstar_portal::find_da_proof_pda(&PORTAL, &challenge).0,
                    false,
                ),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            PortalInstruction::OpenChallenge(northstar_portal::OpenChallenge {
                er_slot: checkpoint.er_slot,
            }),
        )],
    );
    let page = &artifact.da.pages[0];
    let mut trace_path = [[0; 32]; northstar_portal::TRACE_AUTH_PATH_NODES];
    trace_path[..page.post_state_path.siblings.len()]
        .copy_from_slice(&page.post_state_path.siblings);
    send(
        &rpc,
        &payer,
        &[&payer],
        &[instruction(
            vec![
                AccountMeta::new_readonly(payer.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new_readonly(cursor.active_checkpoint, false),
                AccountMeta::new(challenge, false),
                AccountMeta::new(
                    northstar_portal::find_da_proof_pda(&PORTAL, &challenge).0,
                    false,
                ),
            ],
            PortalInstruction::RespondChallenge(northstar_portal::RespondChallenge {
                er_slot: checkpoint.er_slot,
                claimed_step: 0,
                claimed_state_root: page.post_state_root,
                trace_path_len: page.post_state_path.siblings.len() as u8,
                trace_path,
                da_payload_root: artifact.checkpoint.da_commitment,
                da_inclusion_proof_hash: solana_sha256_hasher::hashv(&[&artifact
                    .canonical_bytes()
                    .unwrap()])
                .to_bytes(),
            }),
        )],
    );
    poll("isolated runtime step is ready", 60, || {
        let state =
            northstar_portal::Challenge::try_from_slice(&rpc.get_account(&challenge).ok()?.data)
                .ok()?;
        (state.turn == northstar_portal::ChallengeTurn::Prove).then_some(())
    });
    let fence = rpc.get_slot().unwrap();
    poll("finalized challenge fence", 90, || {
        (rpc.get_slot_with_commitment(CommitmentConfig::finalized())
            .ok()?
            >= fence)
            .then_some(())
    });
    fs::write(directory().join("driver-fence"), fence.to_string()).unwrap();
    println!(
        "NORTHSTAR_TIMING {}",
        serde_json::json!({"phase":"real_er_driver_handoff", "signature":signature.to_string(), "fresh_delegation":true, "synthetic_checkpoint":false, "proof_upload_transactions":0, "er_slot":checkpoint.er_slot})
    );
}

#[test]
#[ignore = "read-only observer for live coordinator recovery"]
fn live_coordinator_observe_settlement() {
    let rpc = rpc();
    let (checkpoint_key, er_slot, target, signature): (
        Pubkey,
        u64,
        Pubkey,
        solana_signature::Signature,
    ) = bincode::deserialize(&fs::read(directory().join("observation.bin")).unwrap()).unwrap();
    let started = Instant::now();
    poll("coordinator proof resolution", 300, || {
        let state =
            Checkpoint::try_from_slice(&rpc.get_account(&checkpoint_key).ok()?.data).ok()?;
        (state.challenge_resolved && state.status != CheckpointStatus::Invalid).then_some(())
    });
    let resolved_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let timing: serde_json::Value =
        serde_json::from_slice(&fs::read(directory().join("challenge-start.json")).unwrap())
            .unwrap();
    let challenge_to_resolution_ms =
        resolved_ms - u128::from(timing["challenge_started_unix_ms"].as_u64().unwrap());
    assert!(
        challenge_to_resolution_ms <= 120_000,
        "challenge-to-confirmed-resolution exceeded two minutes: {challenge_to_resolution_ms}ms"
    );
    crate::proof_coordinator::copy_test_evidence(
        &PathBuf::from(env::var_os("NORTHSTAR_PROOF_JOB_DIR").unwrap()),
        &directory(),
        checkpoint_key,
    );
    let checkpoint = poll("automatic settlement and bond release", 480, || {
        let state =
            Checkpoint::try_from_slice(&rpc.get_account(&checkpoint_key).ok()?.data).ok()?;
        (state.status == CheckpointStatus::Settled
            && state.bond_status == CheckpointBondStatus::Released)
            .then_some(state)
    });
    let account = rpc.get_account(&target).unwrap();
    assert_eq!(account.owner, PORTAL);
    assert_eq!(account.data, vec![100, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(account.lamports, 10_000_000);
    let session_key = northstar_portal::find_session_pda(&PORTAL).0;
    let session = Session::try_from_slice(&rpc.get_account(&session_key).unwrap().data).unwrap();
    assert_eq!(session.last_settled_er_slot, er_slot);
    assert_eq!(session.settlement_status, SettlementStatus::Idle);
    let cursor = CheckpointCursor::try_from_slice(
        &rpc.get_account(&northstar_portal::find_checkpoint_cursor_pda(&PORTAL, &session_key).0)
            .unwrap()
            .data,
    )
    .unwrap();
    assert_eq!(cursor.latest_finalized_er_slot, er_slot);
    assert_eq!(cursor.active_checkpoint, Pubkey::default());
    let report = serde_json::json!({"schema":"northstar-coordinator-recovery-v1", "settled":true, "bond_released":checkpoint.bond_status == CheckpointBondStatus::Released, "observer_submits_transactions":false, "slot_warps":false, "fresh_delegation":true, "runtime_transaction_signature":signature.to_string(), "synthetic_checkpoint":false, "challenge_to_resolution_ms":challenge_to_resolution_ms, "resolve_observed_unix_ms":resolved_ms, "wall_ms":started.elapsed().as_millis()});
    fs::write(
        directory().join("recovery.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!("NORTHSTAR_TIMING {report}");
}
