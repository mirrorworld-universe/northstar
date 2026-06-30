#![cfg(test)]

use {
    base64_no_std::{prelude::BASE64_STANDARD, Engine as _},
    borsh::BorshDeserialize,
    northstar_portal::{
        BeginSettlement, CancelCheckpoint, ChallengeCheckpoint, Checkpoint, CheckpointBondStatus,
        CheckpointCursor, CheckpointStatus, CommitCheckpoint, DepositReceipt, FeeVault,
        NorthstarTransferEvent, OpenSession, PortalInstruction, ProposeCheckpoint, Session,
        TransferEventKind, CHECKPOINT_PROPOSER_BOND_LAMPORTS, WITHDRAWAL_SINK,
    },
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_program_test::{BanksClient, ProgramTest, ProgramTestContext},
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_system_interface::{instruction::transfer, program as system_program},
    solana_transaction::Transaction,
};

fn find_session_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"session"], program_id)
}

fn find_fee_vault_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"fee_vault"], program_id)
}

fn find_checkpoint_pda(program_id: &Pubkey, session: &Pubkey, er_slot: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"checkpoint", session.as_ref(), &er_slot.to_le_bytes()],
        program_id,
    )
}

fn find_checkpoint_cursor_pda(program_id: &Pubkey, session: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"checkpoint_cursor", session.as_ref()], program_id)
}

fn find_deposit_receipt_pda(
    program_id: &Pubkey,
    session: &Pubkey,
    recipient: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"deposit_receipt", session.as_ref(), recipient.as_ref()],
        program_id,
    )
}

fn build_open_session_with_validator_ix(
    program_id: &Pubkey,
    owner: &Pubkey,
    validator: &Pubkey,
    session_pda: &Pubkey,
    fee_vault_pda: &Pubkey,
    grid_id: u64,
    ttl_slots: u64,
    fee_cap: u64,
) -> Instruction {
    let ix = PortalInstruction::OpenSession(OpenSession {
        grid_id,
        ttl_slots,
        fee_cap,
        validator: validator.to_bytes(),
        settlement_interval_slots: 10,
    });
    let data = borsh::to_vec(&ix).unwrap();

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*owner, true),
            AccountMeta::new(*session_pda, false),
            AccountMeta::new(*fee_vault_pda, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}

fn build_open_session_ix(
    program_id: &Pubkey,
    owner: &Pubkey,
    session_pda: &Pubkey,
    fee_vault_pda: &Pubkey,
    grid_id: u64,
    ttl_slots: u64,
    fee_cap: u64,
) -> Instruction {
    build_open_session_with_validator_ix(
        program_id,
        owner,
        owner,
        session_pda,
        fee_vault_pda,
        grid_id,
        ttl_slots,
        fee_cap,
    )
}

fn build_close_session_ix(
    program_id: &Pubkey,
    owner: &Pubkey,
    session_pda: &Pubkey,
    fee_vault_pda: &Pubkey,
) -> Instruction {
    let ix = PortalInstruction::CloseSession;
    let data = borsh::to_vec(&ix).unwrap();

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*owner, true),
            AccountMeta::new(*session_pda, false),
            AccountMeta::new(*fee_vault_pda, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}

fn build_propose_checkpoint_ix(
    program_id: &Pubkey,
    proposer: &Pubkey,
    session_pda: &Pubkey,
    er_slot: u64,
    challenge_window_slots: u64,
) -> Instruction {
    let (checkpoint_pda, _) = find_checkpoint_pda(program_id, session_pda, er_slot);
    let (checkpoint_cursor_pda, _) = find_checkpoint_cursor_pda(program_id, session_pda);
    let ix = PortalInstruction::ProposeCheckpoint(ProposeCheckpoint {
        er_slot,
        previous_state_root: [1; 32],
        new_state_root: [2; 32],
        effect_commitment: [3; 32],
        challenge_window_slots,
    });
    let data = borsh::to_vec(&ix).unwrap();

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*proposer, true),
            AccountMeta::new_readonly(*session_pda, false),
            AccountMeta::new(checkpoint_pda, false),
            AccountMeta::new(checkpoint_cursor_pda, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}

fn build_commit_checkpoint_ix(
    program_id: &Pubkey,
    committer: &Pubkey,
    proposer: &Pubkey,
    session_pda: &Pubkey,
    er_slot: u64,
) -> Instruction {
    let (checkpoint_pda, _) = find_checkpoint_pda(program_id, session_pda, er_slot);
    let (checkpoint_cursor_pda, _) = find_checkpoint_cursor_pda(program_id, session_pda);
    let ix = PortalInstruction::CommitCheckpoint(CommitCheckpoint { er_slot });
    let data = borsh::to_vec(&ix).unwrap();

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*committer, true),
            AccountMeta::new_readonly(*session_pda, false),
            AccountMeta::new(checkpoint_pda, false),
            AccountMeta::new(checkpoint_cursor_pda, false),
            AccountMeta::new(*proposer, false),
        ],
        data,
    }
}

