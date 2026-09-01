use {
    borsh::BorshDeserialize,
    northstar_portal::{OpenSession, PortalInstruction, RegisterSessionBridge},
    northstar_token_bridge::{
        find_buffer_pda, find_er_token_account_pda, find_token_vault_pda,
        instruction::TokenBridgeInstruction, state::ErTokenAccount,
    },
    solana_commitment_config::CommitmentConfig,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_native_token::LAMPORTS_PER_SOL,
    solana_program_pack::Pack,
    solana_pubkey::Pubkey,
    solana_rpc_client::rpc_client::RpcClient,
    solana_rpc_client_api::config::RpcSendTransactionConfig,
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_system_interface::instruction as system_instruction,
    solana_transaction::Transaction,
    solana_transaction_status_client_types::UiTransactionEncoding,
    spl_token_interface::{
        instruction as token_instruction,
        state::{Account as SplTokenAccount, Mint},
    },
    std::{
        net::TcpListener,
        path::PathBuf,
        process::{Child, Command, Stdio},
        thread::sleep,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
};

const DECIMALS: u8 = 6;
const GRID_ID: u64 = 1;
const PORTAL_PROGRAM_ID: Pubkey =
    solana_pubkey::pubkey!("5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf");
const DEPOSIT_AMOUNT: u64 = 600_000_000;
const TRANSFER_AMOUNT: u64 = 250_000_000;
const WITHDRAW_AMOUNT: u64 = 200_000_000;

#[test]
fn live_validator_spl_token_bridge_round_trip() {
    let payer = Keypair::new();
    let bob = Keypair::new();
    let er_fee_payer = Keypair::new();
    let ports = TestPorts::new();
    let ledger = unique_ledger_path();
    let mut validator = ValidatorProcess::start(&payer.pubkey(), &ledger, &ports);

    let rpc = RpcClient::new_with_commitment(ports.rpc_url(), CommitmentConfig::confirmed());
    let er_rpc = RpcClient::new_with_commitment(ports.er_rpc_url(), CommitmentConfig::processed());
    wait_for_health(&rpc);

    let token_bridge_program = rpc
        .get_account(&northstar_token_bridge::id())
        .expect("token bridge loaded into validator genesis");
    assert!(token_bridge_program.executable);

    let validator_identity = rpc.get_identity().expect("validator identity");
    let session = portal_pubkey(northstar_portal::find_session_pda(&PORTAL_PROGRAM_ID));
    let fee_vault = portal_pubkey(northstar_portal::find_fee_vault_pda(&PORTAL_PROGRAM_ID));

    send_tx(
        &rpc,
        &[open_session_ix(
            &payer.pubkey(),
            session,
            fee_vault,
            validator_identity,
        )],
        &payer.pubkey(),
        &[&payer],
    );
    wait_for_health(&er_rpc);
    send_tx(
        &rpc,
        &[system_instruction::transfer(
            &payer.pubkey(),
            &bob.pubkey(),
            LAMPORTS_PER_SOL / 10,
        )],
        &payer.pubkey(),
        &[&payer],
    );

    let mint = Keypair::new();
    let alice_token = Keypair::new();
    let bob_token = Keypair::new();
    let vault_token = Keypair::new();
    let session_bridge = portal_pubkey(northstar_portal::find_session_bridge_pda(
        &PORTAL_PROGRAM_ID,
        &session,
        &mint.pubkey(),
    ));
    let (vault, _) = find_token_vault_pda(&northstar_token_bridge::id(), &session_bridge);
    let (alice_er, _) = find_er_token_account_pda(
        &northstar_token_bridge::id(),
        &session_bridge,
        &payer.pubkey(),
    );
    let (bob_er, _) = find_er_token_account_pda(
        &northstar_token_bridge::id(),
        &session_bridge,
        &bob.pubkey(),
    );

    eprintln!("creating L1 mint and token accounts");
    create_l1_mint_and_token_accounts(
        &rpc,
        &payer,
        &mint,
        &alice_token,
        &bob_token,
        bob.pubkey(),
        &vault_token,
        vault,
    );
    wait_token_amount(&rpc, alice_token.pubkey(), 1_000_000_000);

    eprintln!("rejecting non-canonical vault registration");
    assert_tx_rejected(
        &rpc,
        &[register_session_bridge_ix(
            &bob.pubkey(),
            session,
            session_bridge,
            mint.pubkey(),
            Pubkey::new_unique(),
        )],
        &payer.pubkey(),
        &[&payer, &bob],
    );
    assert!(rpc.get_account(&session_bridge).is_err());

    eprintln!("registering session bridge with non-session authority");
    send_tx(
        &rpc,
        &[register_session_bridge_ix(
            &bob.pubkey(),
            session,
            session_bridge,
            mint.pubkey(),
            vault,
        )],
        &payer.pubkey(),
        &[&payer, &bob],
    );
    eprintln!("initializing bridge accounts");
    send_tx(
        &rpc,
        &[
            initialize_vault_ix(&payer.pubkey(), session_bridge, vault, vault_token.pubkey()),
            initialize_er_ix(&payer.pubkey(), session_bridge, payer.pubkey(), alice_er),
            initialize_er_ix(&payer.pubkey(), session_bridge, bob.pubkey(), bob_er),
        ],
        &payer.pubkey(),
        &[&payer],
    );

    eprintln!("delegating ER fee payer");
    fund_and_delegate_er_fee_payer(&rpc, &payer, &er_fee_payer, session);

    eprintln!("delegating ER token accounts");
    send_tx(
        &rpc,
        &[
            delegate_er_ix(
                &payer.pubkey(),
                &payer.pubkey(),
                session,
                session_bridge,
                alice_er,
            ),
            delegate_er_ix(
                &payer.pubkey(),
                &bob.pubkey(),
                session,
                session_bridge,
                bob_er,
            ),
        ],
        &payer.pubkey(),
        &[&payer, &bob],
    );
    eprintln!("waiting for delegated ER accounts: alice={alice_er}, bob={bob_er}");
    wait_for_er_amount(&er_rpc, alice_er, 0);
    wait_for_er_amount(&er_rpc, bob_er, 0);

    send_tx(
        &rpc,
        &[deposit_ix(
            payer.pubkey(),
            payer.pubkey(),
            vault,
            alice_er,
            session_bridge,
            alice_token.pubkey(),
            vault_token.pubkey(),
            mint.pubkey(),
            DEPOSIT_AMOUNT,
        )],
        &payer.pubkey(),
        &[&payer],
    );
    wait_token_amount(&rpc, alice_token.pubkey(), 400_000_000);
    wait_token_amount(&rpc, vault_token.pubkey(), DEPOSIT_AMOUNT);
    wait_for_er_amount(&er_rpc, alice_er, DEPOSIT_AMOUNT);

    submit_tx(
        &er_rpc,
        &[transfer_ix(
            payer.pubkey(),
            alice_er,
            bob_er,
            TRANSFER_AMOUNT,
        )],
        &er_fee_payer.pubkey(),
        &[&er_fee_payer, &payer],
        RpcSendTransactionConfig {
            skip_preflight: true,
            ..RpcSendTransactionConfig::default()
        },
    );
    wait_for_er_amount(&er_rpc, alice_er, DEPOSIT_AMOUNT - TRANSFER_AMOUNT);
    wait_for_er_amount(&er_rpc, bob_er, TRANSFER_AMOUNT);

    submit_tx(
        &er_rpc,
        &[start_withdrawal_ix(
            bob.pubkey(),
            bob_er,
            session_bridge,
            bob_token.pubkey(),
            WITHDRAW_AMOUNT,
        )],
        &er_fee_payer.pubkey(),
        &[&er_fee_payer, &bob],
        RpcSendTransactionConfig {
            skip_preflight: true,
            ..RpcSendTransactionConfig::default()
        },
    );
    wait_for_er_amount(&er_rpc, bob_er, TRANSFER_AMOUNT - WITHDRAW_AMOUNT);
    wait_for_er_amount(&rpc, bob_er, TRANSFER_AMOUNT - WITHDRAW_AMOUNT);
    wait_token_amount(&rpc, bob_token.pubkey(), WITHDRAW_AMOUNT);
    wait_token_amount(&rpc, vault_token.pubkey(), DEPOSIT_AMOUNT - WITHDRAW_AMOUNT);

    send_tx(
        &rpc,
        &[undelegate_er_ix(&bob.pubkey(), session, bob_er)],
        &payer.pubkey(),
        &[&payer, &bob],
    );
    wait_account_owner(&rpc, bob_er, northstar_token_bridge::id());
    wait_for_er_amount(&rpc, bob_er, TRANSFER_AMOUNT - WITHDRAW_AMOUNT);

    validator.kill();
    let _ = std::fs::remove_dir_all(&ledger);
}

fn open_session_ix(
    payer: &Pubkey,
    session: Pubkey,
    fee_vault: Pubkey,
    validator: Pubkey,
) -> Instruction {
    Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(session, false),
            AccountMeta::new(fee_vault, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&PortalInstruction::OpenSession(OpenSession {
            grid_id: GRID_ID,
            ttl_slots: 2_000,
            fee_cap: 1_000_000,
            validator,
            settlement_interval_slots: 10,
        }))
        .unwrap(),
    }
}

