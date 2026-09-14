use {super::*, northstar_token_bridge::state::TokenDepositProgress};

fn credit_ix(world: &TestWorld, baseline: u64, balance: u64) -> Instruction {
    let program = northstar_token_bridge::id();
    let origin = Pubkey::find_program_address(
        &[TokenDepositProgress::ORIGIN_SEED, world.alice_er.as_ref()],
        &program,
    )
    .0;
    let cursor = Pubkey::find_program_address(
        &[
            TokenDepositProgress::CURSOR_SEED,
            world.alice_er.as_ref(),
            &baseline.to_le_bytes(),
        ],
        &program,
    )
    .0;
    let receipt = northstar_token_bridge::find_token_deposit_receipt_pda(
        &program,
        &world.session_bridge,
        &world.alice_er,
    )
    .0;
    let delegation = Pubkey::find_program_address(
        &[DelegationRecord::SEED_PREFIX, world.alice_er.as_ref()],
        &PORTAL_PROGRAM_ID,
    )
    .0;
    Instruction {
        program_id: program,
        accounts: vec![
            AccountMeta::new(world.payer.pubkey(), true),
            AccountMeta::new(world.alice_er, false),
            AccountMeta::new_readonly(world.session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(world.session, false),
            AccountMeta::new_readonly(delegation, false),
            AccountMeta::new_readonly(receipt, false),
            AccountMeta::new_readonly(origin, false),
            AccountMeta::new(cursor, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::ApplyTokenDeposit { balance }).unwrap(),
    }
}

#[tokio::test]
async fn receipt_credit_is_er_only_bound_and_applied_once() {
    let mut world = TestWorld::new().await;
    let payer = world.payer.pubkey();
    let initialize = [
        initialize_vault_ix(&world),
        initialize_er_ix(&world, world.alice.pubkey(), world.alice_er),
    ];
    process(&mut world.context, &payer, &initialize, &[&world.payer]).await;
    let deposit = deposit_ix(&world, 600);
    process(
        &mut world.context,
        &payer,
        &[deposit],
        &[&world.payer, &world.alice],
    )
    .await;
    let delegate = delegate_ix(&world);
    process(
        &mut world.context,
        &payer,
        &[delegate],
        &[&world.payer, &world.alice],
    )
    .await;
    let deposit = deposit_ix(&world, 100);
    process(
        &mut world.context,
        &payer,
        &[deposit],
        &[&world.payer, &world.alice],
    )
    .await;
    let credit = credit_ix(&world, 600, 700);
    assert!(process_result(
        &mut world.context,
        &payer,
        std::slice::from_ref(&credit),
        &[&world.payer]
    )
    .await
    .is_err());
    assert_eq!(world.er_amount(world.alice_er).await, 600);

    // Program-test models ER hydration; live E2E covers actual bank execution and checkpoint capture.
    let mut account = world
        .context
        .banks_client
        .get_account(world.alice_er)
        .await
        .unwrap()
        .unwrap();
    account.owner = northstar_token_bridge::id();
    world
        .context
        .set_account(&world.alice_er, &AccountSharedData::from(account));
    for (nonce, invalid) in [600, 701].into_iter().enumerate() {
        let instruction = credit_ix(&world, 600, invalid);
        let prefix = system_instruction::transfer(&payer, &world.bob.pubkey(), nonce as u64 + 1);
        assert!(process_result(
            &mut world.context,
            &payer,
            &[prefix, instruction],
            &[&world.payer]
        )
        .await
        .is_err());
        assert_eq!(world.er_amount(world.alice_er).await, 600);
    }
    for index in [2, 5, 6, 7, 8] {
        let mut invalid = credit.clone();
        invalid.accounts[index].pubkey = world.bob.pubkey();
        assert!(
            process_result(&mut world.context, &payer, &[invalid], &[&world.payer])
                .await
                .is_err()
        );
        assert_eq!(world.er_amount(world.alice_er).await, 600);
    }
    let mut unauthorized = credit.clone();
    unauthorized.accounts[0].pubkey = world.alice.pubkey();
    assert!(process_result(
        &mut world.context,
        &payer,
        &[unauthorized],
        &[&world.payer, &world.alice]
    )
    .await
    .is_err());
    assert_eq!(world.er_amount(world.alice_er).await, 600);

    let prefix = system_instruction::transfer(&payer, &world.bob.pubkey(), 3);
    process(
        &mut world.context,
        &payer,
        &[prefix, credit.clone()],
        &[&world.payer],
    )
    .await;
    assert_eq!(world.er_amount(world.alice_er).await, 700);
    let cursor = world
        .context
        .banks_client
        .get_account(credit.accounts[8].pubkey)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        borsh::from_slice::<TokenDepositProgress>(&cursor.data)
            .unwrap()
            .balance,
        700
    );
    let prefix = system_instruction::transfer(&payer, &world.bob.pubkey(), 4);
    assert!(process_result(
        &mut world.context,
        &payer,
        &[prefix, credit.clone()],
        &[&world.payer]
    )
    .await
    .is_err());
    assert_eq!(world.er_amount(world.alice_er).await, 700);
    assert_eq!(
        world
            .context
            .banks_client
            .get_account(credit.accounts[8].pubkey)
            .await
            .unwrap()
            .unwrap(),
        cursor
    );
}