fn build_cancel_checkpoint_ix(
    program_id: &Pubkey,
    proposer: &Pubkey,
    session_pda: &Pubkey,
    er_slot: u64,
) -> Instruction {
    let (checkpoint_pda, _) = find_checkpoint_pda(program_id, session_pda, er_slot);
    let ix = PortalInstruction::CancelCheckpoint(CancelCheckpoint { er_slot });
    let data = borsh::to_vec(&ix).unwrap();

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*proposer, true),
            AccountMeta::new_readonly(*session_pda, false),
            AccountMeta::new(checkpoint_pda, false),
        ],
        data,
    }
}

fn build_challenge_checkpoint_ix(
    program_id: &Pubkey,
    challenger: &Pubkey,
    session_pda: &Pubkey,
    er_slot: u64,
) -> Instruction {
    let (checkpoint_pda, _) = find_checkpoint_pda(program_id, session_pda, er_slot);
    let ix = PortalInstruction::ChallengeCheckpoint(ChallengeCheckpoint { er_slot });
    let data = borsh::to_vec(&ix).unwrap();

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*challenger, true),
            AccountMeta::new_readonly(*session_pda, false),
            AccountMeta::new(checkpoint_pda, false),
        ],
        data,
    }
}

fn build_begin_settlement_ix(
    program_id: &Pubkey,
    validator: &Pubkey,
    session_pda: &Pubkey,
    er_slot: u64,
    checksum: [u8; 32],
) -> Instruction {
    let (checkpoint_pda, _) = find_checkpoint_pda(program_id, session_pda, er_slot);
    let ix = PortalInstruction::BeginSettlement(BeginSettlement { er_slot, checksum });
    let data = borsh::to_vec(&ix).unwrap();

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*validator, true),
            AccountMeta::new(*session_pda, false),
            AccountMeta::new_readonly(checkpoint_pda, false),
        ],
        data,
    }
}

fn build_deposit_fee_ix(
    program_id: &Pubkey,
    depositor: &Pubkey,
    session_pda: &Pubkey,
    recipient: &Pubkey,
    lamports: u64,
) -> Instruction {
    let (deposit_receipt_pda, _) = find_deposit_receipt_pda(program_id, session_pda, recipient);

    let ix = PortalInstruction::DepositFee { lamports };
    let data = borsh::to_vec(&ix).unwrap();

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*depositor, true),
            AccountMeta::new_readonly(*session_pda, false),
            AccountMeta::new(deposit_receipt_pda, false),
            AccountMeta::new_readonly(*recipient, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}

const PORTAL_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("GikCSCpYUq7QR7esoK6GM4UbJzKgdKNvS5bR1rBYH5E4");

async fn setup() -> ProgramTestContext {
    let mut program_test = ProgramTest::default();
    program_test.prefer_bpf(true);
    program_test.add_program("northstar_portal", PORTAL_PROGRAM_ID, None);
    program_test.start_with_context().await
}

async fn setup_with_account(pubkey: Pubkey, account: Account) -> ProgramTestContext {
    let mut program_test = ProgramTest::default();
    program_test.prefer_bpf(true);
    program_test.add_program("northstar_portal", PORTAL_PROGRAM_ID, None);
    program_test.add_account(pubkey, account);
    program_test.start_with_context().await
}

async fn get_account_data(banks: &mut BanksClient, pubkey: &Pubkey) -> Option<Vec<u8>> {
    banks.get_account(*pubkey).await.unwrap().map(|a| a.data)
}

async fn get_lamports(banks: &mut BanksClient, pubkey: &Pubkey) -> u64 {
    banks.get_account(*pubkey).await.unwrap().unwrap().lamports
}

