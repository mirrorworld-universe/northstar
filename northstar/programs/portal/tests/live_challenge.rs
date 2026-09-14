use {
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

const PORTAL_PROGRAM_ID: Pubkey =
    solana_pubkey::pubkey!("5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf");

fn synthetic_state_root(index: u64) -> [u8; 32] {
    let mut root = [0; 32];
    root[24..].copy_from_slice(&(index + 1).to_be_bytes());
    root
}

fn trace_leaf_hash(index: u32, state_root: &[u8; 32]) -> [u8; 32] {
    solana_sha256_hasher::hashv(&[
        b"northstar-checkpoint-v1",
        b"trace",
        b"leaf",
        &36u64.to_le_bytes(),
        &index.to_le_bytes(),
        state_root,
    ])
    .to_bytes()
}

fn trace_node_hash(level: u32, left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    solana_sha256_hasher::hashv(&[
        b"northstar-checkpoint-v1",
        b"trace",
        b"node",
        &level.to_le_bytes(),
        left,
        right,
    ])
    .to_bytes()
}

fn synthetic_trace_layers(step_count: u64) -> Vec<Vec<[u8; 32]>> {
    let count = (step_count + 1) as usize;
    let width = count.next_power_of_two();
    let mut leaves = (0..count)
        .map(|index| trace_leaf_hash(index as u32, &synthetic_state_root(index as u64)))
        .collect::<Vec<_>>();
    leaves.extend((count..width).map(|index| {
        solana_sha256_hasher::hashv(&[
            b"northstar-checkpoint-v1",
            b"trace",
            b"empty-leaf",
            &4u64.to_le_bytes(),
            &(index as u32).to_le_bytes(),
        ])
        .to_bytes()
    }));
    let mut layers = vec![leaves];
    let mut level = 0;
    while layers.last().unwrap().len() > 1 {
        let next = layers
            .last()
            .unwrap()
            .chunks_exact(2)
            .map(|pair| trace_node_hash(level, &pair[0], &pair[1]))
            .collect();
        layers.push(next);
        level += 1;
    }
    layers
}

fn synthetic_trace_root(step_count: u64) -> [u8; 32] {
    let layers = synthetic_trace_layers(step_count);
    let inner = layers.last().unwrap()[0];
    solana_sha256_hasher::hashv(&[
        b"northstar-checkpoint-v1",
        b"trace",
        b"root",
        &((step_count + 1) as u32).to_le_bytes(),
        &inner,
    ])
    .to_bytes()
}

fn synthetic_trace_path(step_count: u64, state_index: u64) -> (u8, [[u8; 32]; 5]) {
    let layers = synthetic_trace_layers(step_count);
    let mut path = [[0; 32]; 5];
    let mut index = state_index as usize;
    for (level, layer) in layers[..layers.len() - 1].iter().enumerate() {
        path[level] = layer[index ^ 1];
        index /= 2;
    }
    ((layers.len() - 1) as u8, path)
}

#[test]
#[ignore = "requires a Northstar solana-test-validator started with --portal"]
fn live_validator_challenge_bisection_and_da_reveal() {
    let rpc_url =
        env::var("NORTHSTAR_LIVE_RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".to_owned());
    let payer_path = env::var_os("NORTHSTAR_LIVE_PAYER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").expect("HOME")).join(".config/solana/id.json")
        });
    let payer = read_keypair_file(payer_path).expect("read live validator payer");
    let challenger = Keypair::new();
    let rpc = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());
    rpc.get_health().expect("validator healthy");

    let session = portal_pubkey(northstar_portal::find_session_pda(&PORTAL_PROGRAM_ID));
    let fee_vault = portal_pubkey(northstar_portal::find_fee_vault_pda(&PORTAL_PROGRAM_ID));
    let er_slot = rpc.get_slot().unwrap().saturating_add(1);
    let checkpoint = portal_pubkey(northstar_portal::find_checkpoint_pda(
        &PORTAL_PROGRAM_ID,
        &session,
        er_slot,
    ));
    let cursor = portal_pubkey(northstar_portal::find_checkpoint_cursor_pda(
        &PORTAL_PROGRAM_ID,
        &session,
    ));
    let challenge = portal_pubkey(northstar_portal::find_challenge_pda(
        &PORTAL_PROGRAM_ID,
        &checkpoint,
    ));
    let da_proof = portal_pubkey(northstar_portal::find_da_proof_pda(
        &PORTAL_PROGRAM_ID,
        &challenge,
    ));

    send(
        &rpc,
        &payer,
        &[&payer],
        &[
            portal_ix(
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
        &[portal_ix(
            vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new(checkpoint, false),
                AccountMeta::new(cursor, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            PortalInstruction::ProposeCheckpoint(ProposeCheckpoint {
                er_slot,
                step_count: 16,
                previous_state_root: synthetic_state_root(0),
                new_state_root: synthetic_state_root(16),
                trace_root: synthetic_trace_root(16),
                tx_effect_root: [5; 32],
                readonly_l1_root: [6; 32],
                da_commitment: [7; 32],
                effect_commitment: [3; 32],
                challenge_window_slots: 300,
            }),
        )],
    );

    send(
        &rpc,
        &payer,
        &[&payer, &challenger],
        &[portal_ix(
            vec![
                AccountMeta::new(challenger.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new(checkpoint, false),
                AccountMeta::new(challenge, false),
                AccountMeta::new(da_proof, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            PortalInstruction::OpenChallenge(OpenChallenge { er_slot }),
        )],
    );

    send(
        &rpc,
        &payer,
        &[&payer],
        &[respond_ix(
            payer.pubkey(),
            session,
            checkpoint,
            challenge,
            da_proof,
            er_slot,
            8,
            synthetic_state_root(8),
        )],
    );
    send(
        &rpc,
        &payer,
        &[&payer, &challenger],
        &[portal_ix(
            vec![
                AccountMeta::new_readonly(challenger.pubkey(), true),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new_readonly(checkpoint, false),
                AccountMeta::new(challenge, false),
            ],
            PortalInstruction::BisectChallenge(BisectChallenge {
                er_slot,
                dispute_upper: true,
            }),
        )],
    );
    send(
        &rpc,
        &payer,
        &[&payer],
        &[respond_ix(
            payer.pubkey(),
            session,
            checkpoint,
            challenge,
            da_proof,
            er_slot,
            12,
            synthetic_state_root(12),
        )],
    );

    let challenge_account = rpc.get_account(&challenge).unwrap();
    let challenge_state = Challenge::try_from_slice(&challenge_account.data).unwrap();
    assert_eq!(
        (challenge_state.start_step, challenge_state.end_step),
        (8, 16)
    );
    assert_eq!(challenge_state.midpoint_step, 12);
    assert_eq!(challenge_state.turn, ChallengeTurn::Challenger);

    let da_account = rpc.get_account(&da_proof).unwrap();
    let da_state = DataAvailabilityProof::try_from_slice(&da_account.data).unwrap();
    assert_eq!(da_state.status, DataAvailabilityStatus::Revealed);
    assert_eq!(da_state.payload_root, [7; 32]);
}

fn respond_ix(
    validator: Pubkey,
    session: Pubkey,
    checkpoint: Pubkey,
    challenge: Pubkey,
    da_proof: Pubkey,
    er_slot: u64,
    claimed_step: u64,
    claimed_state_root: [u8; 32],
) -> Instruction {
    portal_ix(
        vec![
            AccountMeta::new_readonly(validator, true),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new_readonly(checkpoint, false),
            AccountMeta::new(challenge, false),
            AccountMeta::new(da_proof, false),
        ],
        PortalInstruction::RespondChallenge(RespondChallenge {
            er_slot,
            claimed_step,
            claimed_state_root,
            trace_path_len: synthetic_trace_path(16, claimed_step).0,
            trace_path: synthetic_trace_path(16, claimed_step).1,
            da_payload_root: [7; 32],
            da_inclusion_proof_hash: [8; 32],
        }),
    )
}

fn portal_ix(accounts: Vec<AccountMeta>, ix: PortalInstruction) -> Instruction {
    Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts,
        data: borsh::to_vec(&ix).unwrap(),
    }
}

fn portal_pubkey((pubkey, _bump): (Pubkey, u8)) -> Pubkey {
    pubkey
}

fn send(rpc: &RpcClient, payer: &Keypair, signers: &[&Keypair], instructions: &[Instruction]) {
    let blockhash = rpc.get_latest_blockhash().unwrap();
    let transaction =
        Transaction::new_signed_with_payer(instructions, Some(&payer.pubkey()), signers, blockhash);
    rpc.send_and_confirm_transaction(&transaction).unwrap();
}