fn register_session_bridge_ix(
    registrant: &Pubkey,
    session: Pubkey,
    session_bridge: Pubkey,
    mint: Pubkey,
    vault: Pubkey,
) -> Instruction {
    Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*registrant, true),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(session_bridge, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(northstar_token_bridge::id(), false),
            AccountMeta::new_readonly(spl_token_interface::id(), false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&PortalInstruction::RegisterSessionBridge(
            RegisterSessionBridge {
                mint,
                bridge_program: northstar_token_bridge::id(),
                vault,
                token_program: spl_token_interface::id(),
            },
        ))
        .unwrap(),
    }
}

fn initialize_vault_ix(
    payer: &Pubkey,
    session_bridge: Pubkey,
    vault: Pubkey,
    vault_token: Pubkey,
) -> Instruction {
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(vault_token, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::InitializeVault).unwrap(),
    }
}

fn initialize_er_ix(
    payer: &Pubkey,
    session_bridge: Pubkey,
    owner: Pubkey,
    er_account: Pubkey,
) -> Instruction {
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::InitializeErTokenAccount {
            owner: owner.to_bytes(),
        })
        .unwrap(),
    }
}

fn deposit_ix(
    payer: Pubkey,
    owner: Pubkey,
    vault: Pubkey,
    er_account: Pubkey,
    session_bridge: Pubkey,
    source_token: Pubkey,
    vault_token: Pubkey,
    mint: Pubkey,
    amount: u64,
) -> Instruction {
    let (deposit_receipt, _) = northstar_token_bridge::find_token_deposit_receipt_pda(
        &northstar_token_bridge::id(),
        &session_bridge,
        &er_account,
    );
    let delegation_record = portal_pubkey(northstar_portal::find_delegation_record_pda(
        &PORTAL_PROGRAM_ID,
        &er_account,
    ));
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new(vault, false),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new(source_token, false),
            AccountMeta::new(vault_token, false),
            AccountMeta::new_readonly(mint, false),
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

fn transfer_ix(
    authority: Pubkey,
    source_er: Pubkey,
    destination_er: Pubkey,
    amount: u64,
) -> Instruction {
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(source_er, false),
            AccountMeta::new(destination_er, false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::Transfer { amount }).unwrap(),
    }
}