#[tokio::test]
async fn checkpoint_proposal_commit_deadline_flow() {
    let delegated_account = Pubkey::new_unique();
    let delegated_data = vec![9, 8, 7, 6];
    let mut context = setup_with_account(
        delegated_account,
        Account {
            lamports: 1_000_000_000,
            data: delegated_data.clone(),
            owner: PORTAL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .await;

    let payer = context.payer.insecure_clone();
    let payer_pubkey = payer.pubkey();
    let committer = Keypair::new();
    let er_slot = 10;
    let challenge_window_slots = 5;
    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);
    let (checkpoint_pda, _) = find_checkpoint_pda(&PORTAL_PROGRAM_ID, &session_pda, er_slot);
    let (checkpoint_cursor_pda, _) = find_checkpoint_cursor_pda(&PORTAL_PROGRAM_ID, &session_pda);

    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );
    let fund_committer_ix = transfer(&payer_pubkey, &committer.pubkey(), 1_000_000_000);
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[open_ix, fund_committer_ix],
        Some(&payer_pubkey),
        &[&payer],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let propose_ix = build_propose_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &session_pda,
        er_slot,
        challenge_window_slots,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[propose_ix],
        Some(&payer_pubkey),
        &[&payer],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let checkpoint_data = get_account_data(&mut context.banks_client, &checkpoint_pda)
        .await
        .unwrap();
    let checkpoint = Checkpoint::try_from_slice(&checkpoint_data).unwrap();
    assert_eq!(checkpoint.status, CheckpointStatus::Pending);
    assert_eq!(checkpoint.er_slot, er_slot);
    assert_eq!(checkpoint.proposer.as_ref(), payer_pubkey.as_ref());
    assert_eq!(
        checkpoint.challenge_deadline_l1_slot,
        checkpoint.proposed_at_l1_slot + challenge_window_slots
    );
    assert_eq!(
        get_account_data(&mut context.banks_client, &delegated_account)
            .await
            .unwrap(),
        delegated_data
    );

    let early_commit_ix = build_commit_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &committer.pubkey(),
        &payer_pubkey,
        &session_pda,
        er_slot,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[early_commit_ix],
        Some(&payer_pubkey),
        &[&payer, &committer],
        blockhash,
    );
    let result = context.banks_client.process_transaction(tx).await;
    assert!(result.is_err(), "commit before deadline must fail");

    let current_slot = context.banks_client.get_root_slot().await.unwrap();
    context
        .warp_to_slot(current_slot + challenge_window_slots + 1)
        .unwrap();

    let commit_ix = build_commit_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &committer.pubkey(),
        &payer_pubkey,
        &session_pda,
        er_slot,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[commit_ix],
        Some(&payer_pubkey),
        &[&payer, &committer],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let checkpoint_data = get_account_data(&mut context.banks_client, &checkpoint_pda)
        .await
        .unwrap();
    let checkpoint = Checkpoint::try_from_slice(&checkpoint_data).unwrap();
    assert_eq!(checkpoint.status, CheckpointStatus::Committed);

    let cursor_data = get_account_data(&mut context.banks_client, &checkpoint_cursor_pda)
        .await
        .unwrap();
    let cursor = CheckpointCursor::try_from_slice(&cursor_data).unwrap();
    assert_eq!(cursor.latest_finalized_er_slot, er_slot);
    assert_eq!(
        cursor.latest_finalized_checkpoint.as_ref(),
        checkpoint_pda.as_ref()
    );
    assert_eq!(
        get_account_data(&mut context.banks_client, &delegated_account)
            .await
            .unwrap(),
        delegated_data
    );
}

