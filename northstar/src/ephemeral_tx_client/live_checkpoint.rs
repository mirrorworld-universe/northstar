use {
    super::tests::supported_sbf_checkpoint_with_steps,
    borsh::BorshDeserialize,
    northstar_portal::{
        BisectChallenge, Challenge, ChallengeStatus, ChallengeTurn, CreateStepProof,
        DataAvailabilityProof, DataAvailabilityStatus, OpenChallenge, OpenSession,
        PortalInstruction, ProposeCheckpoint, ResolveChallenge, RespondChallenge, SealStepProof,
        StepProofVerifierMode, WriteStepProof, MAX_STEP_PROOF_BYTES, MAX_STEP_PROOF_CHUNK,
        TX_EFFECT_AUTH_PATH_NODES,
    },
    solana_commitment_config::CommitmentConfig,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::{read_keypair_file, Keypair},
    solana_pubkey::Pubkey,
    solana_rpc_client::rpc_client::RpcClient,
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_system_interface::instruction::transfer,
    solana_transaction::Transaction,
    std::{env, fs, path::PathBuf, process::Command},
};

pub(super) const PORTAL: Pubkey =
    solana_pubkey::pubkey!("5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf");

pub(super) fn instruction(accounts: Vec<AccountMeta>, data: PortalInstruction) -> Instruction {
    Instruction {
        program_id: PORTAL,
        accounts,
        data: borsh::to_vec(&data).unwrap(),
    }
}

pub(super) fn send(
    rpc: &RpcClient,
    payer: &Keypair,
    signers: &[&Keypair],
    instructions: &[Instruction],
) {
    let started = std::time::Instant::now();
    let transaction = Transaction::new_signed_with_payer(
        instructions,
        Some(&payer.pubkey()),
        signers,
        rpc.get_latest_blockhash().unwrap(),
    );
    rpc.send_and_confirm_transaction(&transaction).unwrap();
    for instruction in instructions
        .iter()
        .filter(|instruction| instruction.program_id == PORTAL)
    {
        let (phase, step) = match PortalInstruction::try_from_slice(&instruction.data).unwrap() {
            PortalInstruction::OpenSession(_) => ("open_session", None),
            PortalInstruction::ProposeCheckpoint(_) => ("propose_checkpoint", None),
            PortalInstruction::OpenChallenge(_) => ("open_challenge", None),
            PortalInstruction::RespondChallenge(value) => {
                ("respond_or_reveal", Some(value.claimed_step))
            }
            PortalInstruction::BisectChallenge(_) => ("select_half", None),
            _ => ("other", None),
        };
        println!(
            "NORTHSTAR_TIMING {}",
            serde_json::json!({
                "schema_version": 1, "clock": "wall", "phase": phase,
                "step": step, "transaction_ms": started.elapsed().as_secs_f64() * 1000.0,
                "signature": transaction.signatures[0].to_string(),
                "includes_rpc_confirmation": true,
            })
        );
    }
}

