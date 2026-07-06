use {
    borsh::BorshDeserialize,
    northstar_portal::SessionBridge,
    northstar_token_bridge::{
        find_er_token_account_pda, find_token_vault_pda, instruction::TokenBridgeInstruction,
        state::ErTokenAccount,
    },
    solana_account::{Account, AccountSharedData, ReadableAccount},
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_program_pack::Pack,
    solana_program_test::{processor, ProgramTest, ProgramTestContext},
    solana_pubkey::Pubkey,
    solana_rent::Rent,
    solana_sdk_ids::{bpf_loader, system_program},
    solana_signer::Signer,
    solana_transaction::Transaction,
    spl_token_interface::state::{Account as SplTokenAccount, AccountState, Mint},
};

const PORTAL_PROGRAM_ID: Pubkey = Pubkey::new_from_array([7; 32]);
const DECIMALS: u8 = 6;

struct TestWorld {
    context: ProgramTestContext,
    payer: Keypair,
    alice: Keypair,
    bob: Keypair,
    mint: Pubkey,
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
        let session = Pubkey::new_unique();
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

        let mut program_test = ProgramTest::new(
            "northstar_token_bridge",
            northstar_token_bridge::id(),
            processor!(northstar_token_bridge::process_instruction),
        );
        for (address, account) in
            solana_program_binaries::by_id(&spl_token_interface::id(), &Rent::default()).unwrap()
        {
            program_test.add_account(address, shared_to_account(&account));
        }
        program_test.add_account(
            PORTAL_PROGRAM_ID,
            Account {
                lamports: 1,
                data: vec![],
                owner: bpf_loader::id(),
                executable: true,
                rent_epoch: 0,
            },
        );
        program_test.add_account(payer.pubkey(), system_account(5_000_000_000));
        program_test.add_account(alice.pubkey(), system_account(1_000_000_000));
        program_test.add_account(bob.pubkey(), system_account(1_000_000_000));
        program_test.add_account(mint, mint_account());
        program_test.add_account(alice_token, token_account(mint, alice.pubkey(), 1_000));
        program_test.add_account(bob_token, token_account(mint, bob.pubkey(), 0));
        program_test.add_account(vault_token, token_account(mint, vault, 0));
        let bridge = SessionBridge {
            discriminator: SessionBridge::DISCRIMINATOR,
            session: session.to_bytes(),
            mint: mint.to_bytes(),
            bridge_program: northstar_token_bridge::id().to_bytes(),
            vault: vault.to_bytes(),
            token_program: spl_token_interface::id().to_bytes(),
            bump: vault_bump,
        };
        program_test.add_account(
            session_bridge,
            Account {
                lamports: Rent::default().minimum_balance(SessionBridge::LEN),
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
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new_readonly(world.alice.pubkey(), true),
            AccountMeta::new_readonly(world.vault, false),
            AccountMeta::new(world.alice_er, false),
            AccountMeta::new_readonly(world.session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new(world.alice_token, false),
            AccountMeta::new(world.vault_token, false),
            AccountMeta::new_readonly(world.mint, false),
            AccountMeta::new_readonly(spl_token_interface::id(), false),
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
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new_readonly(world.bob.pubkey(), true),
            AccountMeta::new_readonly(world.vault, false),
            AccountMeta::new(world.bob_er, false),
            AccountMeta::new_readonly(world.session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new(world.vault_token, false),
            AccountMeta::new(world.bob_token, false),
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

async fn process(
    context: &mut ProgramTestContext,
    payer: &Pubkey,
    instructions: &[Instruction],
    signers: &[&Keypair],
) {
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(instructions, Some(payer), signers, blockhash);
    context.banks_client.process_transaction(tx).await.unwrap();
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