#[tokio::test]
async fn checkpoint_bond_locks_and_releases() {
    let mut context = setup().await;
    let payer = context.payer.insecure_clone();
    let payer_pubkey = payer.pubkey();
    let proposer = Keypair::new();
    let proposer_pubkey = proposer.pubkey();
    let committer = Keypair::new();
    let er_slot = 11;
    let challenge_window_slots = 5;
    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);
    let (checkpoint_pda, _) = find_checkpoint_pda(&PORTAL_PROGRAM_ID, &session_pda, er_slot);

    let rent = context.banks_client.get_rent().await.unwrap();
    let cursor_rent = rent.minimum_balance(CheckpointCursor::LEN);
    let checkpoint_rent = rent.minimum_balance(Checkpoint::LEN);
    let rent_only_funding = cursor_rent + checkpoint_rent;
    let proposer_keepalive_lamports = rent.minimum_balance(0);

    let open_ix = build_open_session_with_validator_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &proposer_pubkey,
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );
    let fund_proposer_ix = transfer(&payer_pubkey, &proposer_pubkey, rent_only_funding);
    let fund_committer_ix = transfer(&payer_pubkey, &committer.pubkey(), 1_000_000);
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[open_ix, fund_proposer_ix, fund_committer_ix],
        Some(&payer_pubkey),
        &[&payer],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let propose_ix = build_propose_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &proposer_pubkey,
        &session_pda,
        er_slot,
        challenge_window_slots,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[propose_ix.clone()],
        Some(&payer_pubkey),
        &[&payer, &proposer],
        blockhash,
    );
    assert!(
        context.banks_client.process_transaction(tx).await.is_err(),
        "proposal without bond must fail"
    );

    let top_up_ix = transfer(
        &payer_pubkey,
        &proposer_pubkey,
        CHECKPOINT_PROPOSER_BOND_LAMPORTS + proposer_keepalive_lamports,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[top_up_ix], Some(&payer_pubkey), &[&payer], blockhash);
    context.banks_client.process_transaction(tx).await.unwrap();

    let proposer_before_propose = get_lamports(&mut context.banks_client, &proposer_pubkey).await;
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[propose_ix],
        Some(&payer_pubkey),
        &[&payer, &proposer],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let proposer_after_propose = get_lamports(&mut context.banks_client, &proposer_pubkey).await;
    assert_eq!(
        proposer_before_propose - proposer_after_propose,
        rent_only_funding + CHECKPOINT_PROPOSER_BOND_LAMPORTS
    );
    let checkpoint_account = context
        .banks_client
        .get_account(checkpoint_pda)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        checkpoint_account.lamports,
        checkpoint_rent + CHECKPOINT_PROPOSER_BOND_LAMPORTS
    );
    let checkpoint = Checkpoint::try_from_slice(&checkpoint_account.data).unwrap();
    assert_eq!(checkpoint.status, CheckpointStatus::Pending);
    assert_eq!(checkpoint.bond_lamports, CHECKPOINT_PROPOSER_BOND_LAMPORTS);
    assert_eq!(checkpoint.bond_status, CheckpointBondStatus::Locked);

    let current_slot = context.banks_client.get_root_slot().await.unwrap();
    context
        .warp_to_slot(current_slot + challenge_window_slots + 1)
        .unwrap();

    let proposer_before_commit = get_lamports(&mut context.banks_client, &proposer_pubkey).await;
    let commit_ix = build_commit_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &committer.pubkey(),
        &proposer_pubkey,
        &session_pda,
        er_slot,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[commit_ix.clone()],
        Some(&payer_pubkey),
        &[&payer, &committer],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let proposer_after_commit = get_lamports(&mut context.banks_client, &proposer_pubkey).await;
    assert_eq!(
        proposer_after_commit - proposer_before_commit,
        CHECKPOINT_PROPOSER_BOND_LAMPORTS
    );
    let checkpoint_account = context
        .banks_client
        .get_account(checkpoint_pda)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint_account.lamports, checkpoint_rent);
    let checkpoint = Checkpoint::try_from_slice(&checkpoint_account.data).unwrap();
    assert_eq!(checkpoint.status, CheckpointStatus::Committed);
    assert_eq!(checkpoint.bond_status, CheckpointBondStatus::Released);

    let proposer_before_repeat = get_lamports(&mut context.banks_client, &proposer_pubkey).await;
    let checkpoint_before_repeat = get_lamports(&mut context.banks_client, &checkpoint_pda).await;
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[commit_ix],
        Some(&payer_pubkey),
        &[&payer, &committer],
        blockhash,
    );
    assert!(context.banks_client.process_transaction(tx).await.is_err());
    assert_eq!(
        get_lamports(&mut context.banks_client, &proposer_pubkey).await,
        proposer_before_repeat
    );
    assert_eq!(
        get_lamports(&mut context.banks_client, &checkpoint_pda).await,
        checkpoint_before_repeat
    );

    let mut cancel_context = setup().await;
    let cancel_payer = cancel_context.payer.insecure_clone();
    let cancel_payer_pubkey = cancel_payer.pubkey();
    let cancel_proposer = Keypair::new();
    let cancel_proposer_pubkey = cancel_proposer.pubkey();
    let cancel_er_slot = 12;
    let (cancel_session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (cancel_fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);
    let (cancel_checkpoint_pda, _) =
        find_checkpoint_pda(&PORTAL_PROGRAM_ID, &cancel_session_pda, cancel_er_slot);

    let open_ix = build_open_session_with_validator_ix(
        &PORTAL_PROGRAM_ID,
        &cancel_payer_pubkey,
        &cancel_proposer_pubkey,
        &cancel_session_pda,
        &cancel_fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );
    let fund_proposer_ix = transfer(
        &cancel_payer_pubkey,
        &cancel_proposer_pubkey,
        rent_only_funding + CHECKPOINT_PROPOSER_BOND_LAMPORTS + proposer_keepalive_lamports,
    );
    let blockhash = cancel_context
        .banks_client
        .get_latest_blockhash()
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[open_ix, fund_proposer_ix],
        Some(&cancel_payer_pubkey),
        &[&cancel_payer],
        blockhash,
    );
    cancel_context
        .banks_client
        .process_transaction(tx)
        .await
        .unwrap();

    let propose_ix = build_propose_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &cancel_proposer_pubkey,
        &cancel_session_pda,
        cancel_er_slot,
        challenge_window_slots,
    );
    let cancel_before_propose =
        get_lamports(&mut cancel_context.banks_client, &cancel_proposer_pubkey).await;
    let blockhash = cancel_context
        .banks_client
        .get_latest_blockhash()
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[propose_ix],
        Some(&cancel_payer_pubkey),
        &[&cancel_payer, &cancel_proposer],
        blockhash,
    );
    cancel_context
        .banks_client
        .process_transaction(tx)
        .await
        .unwrap();

    let cancel_ix = build_cancel_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &cancel_proposer_pubkey,
        &cancel_session_pda,
        cancel_er_slot,
    );
    let blockhash = cancel_context
        .banks_client
        .get_latest_blockhash()
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[cancel_ix],
        Some(&cancel_payer_pubkey),
        &[&cancel_payer, &cancel_proposer],
        blockhash,
    );
    cancel_context
        .banks_client
        .process_transaction(tx)
        .await
        .unwrap();

    let cancel_after =
        get_lamports(&mut cancel_context.banks_client, &cancel_proposer_pubkey).await;
    assert_eq!(cancel_before_propose - cancel_after, rent_only_funding);
    let checkpoint_data =
        get_account_data(&mut cancel_context.banks_client, &cancel_checkpoint_pda)
            .await
            .unwrap();
    let checkpoint = Checkpoint::try_from_slice(&checkpoint_data).unwrap();
    assert_eq!(checkpoint.status, CheckpointStatus::Cancelled);
    assert_eq!(checkpoint.bond_status, CheckpointBondStatus::Released);
}

