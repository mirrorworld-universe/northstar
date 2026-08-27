use {
    borsh::BorshDeserialize,
    northstar_portal::{account_size, DelegationRecord, Session, SessionBridge, SettlementStatus},
    northstar_token_bridge::{
        find_buffer_pda, find_er_token_account_pda, find_token_vault_pda,
        instruction::TokenBridgeInstruction, state::ErTokenAccount,
    },
    solana_account::{Account, AccountSharedData, ReadableAccount},
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_program_pack::Pack,
    solana_program_test::{ProgramTest, ProgramTestContext},
    solana_pubkey::Pubkey,
    solana_rent::Rent,
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_transaction::Transaction,
    spl_token_interface::state::{Account as SplTokenAccount, AccountState, Mint},
};

const PORTAL_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("GikCSCpYUq7QR7esoK6GM4UbJzKgdKNvS5bR1rBYH5E4");
const DECIMALS: u8 = 6;

struct TestWorld {
    context: ProgramTestContext,
    payer: Keypair,
    alice: Keypair,
    bob: Keypair,
    mint: Pubkey,
    session: Pubkey,
    session_bridge: Pubkey,
    vault: Pubkey,
    alice_er: Pubkey,
    bob_er: Pubkey,
    alice_token: Pubkey,
    bob_token: Pubkey,
    vault_token: Pubkey,
}

impl TestWorld {
    async fn new() -> Self {
        let payer = Keypair::new();
        let alice = Keypair::new();
        let bob = Keypair::new();
        let mint = Pubkey::new_unique();
        let (session, session_bump) =
            Pubkey::find_program_address(&[Session::SEED_PREFIX], &PORTAL_PROGRAM_ID);
        let session_bridge = Pubkey::new_unique();
        let (vault, vault_bump) =
            find_token_vault_pda(&northstar_token_bridge::id(), &session_bridge);
        let (alice_er, _) = find_er_token_account_pda(
            &northstar_token_bridge::id(),
            &session_bridge,
            &alice.pubkey(),
        );
        let (bob_er, _) = find_er_token_account_pda(
            &northstar_token_bridge::id(),
            &session_bridge,
            &bob.pubkey(),
        );
        let alice_token = Pubkey::new_unique();
        let bob_token = Pubkey::new_unique();
        let vault_token = Pubkey::new_unique();

        let mut program_test =
            ProgramTest::new("northstar_token_bridge", northstar_token_bridge::id(), None);
        program_test.add_program("northstar_portal", PORTAL_PROGRAM_ID, None);
        for (address, account) in
            solana_program_binaries::by_id(&spl_token_interface::id(), &Rent::default()).unwrap()
        {
            program_test.add_account(address, shared_to_account(&account));
        }
        program_test.add_account(payer.pubkey(), system_account(5_000_000_000));
        program_test.add_account(alice.pubkey(), system_account(1_000_000_000));
        program_test.add_account(bob.pubkey(), system_account(1_000_000_000));
        let session_state = Session {
            discriminator: Session::DISCRIMINATOR,
            grid_id: 1,
            ttl_slots: 100,
            fee_cap: 0,
            created_at: 0,
            nonce: 0,
            authority: payer.pubkey(),
            validator: payer.pubkey(),
            settlement_interval_slots: 10,
            last_settled_l1_slot: 0,
            last_settled_er_slot: 0,
            settlement_status: SettlementStatus::Idle,
            settlement_er_slot: 0,
            settlement_checksum: [0; 32],
            settlement_accumulator: [0; 32],
            settlement_started_l1_slot: 0,
            bump: session_bump,
        };
        program_test.add_account(
            session,
            Account {
                lamports: Rent::default().minimum_balance(account_size(&session_state)),
                data: borsh::to_vec(&session_state).unwrap(),
                owner: PORTAL_PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            },
        );
        program_test.add_account(mint, mint_account());
        program_test.add_account(alice_token, token_account(mint, alice.pubkey(), 1_000));
        program_test.add_account(bob_token, token_account(mint, bob.pubkey(), 0));
        program_test.add_account(vault_token, token_account(mint, vault, 0));
        let bridge = SessionBridge {
            discriminator: SessionBridge::DISCRIMINATOR,
            session,
            mint,
            bridge_program: northstar_token_bridge::id(),
            vault,
            token_program: spl_token_interface::id(),
            bump: vault_bump,
        };
        program_test.add_account(
            session_bridge,
            Account {
                lamports: Rent::default().minimum_balance(account_size(&bridge)),
                data: borsh::to_vec(&bridge).unwrap(),
                owner: PORTAL_PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            },
        );

        let context = program_test.start_with_context().await;
        Self {
            context,
            payer,
            alice,
            bob,
            mint,
            session,
            session_bridge,
            vault,
            alice_er,
            bob_er,
            alice_token,
            bob_token,
            vault_token,
        }
    }