fn start_withdrawal_ix(
    owner: Pubkey,
    er_account: Pubkey,
    session_bridge: Pubkey,
    destination_token: Pubkey,
    amount: u64,
) -> Instruction {
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(destination_token, false),
            AccountMeta::new_readonly(spl_token_interface::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::StartWithdrawal {
            amount,
            decimals: DECIMALS,
        })
        .unwrap(),
    }
}

fn delegate_er_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    session: Pubkey,
    session_bridge: Pubkey,
    er_account: Pubkey,
) -> Instruction {
    let delegation_record = portal_pubkey(northstar_portal::find_delegation_record_pda(
        &PORTAL_PROGRAM_ID,
        &er_account,
    ));
    let (buffer, _) = find_buffer_pda(&northstar_token_bridge::id(), &er_account);
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(northstar_token_bridge::id(), false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(delegation_record, false),
            AccountMeta::new(buffer, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::DelegateErTokenAccount { grid_id: GRID_ID })
            .unwrap(),
    }
}

fn undelegate_er_ix(authority: &Pubkey, session: Pubkey, er_account: Pubkey) -> Instruction {
    let delegation_record = portal_pubkey(northstar_portal::find_delegation_record_pda(
        &PORTAL_PROGRAM_ID,
        &er_account,
    ));
    let (buffer, _) = find_buffer_pda(&northstar_token_bridge::id(), &er_account);
    Instruction {
        program_id: northstar_token_bridge::id(),
        accounts: vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(northstar_token_bridge::id(), false),
            AccountMeta::new_readonly(PORTAL_PROGRAM_ID, false),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(delegation_record, false),
            AccountMeta::new(buffer, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::UndelegateErTokenAccount).unwrap(),
    }
}