#[tokio::test]
async fn checkpoint_challenge_blocks_commit_and_settlement() {
    let delegated_account = Pubkey::new_unique();
    let delegated_data = vec![4, 3, 2, 1];
    let mut context = setup_with_account(
        delegated_account,
        Account {
            lamports: 1_000_000_000,
            data: delegated_data.clone(),
            owner: PORTAL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .await;

    let payer = context.payer.insecure_clone();
    let payer_pubkey = payer.pubkey();
    let challenger = Keypair::new();
    let challenge_window_slots = 5;
    let er_slot = 20;
    let effect_commitment = [3; 32];
    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);
    let (checkpoint_pda, _) = find_checkpoint_pda(&PORTAL_PROGRAM_ID, &session_pda, er_slot);

    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );
    let fund_challenger_ix = transfer(&payer_pubkey, &challenger.pubkey(), 1_000_000);
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[open_ix, fund_challenger_ix],
        Some(&payer_pubkey),
        &[&payer],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let propose_ix = build_propose_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &session_pda,
        er_slot,
        challenge_window_slots,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[propose_ix],
        Some(&payer_pubkey),
        &[&payer],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let challenge_ix = build_challenge_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &challenger.pubkey(),
        &session_pda,
        er_slot,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[challenge_ix],
        Some(&payer_pubkey),
        &[&payer, &challenger],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let checkpoint_data = get_account_data(&mut context.banks_client, &checkpoint_pda)
        .await
        .unwrap();
    let checkpoint = Checkpoint::try_from_slice(&checkpoint_data).unwrap();
    assert_eq!(checkpoint.status, CheckpointStatus::Challenged);
    assert_eq!(checkpoint.challenger.as_ref(), challenger.pubkey().as_ref());
    assert_eq!(checkpoint.bond_status, CheckpointBondStatus::Locked);

    let current_slot = context.banks_client.get_root_slot().await.unwrap();
    context
        .warp_to_slot(current_slot + challenge_window_slots + 10)
        .unwrap();

    let commit_ix = build_commit_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &challenger.pubkey(),
        &payer_pubkey,
        &session_pda,
        er_slot,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[commit_ix],
        Some(&payer_pubkey),
        &[&payer, &challenger],
        blockhash,
    );
    assert!(context.banks_client.process_transaction(tx).await.is_err());

    let begin_ix = build_begin_settlement_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &session_pda,
        er_slot,
        effect_commitment,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[begin_ix], Some(&payer_pubkey), &[&payer], blockhash);
    assert!(context.banks_client.process_transaction(tx).await.is_err());
    assert_eq!(
        get_account_data(&mut context.banks_client, &delegated_account)
            .await
            .unwrap(),
        delegated_data
    );

    let cancel_ix =
        build_cancel_checkpoint_ix(&PORTAL_PROGRAM_ID, &payer_pubkey, &session_pda, er_slot);
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[cancel_ix], Some(&payer_pubkey), &[&payer], blockhash);
    assert!(context.banks_client.process_transaction(tx).await.is_err());

    let mut unchallenged_context = setup().await;
    let unchallenged_payer = unchallenged_context.payer.insecure_clone();
    let unchallenged_payer_pubkey = unchallenged_payer.pubkey();
    let committer = Keypair::new();
    let unchallenged_er_slot = 21;
    let (unchallenged_session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (unchallenged_fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);
    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &unchallenged_payer_pubkey,
        &unchallenged_session_pda,
        &unchallenged_fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );
    let fund_committer_ix = transfer(&unchallenged_payer_pubkey, &committer.pubkey(), 1_000_000);
    let blockhash = unchallenged_context
        .banks_client
        .get_latest_blockhash()
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[open_ix, fund_committer_ix],
        Some(&unchallenged_payer_pubkey),
        &[&unchallenged_payer],
        blockhash,
    );
    unchallenged_context
        .banks_client
        .process_transaction(tx)
        .await
        .unwrap();

    let propose_ix = build_propose_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &unchallenged_payer_pubkey,
        &unchallenged_session_pda,
        unchallenged_er_slot,
        challenge_window_slots,
    );
    let blockhash = unchallenged_context
        .banks_client
        .get_latest_blockhash()
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[propose_ix],
        Some(&unchallenged_payer_pubkey),
        &[&unchallenged_payer],
        blockhash,
    );
    unchallenged_context
        .banks_client
        .process_transaction(tx)
        .await
        .unwrap();

    let current_slot = unchallenged_context
        .banks_client
        .get_root_slot()
        .await
        .unwrap();
    unchallenged_context
        .warp_to_slot(current_slot + challenge_window_slots + 1)
        .unwrap();

    let commit_ix = build_commit_checkpoint_ix(
        &PORTAL_PROGRAM_ID,
        &committer.pubkey(),
        &unchallenged_payer_pubkey,
        &unchallenged_session_pda,
        unchallenged_er_slot,
    );
    let blockhash = unchallenged_context
        .banks_client
        .get_latest_blockhash()
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[commit_ix],
        Some(&unchallenged_payer_pubkey),
        &[&unchallenged_payer, &committer],
        blockhash,
    );
    unchallenged_context
        .banks_client
        .process_transaction(tx)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_full_lifecycle() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);

    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[open_ix], Some(&payer.pubkey()), &[payer], blockhash);
    banks.process_transaction(tx).await.unwrap();

    let session_data = get_account_data(banks, &session_pda).await.unwrap();
    let session = Session::try_from_slice(&session_data).unwrap();
    assert_eq!(session.discriminator, Session::DISCRIMINATOR);
    assert_eq!(session.grid_id, 1);
    assert_eq!(session.ttl_slots, 100);
    assert_eq!(session.fee_cap, 5_000_000_000);

    let vault_data = get_account_data(banks, &fee_vault_pda).await.unwrap();
    let vault = FeeVault::try_from_slice(&vault_data).unwrap();
    assert_eq!(vault.discriminator, FeeVault::DISCRIMINATOR);

    let deposit_ix = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &payer.pubkey(),
        2_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // Verify DepositReceipt was created with correct balance
    let (deposit_receipt_pda, _) =
        find_deposit_receipt_pda(&PORTAL_PROGRAM_ID, &session_pda, &payer.pubkey());
    let receipt_data = get_account_data(banks, &deposit_receipt_pda).await.unwrap();
    let receipt = DepositReceipt::try_from_slice(&receipt_data).unwrap();
    assert_eq!(receipt.discriminator, DepositReceipt::DISCRIMINATOR);
    assert_eq!(receipt.balance, 0);

    let (
        current_slot,
        new_blockhash,
        payer_keypair,
        payer_pubkey,
        session_pda_addr,
        fee_vault_pda_addr,
    ) = {
        let banks = &mut context.banks_client;
        let payer = &context.payer;
        let current_slot = banks.get_root_slot().await.unwrap();
        let new_blockhash = banks.get_latest_blockhash().await.unwrap();
        let payer_keypair = payer.insecure_clone();
        let payer_pubkey = payer_keypair.pubkey();
        let session_pda_addr = session_pda;
        let fee_vault_pda_addr = fee_vault_pda;
        (
            current_slot,
            new_blockhash,
            payer_keypair,
            payer_pubkey,
            session_pda_addr,
            fee_vault_pda_addr,
        )
    };

    context.warp_to_slot(current_slot + 110).unwrap();
    context.last_blockhash = new_blockhash;

    let banks = &mut context.banks_client;

    let close_ix = build_close_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &session_pda_addr,
        &fee_vault_pda_addr,
    );

    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&payer_pubkey),
        &[&payer_keypair],
        context.last_blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    let session_data = get_account_data(banks, &session_pda_addr).await;
    assert!(session_data.is_none() || session_data.unwrap().is_empty());

    let vault_data = get_account_data(banks, &fee_vault_pda_addr).await;
    assert!(vault_data.is_none() || vault_data.unwrap().is_empty());
}

