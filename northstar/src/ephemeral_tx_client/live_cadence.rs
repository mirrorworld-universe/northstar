use {
    super::live_checkpoint::{instruction, send, PORTAL},
    borsh::BorshDeserialize,
    northstar_portal::{
        Checkpoint, CheckpointCursor, CheckpointStatus, DepositReceipt, OpenSession,
        PortalInstruction, Session,
    },
    solana_commitment_config::CommitmentConfig,
    solana_instruction::AccountMeta,
    solana_keypair::read_keypair_file,
    solana_rpc_client::rpc_client::RpcClient,
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_system_interface::instruction::transfer,
    std::{
        env,
        path::PathBuf,
        thread::sleep,
        time::{Duration, Instant},
    },
};

fn poll<T>(label: &str, seconds: u64, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {label}");
        sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "requires fresh validator with Northstar service; optional external process restart"]
fn live_service_seals_and_settles_one_transaction() {
    let payer = read_keypair_file(
        env::var_os("NORTHSTAR_LIVE_PAYER")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var_os("HOME").unwrap()).join(".config/solana/id.json")
            }),
    )
    .unwrap();
    let rpc = RpcClient::new_with_commitment(
        env::var("NORTHSTAR_LIVE_RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".into()),
        CommitmentConfig::confirmed(),
    );
    let er = RpcClient::new_with_commitment(
        env::var("NORTHSTAR_LIVE_ER_RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8910".into()),
        CommitmentConfig::confirmed(),
    );
    let identity = rpc.get_identity().unwrap();
    let session = northstar_portal::find_session_pda(&PORTAL).0;
    let fee_vault = northstar_portal::find_fee_vault_pda(&PORTAL).0;
    let receipt = northstar_portal::find_deposit_receipt_pda(&PORTAL, &session, &payer.pubkey()).0;
    let cursor = northstar_portal::find_checkpoint_cursor_pda(&PORTAL, &session).0;
    send(
        &rpc,
        &payer,
        &[&payer],
        &[
            transfer(&payer.pubkey(), &identity, 100_000_000),
            instruction(
                vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new(session, false),
                    AccountMeta::new(fee_vault, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                ],
                PortalInstruction::OpenSession(OpenSession {
                    grid_id: 1,
                    ttl_slots: 20_000,
                    fee_cap: 1_000_000_000,
                    validator: identity,
                    settlement_interval_slots: 75,
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
    poll("ER deposit injection", 120, || {
        (er.get_balance(&payer.pubkey()).ok()? == 4_000_000).then_some(())
    });
    let initial = Session::try_from_slice(&rpc.get_account(&session).unwrap().data).unwrap();
    let due = initial.last_settled_l1_slot + initial.settlement_interval_slots;
    poll("empty cadence interval", 90, || {
        (rpc.get_slot().ok()? > due + 2).then_some(())
    });
    assert!(
        rpc.get_account_with_commitment(&cursor, CommitmentConfig::confirmed())
            .unwrap()
            .value
            .is_none(),
        "empty interval must not propose a checkpoint"
    );
    let submitted_slot = rpc.get_slot().unwrap();
    let started = Instant::now();
    send(
        &er,
        &payer,
        &[&payer],
        &[crate::er_withdrawal_instruction(
            &PORTAL,
            &payer.pubkey(),
            &payer.pubkey(),
            1_000_000,
        )],
    );
    let active = poll("automatic partial checkpoint", 45, || {
        let account = rpc.get_account(&cursor).ok()?;
        let state = CheckpointCursor::try_from_slice(&account.data).ok()?;
        (state.active_er_slot > 0).then_some(state)
    });
    let checkpoint_key = active.active_checkpoint;
    let checkpoint =
        Checkpoint::try_from_slice(&rpc.get_account(&checkpoint_key).unwrap().data).unwrap();
    assert_eq!(checkpoint.step_count, 1);
    assert_eq!(checkpoint.proposer, identity);
    assert!(checkpoint.proposed_at_l1_slot >= due);
    assert!(checkpoint.proposed_at_l1_slot <= submitted_slot + 75);
    eprintln!(
        "NORTHSTAR_TIMING {}",
        serde_json::json!({"phase":"live_low_traffic_proposal", "wall_ms":started.elapsed().as_millis(), "submitted_l1_slot": submitted_slot, "proposed_l1_slot": checkpoint.proposed_at_l1_slot, "step_count":1})
    );

    if let Ok(ready) = env::var("NORTHSTAR_LIVE_RESTART_READY") {
        let resume =
            env::var("NORTHSTAR_LIVE_RESTART_RESUME").expect("restart resume marker required");
        let rooted = poll("checkpoint rooted before crash", 60, || {
            let account = rpc
                .get_account_with_commitment(&checkpoint_key, CommitmentConfig::finalized())
                .ok()?
                .value?;
            let rooted = Checkpoint::try_from_slice(&account.data).ok()?;
            (rooted.new_state_root == checkpoint.new_state_root).then_some(rooted)
        });
        assert_eq!(rooted.status, CheckpointStatus::Pending);
        std::fs::write(ready, checkpoint_key.to_string()).unwrap();
        poll("external validator restart", 180, || {
            PathBuf::from(&resume).exists().then_some(())
        });
        poll("RPC after restart", 90, || rpc.get_health().ok());
        let restored = poll("checkpoint restored after replay", 90, || {
            let account = rpc.get_account(&checkpoint_key).ok()?;
            Checkpoint::try_from_slice(&account.data).ok()
        });
        assert_eq!(restored.er_slot, checkpoint.er_slot);
        assert_eq!(restored.step_count, checkpoint.step_count);
        assert_eq!(restored.new_state_root, checkpoint.new_state_root);
        assert_eq!(restored.effect_commitment, checkpoint.effect_commitment);
    }
    poll("automatic settlement", 180, || {
        let account = rpc.get_account(&checkpoint_key).ok()?;
        let state = Checkpoint::try_from_slice(&account.data).ok()?;
        (state.status == CheckpointStatus::Settled).then_some(())
    });
    let settled = Session::try_from_slice(&rpc.get_account(&session).unwrap().data).unwrap();
    assert_eq!(settled.last_settled_er_slot, checkpoint.er_slot);
    let terminal =
        Checkpoint::try_from_slice(&rpc.get_account(&checkpoint_key).unwrap().data).unwrap();
    assert_eq!(
        terminal.bond_status,
        northstar_portal::CheckpointBondStatus::Released
    );
    assert_eq!(er.get_balance(&payer.pubkey()).unwrap(), 3_000_000);
    let receipt_before = rpc.get_account(&receipt).unwrap();
    let receipt_state = DepositReceipt::try_from_slice(&receipt_before.data).unwrap();
    assert_eq!(receipt_state.withdrawn, 1_000_000);
    let settled_slot = rpc.get_slot().unwrap();
    poll("post-settlement retry observation", 30, || {
        (rpc.get_slot().ok()? >= settled_slot + 10).then_some(())
    });
    assert_eq!(rpc.get_account(&receipt).unwrap(), receipt_before);
    eprintln!(
        "NORTHSTAR_TIMING {}",
        serde_json::json!({"phase":"live_low_traffic_settled", "wall_ms":started.elapsed().as_millis(), "restart_requested":env::var_os("NORTHSTAR_LIVE_RESTART_READY").is_some(), "slot_warps":false})
    );
}