#[test]
#[ignore = "requires a fresh solana-test-validator with the Portal SBF program"]
fn real_checkpoint_bisects_to_captured_transaction() {
    let payer_path = env::var_os("NORTHSTAR_LIVE_PAYER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap()).join(".config/solana/id.json")
        });
    let payer = read_keypair_file(payer_path).unwrap();
    let challenger = Keypair::new();
    let rpc = RpcClient::new_with_commitment(
        env::var("NORTHSTAR_LIVE_RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".into()),
        CommitmentConfig::confirmed(),
    );
    rpc.get_health().unwrap();
    let session = northstar_portal::find_session_pda(&PORTAL).0;
    let fee_vault = northstar_portal::find_fee_vault_pda(&PORTAL).0;
    let step_count = env::var("NORTHSTAR_LIVE_STEP_COUNT")
        .map(|value| value.parse::<usize>().expect("integer step count"))
        .unwrap_or(16);
    assert!((1..=16).contains(&step_count));
    let selected_step = env::var("NORTHSTAR_LIVE_SELECTED_STEP")
        .map(|value| value.parse::<usize>().expect("integer selected step"))
        .unwrap_or(10.min(step_count - 1));
    assert!(selected_step < step_count);
    let (artifact, history) = supported_sbf_checkpoint_with_steps(0, session, step_count);
    artifact.verify().unwrap();
    let commitment = artifact.checkpoint;
    let er_slot = commitment.er_slot;
    let checkpoint = northstar_portal::find_checkpoint_pda(&PORTAL, &session, er_slot).0;
    let cursor = northstar_portal::find_checkpoint_cursor_pda(&PORTAL, &session).0;
    let challenge = northstar_portal::find_challenge_pda(&PORTAL, &checkpoint).0;
    let da = northstar_portal::find_da_proof_pda(&PORTAL, &challenge).0;

    send(
        &rpc,
        &payer,
        &[&payer],
        &[
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
                    validator: payer.pubkey(),
                    settlement_interval_slots: 10,
                }),
            ),
            transfer(&payer.pubkey(), &challenger.pubkey(), 50_000_000),
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
                AccountMeta::new(checkpoint, false),
                AccountMeta::new(cursor, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            PortalInstruction::ProposeCheckpoint(ProposeCheckpoint {
                er_slot,
                step_count: u64::from(commitment.step_count),
                previous_state_root: commitment.previous_state_root,
                new_state_root: commitment.new_state_root,
                trace_root: commitment.trace_root,
                tx_effect_root: commitment.transaction_effect_root,
                readonly_l1_root: commitment.readonly_l1_root,
                da_commitment: commitment.da_commitment,
                effect_commitment: commitment.effect_commitment,
                challenge_window_slots: 750,
            }),
        )],
    );
    let challenge_started = std::time::Instant::now();
    send(
        &rpc,
        &payer,
        &[&payer, &challenger],
        &[instruction(
            vec![
                AccountMeta::new(challenger.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new(checkpoint, false),
                AccountMeta::new(challenge, false),
                AccountMeta::new(da, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            PortalInstruction::OpenChallenge(OpenChallenge { er_slot }),
        )],
    );

    let response = |step: usize, reveal: bool| {
        let page = &artifact.da.pages[step];
        let (root, path) = if reveal {
            (page.post_state_root, &page.post_state_path)
        } else {
            (page.pre_state_root, &page.pre_state_path)
        };
        let mut trace_path = [[0; 32]; northstar_portal::TRACE_AUTH_PATH_NODES];
        trace_path[..path.siblings.len()].copy_from_slice(&path.siblings);
        instruction(
            vec![
                AccountMeta::new_readonly(payer.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new_readonly(checkpoint, false),
                AccountMeta::new(challenge, false),
                AccountMeta::new(da, false),
            ],
            PortalInstruction::RespondChallenge(RespondChallenge {
                er_slot,
                claimed_step: step as u64,
                claimed_state_root: root,
                trace_path_len: path.siblings.len() as u8,
                trace_path,
                da_payload_root: commitment.da_commitment,
                da_inclusion_proof_hash: solana_sha256_hasher::hashv(&[&artifact
                    .canonical_bytes()
                    .unwrap()])
                .to_bytes(),
            }),
        )
    };

    if let Ok(outcome) = env::var("NORTHSTAR_LIVE_OUTCOME") {
        use northstar_portal::{Checkpoint, CheckpointStatus, TimeoutChallenge};
        assert!(matches!(
            outcome.as_str(),
            "respondent-timeout" | "challenger-timeout"
        ));
        if outcome == "challenger-timeout" {
            assert!(step_count > 1);
            send(&rpc, &payer, &[&payer], &[response(step_count / 2, false)]);
        }
        let state = Challenge::try_from_slice(&rpc.get_account(&challenge).unwrap().data).unwrap();
        let before =
            Checkpoint::try_from_slice(&rpc.get_account(&checkpoint).unwrap().data).unwrap();
        let balance_before = rpc.get_balance(&challenger.pubkey()).unwrap();
        let started = std::time::Instant::now();
        let deadline = state.turn_deadline_l1_slot.min(state.hard_deadline_l1_slot);
        while rpc.get_slot().unwrap() < deadline {
            assert!(
                started.elapsed().as_secs() < 900,
                "real-clock deadline did not advance"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        send(
            &rpc,
            &payer,
            &[&payer],
            &[instruction(
                vec![
                    AccountMeta::new_readonly(payer.pubkey(), true),
                    AccountMeta::new_readonly(session, false),
                    AccountMeta::new(checkpoint, false),
                    AccountMeta::new(challenge, false),
                    AccountMeta::new(da, false),
                    AccountMeta::new(challenger.pubkey(), false),
                    AccountMeta::new(cursor, false),
                ],
                PortalInstruction::TimeoutChallenge(TimeoutChallenge { er_slot }),
            )],
        );
        let after =
            Checkpoint::try_from_slice(&rpc.get_account(&checkpoint).unwrap().data).unwrap();
        if outcome == "respondent-timeout" {
            assert_eq!(after.status, CheckpointStatus::Invalid);
            assert_eq!(
                rpc.get_balance(&challenger.pubkey()).unwrap(),
                balance_before + before.bond_lamports
            );
            send(
                &rpc,
                &payer,
                &[&payer],
                &[instruction(
                    vec![
                        AccountMeta::new(payer.pubkey(), true),
                        AccountMeta::new_readonly(session, false),
                        AccountMeta::new(checkpoint, false),
                        AccountMeta::new(cursor, false),
                        AccountMeta::new_readonly(system_program::id(), false),
                    ],
                    PortalInstruction::ProposeCheckpoint(ProposeCheckpoint {
                        er_slot,
                        step_count: u64::from(commitment.step_count),
                        previous_state_root: commitment.previous_state_root,
                        new_state_root: commitment.new_state_root,
                        trace_root: commitment.trace_root,
                        tx_effect_root: commitment.transaction_effect_root,
                        readonly_l1_root: commitment.readonly_l1_root,
                        da_commitment: commitment.da_commitment,
                        effect_commitment: commitment.effect_commitment,
                        challenge_window_slots: 750,
                    }),
                )],
            );
            let replacement =
                Checkpoint::try_from_slice(&rpc.get_account(&checkpoint).unwrap().data).unwrap();
            assert_eq!(replacement.status, CheckpointStatus::Pending);
            assert_eq!(replacement.er_slot, er_slot);
            assert_eq!(replacement.previous_state_root, before.previous_state_root);
            assert_eq!(replacement.bond_lamports, before.bond_lamports);
        } else {
            assert_eq!(after.status, CheckpointStatus::Pending);
            assert_eq!(after.bond_status, before.bond_status);
            assert_eq!(
                rpc.get_balance(&challenger.pubkey()).unwrap(),
                balance_before
            );
        }
        eprintln!(
            "NORTHSTAR_TIMING {}",
            serde_json::json!({
                "schema_version": 1, "phase": outcome, "wall_ms": started.elapsed().as_millis(),
                "deadline_l1_slot": deadline, "observed_l1_slot": rpc.get_slot().unwrap(),
                "includes_rpc_confirmation": true, "slot_warps": false,
                "replacement_proposed": outcome == "respondent-timeout",
            })
        );
        return;
    }

    let original_challenge = rpc.get_account(&challenge).unwrap().data;
    for mutation in 0..3 {
        let mut invalid = response(step_count / 2, step_count == 1);
        let PortalInstruction::RespondChallenge(mut data) =
            PortalInstruction::try_from_slice(&invalid.data).unwrap()
        else {
            unreachable!();
        };
        match (step_count, mutation) {
            (1, 0) => data.claimed_state_root[0] ^= 1,
            (1, 1) => data.da_payload_root[0] ^= 1,
            (1, 2) => data.da_inclusion_proof_hash = [0; 32],
            (_, 0) => data.trace_path[0][0] ^= 1,
            (_, 1) if data.trace_path_len >= 2 => data.trace_path.swap(0, 1),
            (_, 1) => continue,
            (_, 2) => data.trace_path_len -= 1,
            _ => unreachable!(),
        }
        invalid.data = borsh::to_vec(&PortalInstruction::RespondChallenge(data)).unwrap();
        let transaction = Transaction::new_signed_with_payer(
            &[invalid],
            Some(&payer.pubkey()),
            &[&payer],
            rpc.get_latest_blockhash().unwrap(),
        );
        assert!(rpc
            .simulate_transaction(&transaction)
            .unwrap()
            .value
            .err
            .is_some());
        assert_eq!(
            rpc.get_account(&challenge).unwrap().data,
            original_challenge
        );
    }

    let (mut start, mut end) = (0, step_count);
    let mut rounds = 0;
    while end - start > 1 {
        let step = start + (end - start) / 2;
        let dispute_upper = selected_step >= step;
        send(&rpc, &payer, &[&payer], &[response(step, false)]);
        send(
            &rpc,
            &payer,
            &[&payer, &challenger],
            &[instruction(
                vec![
                    AccountMeta::new_readonly(challenger.pubkey(), true),
                    AccountMeta::new_readonly(session, false),
                    AccountMeta::new_readonly(checkpoint, false),
                    AccountMeta::new(challenge, false),
                ],
                PortalInstruction::BisectChallenge(BisectChallenge {
                    er_slot,
                    dispute_upper,
                }),
            )],
        );
        if dispute_upper {
            start = step;
        } else {
            end = step;
        }
        rounds += 1;
    }
    send(&rpc, &payer, &[&payer], &[response(selected_step, true)]);
    let state = Challenge::try_from_slice(&rpc.get_account(&challenge).unwrap().data).unwrap();
    assert_eq!(
        (state.start_step, state.end_step),
        (selected_step as u64, selected_step as u64 + 1)
    );
    assert_eq!(state.turn, ChallengeTurn::Prove);
    assert_eq!(state.rounds, rounds);
    assert!(rounds <= usize::BITS - (step_count - 1).leading_zeros());
    if step_count == 16 {
        assert_eq!(rounds, 4);
    }
    assert_eq!(
        state.start_state_root,
        artifact.da.pages[selected_step].pre_state_root
    );
    assert_eq!(
        state.end_state_root,
        artifact.da.pages[selected_step].post_state_root
    );
    let da_state =
        DataAvailabilityProof::try_from_slice(&rpc.get_account(&da).unwrap().data).unwrap();
    assert_eq!(da_state.status, DataAvailabilityStatus::Revealed);
    assert_eq!(da_state.payload_root, commitment.da_commitment);

    use {
        northstar_transaction_proof::{
            decode_witness, encode_witness, fixture::build_replay_witness_v1, public_inputs_bytes,
        },
        northstar_zk_types::{
            ErStepPublicInputsV1, FrBytes, ER_STEP_PROOF_KIND_FULL_TRANSACTION,
            ER_STEP_PROOF_VERSION_V1,
        },
    };
    let session_state =
        northstar_portal::Session::try_from_slice(&rpc.get_account(&session).unwrap().data)
            .unwrap();
    let mut reference = build_replay_witness_v1().unwrap();
    reference.session_context = northstar_transaction_proof::session_context_bytes_v1(
        PORTAL.to_bytes(),
        session.to_bytes(),
        session_state.grid_id,
        session_state.nonce,
        session_state.validator.to_bytes(),
    );
    let session_public = northstar_transaction_proof::replay(&reference).unwrap();
    let page = &artifact.da.pages[state.start_step as usize];
    let expected = public_inputs_bytes(ErStepPublicInputsV1 {
        domain: session_public.domain,
        session_context: session_public.session_context,
        slot_step: FrBytes::from_u64_pair(er_slot, state.start_step),
        pre_state_root: FrBytes::new(state.start_state_root).unwrap(),
        post_state_root: FrBytes::new(state.end_state_root).unwrap(),
        tx_effect_root: FrBytes::new(page.transaction_effect_commitment).unwrap(),
        readonly_l1_root: FrBytes::new(commitment.readonly_l1_root).unwrap(),
        settlement_effect_root: FrBytes::new(commitment.effect_commitment).unwrap(),
    });
    let extraction_started = std::time::Instant::now();
    let witness = crate::replay::extract_replay_witness_v1(
        &history,
        &artifact,
        state.start_step as usize,
        crate::replay::ReplayContextV1 {
            session_context: reference.session_context,
            agave_revision: reference.runtime.agave_revision,
            northstar_revision: reference.runtime.northstar_revision,
            vm_config_hash: reference.runtime.vm_config_hash,
            syscall_registry_hash: reference.runtime.syscall_registry_hash,
        },
        &expected,
    )
    .unwrap();
    println!(
        "NORTHSTAR_TIMING {}",
        serde_json::json!({
            "schema_version": 1, "clock": "wall", "phase": "witness_extraction",
            "step": state.start_step,
            "elapsed_ms": extraction_started.elapsed().as_secs_f64() * 1000.0,
            "proving_ms": null, "verification_ms": null, "recovery_ms": null,
            "resolved": false,
        })
    );
    assert_eq!(witness.transaction_bytes, page.transaction);
    assert_eq!(
        public_inputs_bytes(northstar_transaction_proof::replay(&witness).unwrap()),
        expected
    );

    let Some(prover) = env::var_os("NORTHSTAR_LIVE_PROVER") else {
        return;
    };
    let artifact_dir = PathBuf::from(
        env::var_os("NORTHSTAR_LIVE_PROOF_DIR").expect("NORTHSTAR_LIVE_PROOF_DIR path"),
    );
    assert!(artifact_dir.is_absolute());
    fs::create_dir(&artifact_dir).unwrap();
    let witness_path = artifact_dir.join("witness-v2.bin");
    let measurement_path = artifact_dir.join("measurements.json");
    let encoded = encode_witness(&witness).unwrap();
    fs::write(&witness_path, &encoded).unwrap();
    let exported = decode_witness(&fs::read(&witness_path).unwrap()).unwrap();
    assert_eq!(
        public_inputs_bytes(northstar_transaction_proof::replay(&exported).unwrap()),
        expected
    );

    let proving_started = std::time::Instant::now();
    let status = Command::new(prover)
        .current_dir(&artifact_dir)
        .env("SP1_PROVER", "cuda")
        .arg("groth16")
        .arg(&witness_path)
        .arg(&measurement_path)
        .arg("live-checkpoint")
        .status()
        .unwrap();
    assert!(status.success());
    let proving_ms = proving_started.elapsed().as_secs_f64() * 1000.0;
    let proof: [u8; MAX_STEP_PROOF_BYTES] =
        fs::read(artifact_dir.join("northstar-sp1-groth16-onchain.bin"))
            .unwrap()
            .try_into()
            .unwrap();
    let proved_public: [u8; 256] = fs::read(artifact_dir.join("northstar-sp1-public-inputs.bin"))
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(proved_public, expected);
    let measurements: serde_json::Value =
        serde_json::from_slice(&fs::read(&measurement_path).unwrap()).unwrap();
    let candidate: serde_json::Value =
        serde_json::from_str(include_str!("../../zkvm-replay/partial-candidate-v1.json")).unwrap();
    assert_eq!(
        measurements["program_vkey_hash"],
        candidate["program_vkey_hash"]
    );
    let proof_phase = measurements["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"] == "groth16")
        .unwrap();
    assert!(proof_phase["prove_and_wrap_ms"].as_u64().unwrap() <= 120_000);
    assert!(challenge_started.elapsed().as_secs() < 600);

    let proof_account = northstar_portal::find_step_proof_pda(&PORTAL, &checkpoint).0;
    let mut tx_effect_path = [[0; 32]; TX_EFFECT_AUTH_PATH_NODES];
    tx_effect_path[..page.transaction_effect_path.siblings.len()]
        .copy_from_slice(&page.transaction_effect_path.siblings);
    let upload_started = std::time::Instant::now();
    send(
        &rpc,
        &payer,
        &[&payer, &challenger],
        &[instruction(
            vec![
                AccountMeta::new(challenger.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new_readonly(checkpoint, false),
                AccountMeta::new_readonly(challenge, false),
                AccountMeta::new(proof_account, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            PortalInstruction::CreateStepProof(CreateStepProof {
                er_slot,
                proof_kind: ER_STEP_PROOF_KIND_FULL_TRANSACTION,
                proof_version: ER_STEP_PROOF_VERSION_V1,
                step_index: state.start_step,
                session_context: expected[32..64].try_into().unwrap(),
                tx_effect_root: page.transaction_effect_commitment,
                tx_effect_path_len: page.transaction_effect_path.siblings.len() as u8,
                tx_effect_path,
                readonly_l1_root: commitment.readonly_l1_root,
                settlement_effect_root: commitment.effect_commitment,
            }),
        )],
    );
    for (offset, bytes) in proof.chunks(MAX_STEP_PROOF_CHUNK).enumerate() {
        let mut chunk = [0; MAX_STEP_PROOF_CHUNK];
        chunk[..bytes.len()].copy_from_slice(bytes);
        send(
            &rpc,
            &payer,
            &[&payer, &challenger],
            &[instruction(
                vec![
                    AccountMeta::new_readonly(challenger.pubkey(), true),
                    AccountMeta::new_readonly(session, false),
                    AccountMeta::new_readonly(checkpoint, false),
                    AccountMeta::new_readonly(challenge, false),
                    AccountMeta::new(proof_account, false),
                ],
                PortalInstruction::WriteStepProof(WriteStepProof {
                    er_slot,
                    offset: (offset * MAX_STEP_PROOF_CHUNK) as u32,
                    chunk_len: bytes.len() as u16,
                    chunk,
                }),
            )],
        );
    }
    send(
        &rpc,
        &payer,
        &[&payer, &challenger],
        &[instruction(
            vec![
                AccountMeta::new_readonly(challenger.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new_readonly(checkpoint, false),
                AccountMeta::new_readonly(challenge, false),
                AccountMeta::new(proof_account, false),
            ],
            PortalInstruction::SealStepProof(SealStepProof {
                er_slot,
                proof_len: proof.len() as u32,
            }),
        )],
    );
    let upload_ms = upload_started.elapsed().as_secs_f64() * 1000.0;

    let resolve = instruction(
        vec![
            AccountMeta::new_readonly(challenger.pubkey(), true),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(checkpoint, false),
            AccountMeta::new(challenge, false),
            AccountMeta::new_readonly(da, false),
            AccountMeta::new_readonly(proof_account, false),
            AccountMeta::new(challenger.pubkey(), false),
            AccountMeta::new(cursor, false),
        ],
        PortalInstruction::ResolveChallenge(ResolveChallenge {
            er_slot,
            verifier_mode: StepProofVerifierMode::Production,
        }),
    );
    let resolve_transaction = Transaction::new_signed_with_payer(
        std::slice::from_ref(&resolve),
        Some(&payer.pubkey()),
        &[&payer, &challenger],
        rpc.get_latest_blockhash().unwrap(),
    );
    let simulation = rpc
        .simulate_transaction(&resolve_transaction)
        .unwrap()
        .value;
    assert_eq!(simulation.err, None);
    let resolver_cu = simulation.units_consumed.unwrap();
    assert!(resolver_cu <= 130_000);
    let checkpoint_before = rpc.get_account(&checkpoint).unwrap();
    let cursor_before = rpc.get_account(&cursor).unwrap();
    let recipient_before = rpc.get_account(&challenger.pubkey()).unwrap();
    let resolution_started = std::time::Instant::now();
    rpc.send_and_confirm_transaction(&resolve_transaction)
        .unwrap();
    let resolution_ms = resolution_started.elapsed().as_secs_f64() * 1000.0;

    let resolved =
        northstar_portal::Checkpoint::try_from_slice(&rpc.get_account(&checkpoint).unwrap().data)
            .unwrap();
    let resolved_challenge =
        Challenge::try_from_slice(&rpc.get_account(&challenge).unwrap().data).unwrap();
    assert_eq!(resolved.status, northstar_portal::CheckpointStatus::Pending);
    assert!(resolved.challenge_resolved);
    assert_eq!(resolved_challenge.status, ChallengeStatus::ValidatorWon);
    assert_eq!(rpc.get_account(&cursor).unwrap(), cursor_before);
    assert_eq!(
        rpc.get_account(&challenger.pubkey()).unwrap(),
        recipient_before
    );
    assert_eq!(
        rpc.get_account(&checkpoint).unwrap().lamports,
        checkpoint_before.lamports
    );
    let checkpoint_before =
        northstar_portal::Checkpoint::try_from_slice(&checkpoint_before.data).unwrap();
    assert_eq!(resolved.bond_lamports, checkpoint_before.bond_lamports);
    assert_eq!(resolved.bond_status, checkpoint_before.bond_status);
    let summary = serde_json::json!({
        "schema_version": 1, "clock": "wall", "phase": "proof_resolution",
        "step": state.start_step, "proving_ms": proving_ms, "upload_ms": upload_ms,
        "resolution_ms": resolution_ms, "resolver_cu": resolver_cu,
        "challenge_to_outcome_ms": challenge_started.elapsed().as_secs_f64() * 1000.0,
        "resolved": true, "slot_warps": false,
    });
    fs::write(
        artifact_dir.join("resolution.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("NORTHSTAR_TIMING {summary}");
}