    async fn token_amount(&mut self, account: Pubkey) -> u64 {
        let account = self
            .context
            .banks_client
            .get_account(account)
            .await
            .unwrap()
            .unwrap();
        SplTokenAccount::unpack(&account.data).unwrap().amount
    }

    async fn er_amount(&mut self, account: Pubkey) -> u64 {
        let account = self
            .context
            .banks_client
            .get_account(account)
            .await
            .unwrap()
            .unwrap();
        ErTokenAccount::try_from_slice(&account.data)
            .unwrap()
            .amount
    }

    async fn account_owner(&mut self, account: Pubkey) -> Pubkey {
        self.context
            .banks_client
            .get_account(account)
            .await
            .unwrap()
            .unwrap()
            .owner
    }
}

#[tokio::test]
async fn deposit_transfer_and_withdraw_round_trip() {
    let mut world = TestWorld::new().await;

    let payer_pubkey = world.payer.pubkey();
    let initialize_vault = initialize_vault_ix(&world);
    process(
        &mut world.context,
        &payer_pubkey,
        &[initialize_vault],
        &[&world.payer],
    )
    .await;

    let initialize_alice_er = initialize_er_ix(&world, world.alice.pubkey(), world.alice_er);
    let initialize_bob_er = initialize_er_ix(&world, world.bob.pubkey(), world.bob_er);
    process(
        &mut world.context,
        &payer_pubkey,
        &[initialize_alice_er, initialize_bob_er],
        &[&world.payer],
    )
    .await;

    let deposit = deposit_ix(&world, 600);
    process(
        &mut world.context,
        &payer_pubkey,
        &[deposit],
        &[&world.payer, &world.alice],
    )
    .await;
    assert_eq!(world.token_amount(world.alice_token).await, 400);
    assert_eq!(world.token_amount(world.vault_token).await, 600);
    assert_eq!(world.er_amount(world.alice_er).await, 600);

    let transfer = transfer_ix(&world, 250);
    process(
        &mut world.context,
        &payer_pubkey,
        &[transfer],
        &[&world.payer, &world.alice],
    )
    .await;
    assert_eq!(world.er_amount(world.alice_er).await, 350);
    assert_eq!(world.er_amount(world.bob_er).await, 250);

    let withdraw = withdraw_ix(&world, 200);
    process(
        &mut world.context,
        &payer_pubkey,
        &[withdraw],
        &[&world.payer, &world.bob],
    )
    .await;
    assert_eq!(world.token_amount(world.bob_token).await, 200);
    assert_eq!(world.token_amount(world.vault_token).await, 400);
    assert_eq!(world.er_amount(world.bob_er).await, 50);
    let vault_account = world
        .context
        .banks_client
        .get_account(world.vault)
        .await
        .unwrap()
        .unwrap();
    let vault_state =
        borsh::from_slice::<northstar_token_bridge::state::TokenVault>(&vault_account.data)
            .unwrap();
    assert_eq!(vault_state.deposited, 600);
    assert_eq!(vault_state.withdrawn, 200);
}

#[tokio::test]
async fn transfer_rejects_identical_source_and_destination() {
    let mut world = TestWorld::new().await;
    let payer_pubkey = world.payer.pubkey();

    let initialize_vault = initialize_vault_ix(&world);
    let initialize_alice_er = initialize_er_ix(&world, world.alice.pubkey(), world.alice_er);
    process(
        &mut world.context,
        &payer_pubkey,
        &[initialize_vault, initialize_alice_er],
        &[&world.payer],
    )
    .await;
    let deposit = deposit_ix(&world, 600);
    process(
        &mut world.context,
        &payer_pubkey,
        &[deposit],
        &[&world.payer, &world.alice],
    )
    .await;

    let self_transfer = Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new_readonly(world.alice.pubkey(), true),
            AccountMeta::new(world.alice_er, false),
            AccountMeta::new(world.alice_er, false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::Transfer { amount: 600 }).unwrap(),
    };
    let result = process_result(
        &mut world.context,
        &payer_pubkey,
        &[self_transfer],
        &[&world.payer, &world.alice],
    )
    .await;

    assert!(result.is_err(), "self-transfer must be rejected");
    assert_eq!(world.er_amount(world.alice_er).await, 600);
}

