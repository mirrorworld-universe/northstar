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
                step_count: 4,
                previous_state_root: [0; 32],
                new_state_root: [2; 32],
                trace_root: [4; 32],
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
            2,
            [9; 32],
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
            3,
            [10; 32],
        )],
    );

    let challenge_account = rpc.get_account(&challenge).unwrap();
    let challenge_state = Challenge::try_from_slice(&challenge_account.data).unwrap();
    assert_eq!(
        (challenge_state.start_step, challenge_state.end_step),
        (2, 4)
    );
    assert_eq!(challenge_state.midpoint_step, 3);
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