#[tokio::test]
async fn test_can_close_active_session() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);

    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &fee_vault_pda,
        1,
        1000,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[open_ix], Some(&payer.pubkey()), &[payer], blockhash);
    banks.process_transaction(tx).await.unwrap();

    let close_ix = build_close_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &fee_vault_pda,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[close_ix], Some(&payer.pubkey()), &[payer], blockhash);
    banks.process_transaction(tx).await.unwrap();

    let session_data = get_account_data(banks, &session_pda).await;
    assert!(session_data.is_none() || session_data.unwrap().is_empty());
}

#[tokio::test]
async fn test_cannot_deposit_to_wrong_vault() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let user_b = Keypair::new();

    let transfer_ix = transfer(&payer.pubkey(), &user_b.pubkey(), 10_000_000_000);

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_ix],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);

    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[open_ix], Some(&payer.pubkey()), &[payer], blockhash);
    banks.process_transaction(tx).await.unwrap();

    // Now user_b CAN deposit to payer's session (anyone can deposit to any valid session)
    let deposit_ix = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &user_b.pubkey(),
        &session_pda,
        &user_b.pubkey(),
        1_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&user_b.pubkey()),
        &[&user_b],
        blockhash,
    );
    let result = banks.process_transaction(tx).await;
    assert!(
        result.is_ok(),
        "Third party deposit should succeed: {:?}",
        result
    );

    // Verify the DepositReceipt was created with correct balance
    let (deposit_receipt_pda, _) =
        find_deposit_receipt_pda(&PORTAL_PROGRAM_ID, &session_pda, &user_b.pubkey());
    let receipt_data = get_account_data(banks, &deposit_receipt_pda).await.unwrap();
    let receipt = DepositReceipt::try_from_slice(&receipt_data).unwrap();
    assert_eq!(receipt.balance, 0);
}

#[tokio::test]
async fn test_multiple_deposits_accumulate() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);

    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[open_ix], Some(&payer.pubkey()), &[payer], blockhash);
    banks.process_transaction(tx).await.unwrap();

    let deposit_ix_1 = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &payer.pubkey(),
        1_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_1],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    let deposit_ix_2 = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &payer.pubkey(),
        2_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_2],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // Verify DepositReceipt has cumulative balance
    let (deposit_receipt_pda, _) =
        find_deposit_receipt_pda(&PORTAL_PROGRAM_ID, &session_pda, &payer.pubkey());
    let receipt_data = get_account_data(banks, &deposit_receipt_pda).await.unwrap();
    let receipt = DepositReceipt::try_from_slice(&receipt_data).unwrap();
    assert_eq!(receipt.balance, 0);
}

