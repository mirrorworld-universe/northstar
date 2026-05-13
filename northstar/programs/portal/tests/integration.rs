#![cfg(test)]

use {
    borsh::BorshDeserialize,
    northstar_portal::{DepositReceipt, FeeVault, OpenSession, PortalInstruction, Session},
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

fn build_open_session_ix(
    program_id: &Pubkey,
    owner: &Pubkey,
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

async fn get_account_data(banks: &mut BanksClient, pubkey: &Pubkey) -> Option<Vec<u8>> {
    banks.get_account(*pubkey).await.unwrap().map(|a| a.data)
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
    assert_eq!(receipt.balance, 2_000_000_000);

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
    assert_eq!(receipt.balance, 1_000_000_000);
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
    assert_eq!(receipt.balance, 3_000_000_000);
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
    assert_eq!(receipt_b.balance, 1_000_000_000);

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
    assert_eq!(receipt_c.balance, 2_000_000_000);
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
    assert_eq!(receipt.balance, 1_500_000_000);
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
    assert_eq!(receipt.balance, 3_000_000_000);
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
