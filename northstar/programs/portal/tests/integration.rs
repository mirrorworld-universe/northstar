#![cfg(test)]

use {
    borsh::BorshDeserialize,
    northstar_portal::{FeeVault, Session, FEE_VAULT_DISCRIMINATOR, SESSION_DISCRIMINATOR},
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_program_test::{BanksClient, ProgramTest, ProgramTestContext},
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_system_interface::{instruction::transfer, program as system_program},
    solana_transaction::Transaction,
};

fn find_session_pda(program_id: &Pubkey, owner: &Pubkey, grid_id: u64) -> (Pubkey, u8) {
    let grid_id_bytes = grid_id.to_le_bytes();
    Pubkey::find_program_address(&[b"session", owner.as_ref(), &grid_id_bytes], program_id)
}

fn find_fee_vault_pda(program_id: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"fee_vault", owner.as_ref()], program_id)
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
    let mut data = vec![0u8];
    data.extend_from_slice(&grid_id.to_le_bytes());
    data.extend_from_slice(&ttl_slots.to_le_bytes());
    data.extend_from_slice(&fee_cap.to_le_bytes());

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

fn build_close_session_ix(
    program_id: &Pubkey,
    owner: &Pubkey,
    session_pda: &Pubkey,
    fee_vault_pda: &Pubkey,
    grid_id: u64,
) -> Instruction {
    let mut data = vec![1u8];
    data.extend_from_slice(&grid_id.to_le_bytes());

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

fn build_deposit_fee_ix(
    program_id: &Pubkey,
    owner: &Pubkey,
    fee_vault_pda: &Pubkey,
    amount: u64,
) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&amount.to_le_bytes());

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*owner, true),
            AccountMeta::new(*fee_vault_pda, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}

const PORTAL_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("GikCSCpYUq7QR7esoK6GM4UbJzKgdKNvS5bR1rBYH5E4");

async fn setup() -> ProgramTestContext {
    let mut program_test = ProgramTest::default();
    program_test.add_program("northstar_portal", PORTAL_PROGRAM_ID, None);
    program_test.start_with_context().await
}

async fn get_account_data(banks: &mut BanksClient, pubkey: &Pubkey) -> Option<Vec<u8>> {
    banks.get_account(*pubkey).await.unwrap().map(|a| a.data)
}

#[tokio::test]
async fn test_full_lifecycle() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID, &payer.pubkey(), 1);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID, &payer.pubkey());

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
    assert_eq!(session.discriminator, SESSION_DISCRIMINATOR);
    assert_eq!(session.grid_id, 1);
    assert_eq!(session.ttl_slots, 100);
    assert_eq!(session.fee_cap, 5_000_000_000);

    let vault_data = get_account_data(banks, &fee_vault_pda).await.unwrap();
    let vault = FeeVault::try_from_slice(&vault_data).unwrap();
    assert_eq!(vault.discriminator, FEE_VAULT_DISCRIMINATOR);
    assert_eq!(vault.balance, 0);

    let deposit_ix = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &fee_vault_pda,
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

    let vault_data = get_account_data(banks, &fee_vault_pda).await.unwrap();
    let vault = FeeVault::try_from_slice(&vault_data).unwrap();
    assert_eq!(vault.balance, 2_000_000_000);

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
        1,
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
async fn test_cannot_close_active_session() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID, &payer.pubkey(), 1);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID, &payer.pubkey());

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
        1,
    );

    let blockhash = banks.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[close_ix], Some(&payer.pubkey()), &[payer], blockhash);
    let result = banks.process_transaction(tx).await;
    assert!(result.is_err());
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

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID, &payer.pubkey(), 1);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID, &payer.pubkey());

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

    let _user_b_fee_vault_pda = find_fee_vault_pda(&PORTAL_PROGRAM_ID, &user_b.pubkey()).0;

    let deposit_ix = build_deposit_fee_ix(
        &PORTAL_PROGRAM_ID,
        &user_b.pubkey(),
        &fee_vault_pda,
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
    assert!(result.is_err());
}

#[tokio::test]
async fn test_multiple_deposits_accumulate() {
    let mut context = setup().await;
    let banks = &mut context.banks_client;
    let payer = &context.payer;

    let (session_pda, _) = find_session_pda(&PORTAL_PROGRAM_ID, &payer.pubkey(), 1);
    let (fee_vault_pda, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID, &payer.pubkey());

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
        &fee_vault_pda,
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
        &fee_vault_pda,
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

    let vault_data = get_account_data(banks, &fee_vault_pda).await.unwrap();
    let vault = FeeVault::try_from_slice(&vault_data).unwrap();
    assert_eq!(vault.balance, 3_000_000_000);
}

#[tokio::test]
async fn test_independent_grid_sessions() {
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

    let (session_pda_1, _) = find_session_pda(&PORTAL_PROGRAM_ID, &payer.pubkey(), 1);
    let (fee_vault_pda_1, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID, &payer.pubkey());

    let open_ix_1 = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer.pubkey(),
        &session_pda_1,
        &fee_vault_pda_1,
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

    let (session_pda_2, _) = find_session_pda(&PORTAL_PROGRAM_ID, &user_b.pubkey(), 1);
    let (fee_vault_pda_2, _) = find_fee_vault_pda(&PORTAL_PROGRAM_ID, &user_b.pubkey());

    let open_ix_2 = build_open_session_ix(
        &PORTAL_PROGRAM_ID,
        &user_b.pubkey(),
        &session_pda_2,
        &fee_vault_pda_2,
        1,
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
    banks.process_transaction(tx).await.unwrap();

    let session_data_1 = get_account_data(banks, &session_pda_1).await.unwrap();
    let session_1 = Session::try_from_slice(&session_data_1).unwrap();
    assert_eq!(session_1.grid_id, 1);

    let session_data_2 = get_account_data(banks, &session_pda_2).await.unwrap();
    let session_2 = Session::try_from_slice(&session_data_2).unwrap();
    assert_eq!(session_2.grid_id, 1);

    let (
        current_slot,
        new_blockhash,
        payer_keypair,
        payer_pubkey,
        session_pda_1_addr,
        fee_vault_pda_addr,
    ) = {
        let banks = &mut context.banks_client;
        let payer = &context.payer;
        let current_slot = banks.get_root_slot().await.unwrap();
        let new_blockhash = banks.get_latest_blockhash().await.unwrap();
        let payer_keypair = payer.insecure_clone();
        let payer_pubkey = payer_keypair.pubkey();
        let session_pda_1_addr = session_pda_1;
        let fee_vault_pda_addr = fee_vault_pda_1;
        (
            current_slot,
            new_blockhash,
            payer_keypair,
            payer_pubkey,
            session_pda_1_addr,
            fee_vault_pda_addr,
        )
    };

    context.warp_to_slot(current_slot + 110).unwrap();
    context.last_blockhash = new_blockhash;

    let banks = &mut context.banks_client;

    let close_ix_1 = build_close_session_ix(
        &PORTAL_PROGRAM_ID,
        &payer_pubkey,
        &session_pda_1_addr,
        &fee_vault_pda_addr,
        1,
    );

    let tx = Transaction::new_signed_with_payer(
        &[close_ix_1],
        Some(&payer_pubkey),
        &[&payer_keypair],
        context.last_blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    let session_data_1 = get_account_data(banks, &session_pda_1_addr).await;
    assert!(session_data_1.is_none() || session_data_1.unwrap().is_empty());

    let session_data_2 = get_account_data(banks, &session_pda_2).await.unwrap();
    let session_2 = Session::try_from_slice(&session_data_2).unwrap();
    assert_eq!(session_2.grid_id, 1);
}