#[tokio::test]
async fn test_global_session_prevents_second_open() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let user_b = Keypair::new();

    let transfer_ix = transfer(&payer.pubkey(), &user_b.pubkey(), 10_000_000_000);

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_ix],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);

    let open_ix_1 = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[open_ix_1],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    let open_ix_2 = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &user_b.pubkey(),
        &session_pda,
        &fee_vault_pda,
        2,
        100,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[open_ix_2],
        Some(&user_b.pubkey()),
        &[&user_b],
        blockhash,
    );
    let result = banks.process_transaction(tx).await;
    assert!(result.is_err(), "only one global session can exist");
}

/// Test: Multiple users can deposit to the same FeeVault
#[tokio::test]
async fn test_anyone_can_deposit_to_vault() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let user_b = Keypair::new();
    let user_c = Keypair::new();

    // Fund users
    let transfer_ix_1 = transfer(&payer.pubkey(), &user_b.pubkey(), 10_000_000_000);
    let transfer_ix_2 = transfer(&payer.pubkey(), &user_c.pubkey(), 10_000_000_000);

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_ix_1, transfer_ix_2],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // User A (payer) opens a session
    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);

    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[open_ix], Some(&payer.pubkey()), &[payer], blockhash);
    banks.process_transaction(tx).await.unwrap();

    // User B deposits 1 SOL (to their own receipt)
    let deposit_ix_b = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &user_b.pubkey(),
        &session_pda,
        &user_b.pubkey(),
        1_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_b],
        Some(&user_b.pubkey()),
        &[&user_b],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // Verify user_b's DepositReceipt has 1 SOL
    let (receipt_b_pda, _) =
        find_deposit_receipt_pda(&PORTAL_PROGRAM_ID, &session_pda, &user_b.pubkey());
    let receipt_b_data = get_account_data(banks, &receipt_b_pda).await.unwrap();
    let receipt_b = DepositReceipt::try_from_slice(&receipt_b_data).unwrap();
    assert_eq!(receipt_b.balance, 0);

    // User C deposits 2 SOL (to their own receipt)
    let deposit_ix_c = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &user_c.pubkey(),
        &session_pda,
        &user_c.pubkey(),
        2_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_c],
        Some(&user_c.pubkey()),
        &[&user_c],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // Verify user_c's DepositReceipt has 2 SOL
    let (receipt_c_pda, _) =
        find_deposit_receipt_pda(&PORTAL_PROGRAM_ID, &session_pda, &user_c.pubkey());
    let receipt_c_data = get_account_data(banks, &receipt_c_pda).await.unwrap();
    let receipt_c = DepositReceipt::try_from_slice(&receipt_c_data).unwrap();
    assert_eq!(receipt_c.balance, 0);
}

/// Test: Depositing with invalid session fails
#[tokio::test]
async fn test_deposit_to_invalid_session_fails() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    // Create a random account that is NOT owned by the portal program
    let invalid_session = Keypair::new();
    let invalid_session_pubkey = invalid_session.pubkey();

    // Create and fund the invalid account (owned by system program)
    let fund_ix = transfer(&payer.pubkey(), &invalid_session_pubkey, 1_000_000_000);

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[fund_ix], Some(&payer.pubkey()), &[payer], blockhash);
    banks.process_transaction(tx).await.unwrap();

    // Try to deposit using the invalid account as session
    // This should fail because the "session" is not owned by the portal program
    let deposit_ix = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &invalid_session_pubkey,
        &payer.pubkey(),
        500_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    let result = banks.process_transaction(tx).await;
    assert!(result.is_err(), "Deposit to invalid session should fail");
}

/// Test: Third party deposits SOL for a different recipient
#[tokio::test]
async fn test_third_party_deposit_for_recipient() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let user_b = Keypair::new();
    let user_c = Keypair::new();

    // Fund user_b and user_c
    let transfer_ix_1 = transfer(&payer.pubkey(), &user_b.pubkey(), 10_000_000_000);
    let transfer_ix_2 = transfer(&payer.pubkey(), &user_c.pubkey(), 10_000_000_000);

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_ix_1, transfer_ix_2],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // User A (payer) opens a session
    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);

    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[open_ix], Some(&payer.pubkey()), &[payer], blockhash);
    banks.process_transaction(tx).await.unwrap();

    // User B deposits 1.5 SOL for User C (recipient = user_c, depositor = user_b)
    let deposit_ix = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &user_b.pubkey(),
        &session_pda,
        &user_c.pubkey(),
        1_500_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&user_b.pubkey()),
        &[&user_b],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // Verify DepositReceipt for (session_pda, user_c) was created with correct balance
    let (deposit_receipt_pda, _) =
        find_deposit_receipt_pda(&PORTAL_PROGRAM_ID, &session_pda, &user_c.pubkey());
    let receipt_data = get_account_data(banks, &deposit_receipt_pda).await.unwrap();
    let receipt = DepositReceipt::try_from_slice(&receipt_data).unwrap();
    assert_eq!(receipt.discriminator, DepositReceipt::DISCRIMINATOR);
    assert_eq!(receipt.session.as_ref(), session_pda.as_ref());
    assert_eq!(receipt.recipient.as_ref(), user_c.pubkey().as_ref());
    assert_eq!(receipt.balance, 0);
}