fn fund_and_delegate_er_fee_payer(
    rpc: &RpcClient,
    payer: &Keypair,
    er_fee_payer: &Keypair,
    session: Pubkey,
) {
    send_tx(
        rpc,
        &[system_instruction::transfer(
            &payer.pubkey(),
            &er_fee_payer.pubkey(),
            LAMPORTS_PER_SOL / 10,
        )],
        &payer.pubkey(),
        &[payer],
    );

    let delegation_record = portal_pubkey(northstar_portal::find_delegation_record_pda(
        &PORTAL_PROGRAM_ID,
        &er_fee_payer.pubkey(),
    ));
    let ix = Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(er_fee_payer.pubkey(), true),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(delegation_record, false),
            AccountMeta::new_readonly(payer.pubkey(), false),
        ],
        data: borsh::to_vec(&PortalInstruction::Delegate { grid_id: GRID_ID }).unwrap(),
    };
    send_tx(
        rpc,
        &[
            system_instruction::assign(&er_fee_payer.pubkey(), &PORTAL_PROGRAM_ID),
            ix,
        ],
        &payer.pubkey(),
        &[payer, er_fee_payer],
    );
}

fn create_l1_mint_and_token_accounts(
    rpc: &RpcClient,
    payer: &Keypair,
    mint: &Keypair,
    alice_token: &Keypair,
    bob_token: &Keypair,
    bob_owner: Pubkey,
    vault_token: &Keypair,
    vault: Pubkey,
) {
    let mint_rent = rpc
        .get_minimum_balance_for_rent_exemption(Mint::LEN)
        .unwrap();
    let token_rent = rpc
        .get_minimum_balance_for_rent_exemption(SplTokenAccount::LEN)
        .unwrap();
    let instructions = vec![
        system_instruction::create_account(
            &payer.pubkey(),
            &mint.pubkey(),
            mint_rent,
            Mint::LEN as u64,
            &spl_token_interface::id(),
        ),
        token_instruction::initialize_mint(
            &spl_token_interface::id(),
            &mint.pubkey(),
            &payer.pubkey(),
            None,
            DECIMALS,
        )
        .unwrap(),
        create_token_account_ix(
            payer,
            alice_token,
            payer.pubkey(),
            mint.pubkey(),
            token_rent,
        ),
        token_instruction::initialize_account(
            &spl_token_interface::id(),
            &alice_token.pubkey(),
            &mint.pubkey(),
            &payer.pubkey(),
        )
        .unwrap(),
        create_token_account_ix(payer, bob_token, bob_owner, mint.pubkey(), token_rent),
        token_instruction::initialize_account(
            &spl_token_interface::id(),
            &bob_token.pubkey(),
            &mint.pubkey(),
            &bob_owner,
        )
        .unwrap(),
        create_token_account_ix(payer, vault_token, vault, mint.pubkey(), token_rent),
        token_instruction::initialize_account(
            &spl_token_interface::id(),
            &vault_token.pubkey(),
            &mint.pubkey(),
            &vault,
        )
        .unwrap(),
        token_instruction::mint_to(
            &spl_token_interface::id(),
            &mint.pubkey(),
            &alice_token.pubkey(),
            &payer.pubkey(),
            &[],
            1_000_000_000,
        )
        .unwrap(),
    ];
    send_tx(
        rpc,
        &instructions,
        &payer.pubkey(),
        &[payer, mint, alice_token, bob_token, vault_token],
    );
}

fn create_token_account_ix(
    payer: &Keypair,
    account: &Keypair,
    _owner: Pubkey,
    _mint: Pubkey,
    lamports: u64,
) -> Instruction {
    system_instruction::create_account(
        &payer.pubkey(),
        &account.pubkey(),
        lamports,
        SplTokenAccount::LEN as u64,
        &spl_token_interface::id(),
    )
}

fn wait_token_amount(rpc: &RpcClient, token_account: Pubkey, expected: u64) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(account) = rpc.get_account(&token_account) {
            if let Ok(state) = SplTokenAccount::unpack(&account.data) {
                if state.amount == expected {
                    return;
                }
            }
        }
        sleep(Duration::from_millis(250));
    }
    panic!("timed out waiting for token account {token_account} amount {expected}");
}

fn wait_account_owner(rpc: &RpcClient, account: Pubkey, expected_owner: Pubkey) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(account_data) = rpc.get_account(&account) {
            if account_data.owner == expected_owner {
                return;
            }
        }
        sleep(Duration::from_millis(250));
    }
    panic!("timed out waiting for account {account} owner {expected_owner}");
}

fn wait_for_er_amount(rpc: &RpcClient, er_account: Pubkey, expected: u64) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Ok(account) = rpc.get_account(&er_account) {
            if let Ok(state) = ErTokenAccount::try_from_slice(&account.data) {
                if state.amount == expected {
                    return;
                }
            }
        }
        sleep(Duration::from_millis(500));
    }
    panic!("timed out waiting for {er_account} amount {expected}");
}

