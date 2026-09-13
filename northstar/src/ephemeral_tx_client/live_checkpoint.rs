use {
    super::tests::supported_sbf_checkpoint_with_fee,
    borsh::BorshDeserialize,
    northstar_portal::{
        BisectChallenge, Challenge, ChallengeTurn, DataAvailabilityProof, DataAvailabilityStatus,
        OpenChallenge, OpenSession, PortalInstruction, ProposeCheckpoint, RespondChallenge,
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
    std::{env, path::PathBuf},
};

const PORTAL: Pubkey = solana_pubkey::pubkey!("5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf");

fn instruction(accounts: Vec<AccountMeta>, data: PortalInstruction) -> Instruction {
    Instruction {
        program_id: PORTAL,
        accounts,
        data: borsh::to_vec(&data).unwrap(),
    }
}

fn send(rpc: &RpcClient, payer: &Keypair, signers: &[&Keypair], instructions: &[Instruction]) {
    let transaction = Transaction::new_signed_with_payer(
        instructions,
        Some(&payer.pubkey()),
        signers,
        rpc.get_latest_blockhash().unwrap(),
    );
    rpc.send_and_confirm_transaction(&transaction).unwrap();
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
    let (artifact, history) = supported_sbf_checkpoint_with_fee(0, session);
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

    let original_challenge = rpc.get_account(&challenge).unwrap().data;
    for mutation in 0..3 {
        let mut invalid = response(8, false);
        let PortalInstruction::RespondChallenge(mut data) =
            PortalInstruction::try_from_slice(&invalid.data).unwrap()
        else {
            unreachable!();
        };
        match mutation {
            0 => data.trace_path[0][0] ^= 1,
            1 => data.trace_path.swap(0, 1),
            2 => data.trace_path_len -= 1,
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

    for (step, dispute_upper) in [(8, true), (12, false), (10, true), (11, false)] {
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
    }
    send(&rpc, &payer, &[&payer], &[response(10, true)]);
    let state = Challenge::try_from_slice(&rpc.get_account(&challenge).unwrap().data).unwrap();
    assert_eq!((state.start_step, state.end_step), (10, 11));
    assert_eq!(state.turn, ChallengeTurn::Prove);
    assert_eq!(state.start_state_root, artifact.da.pages[10].pre_state_root);
    assert_eq!(state.end_state_root, artifact.da.pages[10].post_state_root);
    let da_state =
        DataAvailabilityProof::try_from_slice(&rpc.get_account(&da).unwrap().data).unwrap();
    assert_eq!(da_state.status, DataAvailabilityStatus::Revealed);
    assert_eq!(da_state.payload_root, commitment.da_commitment);

    use {
        northstar_transaction_proof::{fixture::build_replay_witness_v1, public_inputs_bytes},
        northstar_zk_types::{ErStepPublicInputsV1, FrBytes},
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
    assert_eq!(witness.transaction_bytes, page.transaction);
    assert_eq!(
        public_inputs_bytes(northstar_transaction_proof::replay(&witness).unwrap()),
        expected
    );
}