#[tokio::test]
async fn delegate_and_undelegate_preserve_er_token_account_state() {
    let mut world = TestWorld::new().await;
    let payer_pubkey = world.payer.pubkey();

    let initialize_vault = initialize_vault_ix(&world);
    let initialize_alice_er = initialize_er_ix(&world, world.alice.pubkey(), world.alice_er);
    let deposit = deposit_ix(&world, 600);
    process(
        &mut world.context,
        &payer_pubkey,
        &[initialize_vault, initialize_alice_er],
        &[&world.payer],
    )
    .await;
    process(
        &mut world.context,
        &payer_pubkey,
        &[deposit],
        &[&world.payer, &world.alice],
    )
    .await;

    let delegate = delegate_ix(&world);
    process(
        &mut world.context,
        &payer_pubkey,
        &[delegate],
        &[&world.payer],
    )
    .await;
    assert_eq!(world.account_owner(world.alice_er).await, PORTAL_PROGRAM_ID);

    let undelegate = undelegate_ix(&world);
    process(
        &mut world.context,
        &payer_pubkey,
        &[undelegate],
        &[&world.payer],
    )
    .await;
    assert_eq!(
        world.account_owner(world.alice_er).await,
        northstar_token_bridge::id()
    );
    assert_eq!(world.er_amount(world.alice_er).await, 600);

    let withdraw = withdraw_alice_ix(&world, 100);
    process(
        &mut world.context,
        &payer_pubkey,
        &[withdraw],
        &[&world.payer, &world.alice],
    )
    .await;
    assert_eq!(world.token_amount(world.alice_token).await, 500);
    assert_eq!(world.er_amount(world.alice_er).await, 500);
}

fn initialize_vault_ix(world: &TestWorld) -> Instruction {
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(world.payer.pubkey(), true),
            AccountMeta::new(world.vault, false),
            AccountMeta::new_readonly(world.session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(world.vault_token, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::InitializeVault).unwrap(),
    }
}

fn initialize_er_ix(world: &TestWorld, owner: Pubkey, er_account: Pubkey) -> Instruction {
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(world.payer.pubkey(), true),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(world.session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::InitializeErTokenAccount {
            owner: owner.to_bytes(),
        })
        .unwrap(),
    }
}

fn deposit_ix(world: &TestWorld, amount: u64) -> Instruction {
    let (deposit_receipt, _) = northstar_token_bridge::find_token_deposit_receipt_pda(
        &northstar_token_bridge::id(),
        &world.session_bridge,
        &world.alice_er,
    );
    let delegation_record = Pubkey::find_program_address(
        &[b"delegation", world.alice_er.as_ref()],
        &PORTAL_PROGRAM_ID,
    )
    .0;
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(world.alice.pubkey(), true),
            AccountMeta::new(world.vault, false),
            AccountMeta::new(world.alice_er, false),
            AccountMeta::new_readonly(world.session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new(world.alice_token, false),
            AccountMeta::new(world.vault_token, false),
            AccountMeta::new_readonly(world.mint, false),
            AccountMeta::new_readonly(spl_token_interface::id(), false),
            AccountMeta::new(deposit_receipt, false),
            AccountMeta::new_readonly(delegation_record, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::Deposit {
            amount,
            decimals: DECIMALS,
        })
        .unwrap(),
    }
}

fn transfer_ix(world: &TestWorld, amount: u64) -> Instruction {
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new_readonly(world.alice.pubkey(), true),
            AccountMeta::new(world.alice_er, false),
            AccountMeta::new(world.bob_er, false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::Transfer { amount }).unwrap(),
    }
}

fn withdraw_ix(world: &TestWorld, amount: u64) -> Instruction {
    withdraw_ix_for(
        world,
        world.bob.pubkey(),
        world.bob_er,
        world.bob_token,
        amount,
    )
}