/// Test: Same depositor deposits twice - single DepositReceipt with cumulative balance
#[tokio::test]
async fn test_cumulative_deposit_receipt() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);

    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &fee_vault_pda,
        1,
        100,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[open_ix], Some(&payer.pubkey()), &[payer], blockhash);
    banks.process_transaction(tx).await.unwrap();

    // First deposit: 1 SOL
    let deposit_ix_1 = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &payer.pubkey(),
        1_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_1],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // Second deposit: 2 SOL more
    let deposit_ix_2 = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda,
        &payer.pubkey(),
        2_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_2],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // Verify single DepositReceipt with cumulative balance (3 SOL)
    let (deposit_receipt_pda, _) =
        find_deposit_receipt_pda(&PORTAL_PROGRAM_ID, &session_pda, &payer.pubkey());
    let receipt_data = get_account_data(banks, &deposit_receipt_pda).await.unwrap();
    let receipt = DepositReceipt::try_from_slice(&receipt_data).unwrap();
    assert_eq!(receipt.balance, 0);
}

/// Test: Depositing to an expired session fails with SessionExpired error
#[tokio::test]
async fn test_deposit_to_expired_session_fails() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer_pubkey = context.payer.pubkey();

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID);

    // Open session with short TTL (10 slots)
    let open_ix = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &session_pda,
        &fee_vault_pda,
        1,
        10,
        5_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[open_ix],
        Some(&payer_pubkey),
        &[&context.payer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    // Warp past the session TTL
    let current_slot = banks.get_root_slot().await.unwrap();
    context.warp_to_slot(current_slot + 15).unwrap();

    // Need to get fresh banks client after warp
    let banks = &mut context.banks_client;

    // Try to deposit after session expired
    let deposit_ix = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &session_pda,
        &payer_pubkey,
        1_000_000_000,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&payer_pubkey),
        &[&context.payer],
        blockhash,
    );
    let result = banks.process_transaction(tx).await;
    assert!(result.is_err(), "Deposit to expired session should fail");
}

#[tokio::test]
async fn test_start_withdrawal_transfers_to_fixed_sink_and_emits_event() {
    let context = setup().await;
    let source = Keypair::new();
    let l1_recipient = Pubkey::new_unique();
    let funding = transfer(&context.payer.pubkey(), &source.pubkey(), 10_000_000);
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let transaction = Transaction::new_signed_with_payer(
        &[funding],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        blockhash,
    );
    context
        .banks_client
        .process_transaction(transaction)
        .await
        .unwrap();
    let source_before = context
        .banks_client
        .get_account(source.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let sink_before = context
        .banks_client
        .get_account(Pubkey::new_from_array(WITHDRAWAL_SINK))
        .await
        .unwrap()
        .map(|account| account.lamports)
        .unwrap_or_default();
    let lamports = 1_000_000;
    let instruction = Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(source.pubkey(), true),
            AccountMeta::new_readonly(l1_recipient, false),
            AccountMeta::new(Pubkey::new_from_array(WITHDRAWAL_SINK), false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(solana_sdk_ids::sysvar::clock::id(), false),
        ],
        data: borsh::to_vec(&PortalInstruction::StartWithdrawal { lamports }).unwrap(),
    };
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&context.payer.pubkey()),
        &[&context.payer, &source],
        blockhash,
    );
    let result = context
        .banks_client
        .process_transaction_with_metadata(transaction)
        .await
        .unwrap();
    result.result.unwrap();

    let source_after = context
        .banks_client
        .get_account(source.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let sink_after = context
        .banks_client
        .get_account(Pubkey::new_from_array(WITHDRAWAL_SINK))
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(source_before - source_after, lamports);
    assert_eq!(sink_after - sink_before, lamports);

    let metadata = result.metadata.unwrap();
    let event_data = metadata
        .log_messages
        .iter()
        .find_map(|log| {
            log.strip_prefix("Program log: ")
                .unwrap_or(log)
                .strip_prefix(northstar_portal::TRANSFER_EVENT_LOG_PREFIX)
        })
        .unwrap();
    let event: NorthstarTransferEvent =
        borsh::from_slice(&BASE64_STANDARD.decode(event_data).unwrap()).unwrap();
    assert_eq!(event.kind, TransferEventKind::Withdrawal);
    assert_eq!(event.from, source.pubkey().to_bytes());
    assert_eq!(event.to, l1_recipient.to_bytes());
    assert_eq!(event.lamports, lamports);
    assert_eq!(event.pre_balance - event.post_balance, lamports);
}