fn send_tx(rpc: &RpcClient, instructions: &[Instruction], payer: &Pubkey, signers: &[&Keypair]) {
    send_tx_with_config(
        rpc,
        instructions,
        payer,
        signers,
        RpcSendTransactionConfig {
            skip_preflight: true,
            max_retries: Some(20),
            ..RpcSendTransactionConfig::default()
        },
    );
}

fn assert_tx_rejected(
    rpc: &RpcClient,
    instructions: &[Instruction],
    payer: &Pubkey,
    signers: &[&Keypair],
) {
    let blockhash = rpc.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(instructions, Some(payer), signers, blockhash);
    let simulation = rpc.simulate_transaction(&tx).unwrap().value;
    assert!(
        simulation.err.is_some(),
        "transaction unexpectedly succeeded: {:?}",
        simulation.logs
    );
}

fn send_tx_with_config(
    rpc: &RpcClient,
    instructions: &[Instruction],
    payer: &Pubkey,
    signers: &[&Keypair],
    config: RpcSendTransactionConfig,
) {
    let signature = submit_tx(rpc, instructions, payer, signers, config);
    rpc.poll_for_signature_with_commitment(&signature, CommitmentConfig::processed())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(status) = rpc.get_signature_status(&signature).unwrap() {
            if let Err(err) = status {
                let mut confirmed = None;
                while Instant::now() < deadline {
                    if let Ok(transaction) =
                        rpc.get_transaction(&signature, UiTransactionEncoding::Json)
                    {
                        confirmed = Some(transaction);
                        break;
                    }
                    sleep(Duration::from_millis(100));
                }
                panic!(
                    "transaction {signature} failed: {err:?}; meta={:?}",
                    confirmed.and_then(|transaction| transaction.transaction.meta)
                );
            }
            return;
        }
        sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for transaction status {signature}");
}

fn submit_tx(
    rpc: &RpcClient,
    instructions: &[Instruction],
    payer: &Pubkey,
    signers: &[&Keypair],
    config: RpcSendTransactionConfig,
) -> solana_signature::Signature {
    let blockhash = rpc.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(instructions, Some(payer), signers, blockhash);
    if !config.skip_preflight {
        let simulation = rpc.simulate_transaction(&tx).unwrap().value;
        assert_eq!(
            simulation.err, None,
            "simulation failed: {:?}",
            simulation.logs
        );
    }
    rpc.send_transaction_with_config(&tx, config).unwrap()
}

fn wait_for_health(rpc: &RpcClient) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if rpc.get_health().is_ok() {
            return;
        }
        sleep(Duration::from_millis(500));
    }
    panic!("RPC did not become healthy");
}

fn portal_pubkey((pubkey, _bump): (Pubkey, u8)) -> Pubkey {
    pubkey
}

struct TestPorts {
    rpc: u16,
    faucet: u16,
    er_rpc: u16,
    er_ws: u16,
    er_tpu: u16,
}

impl TestPorts {
    fn new() -> Self {
        Self {
            rpc: available_port(),
            faucet: available_port(),
            er_rpc: available_port(),
            er_ws: available_port(),
            er_tpu: available_port(),
        }
    }

    fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.rpc)
    }

    fn er_rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.er_rpc)
    }
}

struct ValidatorProcess {
    child: Child,
}

impl ValidatorProcess {
    fn start(mint: &Pubkey, ledger: &PathBuf, ports: &TestPorts) -> Self {
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
            .to_path_buf();
        let child = Command::new("cargo")
            .current_dir(workspace_root)
            .args([
                "run",
                "--bin",
                "solana-test-validator",
                "--",
                "--reset",
                "--ledger",
            ])
            .arg(ledger)
            .args([
                "--mint",
                &mint.to_string(),
                "--rpc-port",
                &ports.rpc.to_string(),
                "--faucet-port",
                &ports.faucet.to_string(),
                "--portal",
                &PORTAL_PROGRAM_ID.to_string(),
                "--ephemeral-rpc-port",
                &ports.er_rpc.to_string(),
                "--ephemeral-ws-port",
                &ports.er_ws.to_string(),
                "--ephemeral-tpu-port",
                &ports.er_tpu.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start solana-test-validator");
        Self { child }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ValidatorProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

fn available_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn unique_ledger_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "northstar-spl-token-bridge-e2e-{}-{nanos}",
        std::process::id()
    ))
}