fn withdraw_alice_ix(world: &TestWorld, amount: u64) -> Instruction {
    withdraw_ix_for(
        world,
        world.alice.pubkey(),
        world.alice_er,
        world.alice_token,
        amount,
    )
}

fn withdraw_ix_for(
    world: &TestWorld,
    owner: Pubkey,
    er_account: Pubkey,
    destination_token: Pubkey,
    amount: u64,
) -> Instruction {
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new(world.vault, false),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(world.session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new(world.vault_token, false),
            AccountMeta::new(destination_token, false),
            AccountMeta::new_readonly(world.mint, false),
            AccountMeta::new_readonly(spl_token_interface::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::Withdraw {
            amount,
            decimals: DECIMALS,
        })
        .unwrap(),
    }
}

fn delegate_ix(world: &TestWorld) -> Instruction {
    let (delegation_record, _) = Pubkey::find_program_address(
        &[DelegationRecord::SEED_PREFIX, world.alice_er.as_ref()],
        &PORTAL_PROGRAM_ID,
    );
    let (buffer, _) = find_buffer_pda(&northstar_token_bridge::id(), &world.alice_er);
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(world.payer.pubkey(), true),
            AccountMeta::new(world.alice_er, false),
            AccountMeta::new_readonly(northstar_token_bridge::id(), false),
            AccountMeta::new_readonly(world.session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(world.session, false),
            AccountMeta::new(delegation_record, false),
            AccountMeta::new(buffer, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::DelegateErTokenAccount { grid_id: 1 })
            .unwrap(),
    }
}

fn undelegate_ix(world: &TestWorld) -> Instruction {
    let (delegation_record, _) = Pubkey::find_program_address(
        &[DelegationRecord::SEED_PREFIX, world.alice_er.as_ref()],
        &PORTAL_PROGRAM_ID,
    );
    let (buffer, _) = find_buffer_pda(&northstar_token_bridge::id(), &world.alice_er);
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(world.payer.pubkey(), true),
            AccountMeta::new(world.alice_er, false),
            AccountMeta::new_readonly(northstar_token_bridge::id(), false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(world.session, false),
            AccountMeta::new(delegation_record, false),
            AccountMeta::new(buffer, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::UndelegateErTokenAccount).unwrap(),
    }
}

async fn process(
    context: &mut ProgramTestContext,
    payer: &Pubkey,
    instructions: &[Instruction],
    signers: &[&Keypair],
) {
    process_result(context, payer, instructions, signers)
        .await
        .unwrap();
}

async fn process_result(
    context: &mut ProgramTestContext,
    payer: &Pubkey,
    instructions: &[Instruction],
    signers: &[&Keypair],
) -> Result<(), solana_program_test::BanksClientError> {
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(instructions, Some(payer), signers, blockhash);
    context.banks_client.process_transaction(tx).await
}

fn shared_to_account(account: &AccountSharedData) -> Account {
    Account {
        lamports: account.lamports(),
        data: account.data().to_vec(),
        owner: *account.owner(),
        executable: account.executable(),
        rent_epoch: account.rent_epoch(),
    }
}

fn system_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn mint_account() -> Account {
    let mut data = vec![0; Mint::LEN];
    Pack::pack(
        Mint {
            mint_authority: None.into(),
            supply: 1_000,
            decimals: DECIMALS,
            is_initialized: true,
            freeze_authority: None.into(),
        },
        &mut data,
    )
    .unwrap();
    Account {
        lamports: Rent::default().minimum_balance(Mint::LEN),
        data,
        owner: spl_token_interface::id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn token_account(mint: Pubkey, owner: Pubkey, amount: u64) -> Account {
    let mut data = vec![0; SplTokenAccount::LEN];
    Pack::pack(
        SplTokenAccount {
            mint,
            owner,
            amount,
            delegate: None.into(),
            state: AccountState::Initialized,
            is_native: None.into(),
            delegated_amount: 0,
            close_authority: None.into(),
        },
        &mut data,
    )
    .unwrap();
    Account {
        lamports: Rent::default().minimum_balance(SplTokenAccount::LEN),
        data,
        owner: spl_token_interface::id(),
        executable: false,
        rent_epoch: 0,
    }
}
