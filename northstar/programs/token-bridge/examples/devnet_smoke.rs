use {
    borsh::BorshDeserialize,
    northstar_portal::{PortalInstruction, RegisterSessionBridge, Session},
    northstar_token_bridge::{
        find_buffer_pda, find_er_token_account_pda, find_token_vault_pda,
        instruction::TokenBridgeInstruction, state::ErTokenAccount,
    },
    solana_commitment_config::CommitmentConfig,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::{read_keypair_file, write_keypair_file, Keypair},
    solana_program_pack::Pack,
    solana_pubkey::Pubkey,
    solana_rpc_client::rpc_client::RpcClient,
    solana_rpc_client_api::config::RpcSendTransactionConfig,
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_system_interface::instruction as system_instruction,
    solana_transaction::Transaction,
    spl_token_interface::{
        instruction as token_instruction,
        state::{Account as SplTokenAccount, Mint},
    },
    std::{
        collections::HashMap,
        env, fs,
        path::Path,
        str::FromStr,
        thread::sleep,
        time::{Duration, Instant},
    },
};

const DECIMALS: u8 = 6;
const DEPOSIT_AMOUNT: u64 = 1_000_000;
const WITHDRAW_AMOUNT: u64 = 1_000_000;
const WAIT_TIMEOUT: Duration = Duration::from_secs(900);

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let phase = env::args().nth(1).ok_or("expected prepare or withdraw")?;
    let config = Config::from_env()?;
    match phase.as_str() {
        "prepare" => prepare(&config),
        "withdraw" => withdraw(&config),
        _ => Err(format!("unknown phase: {phase}").into()),
    }
}

struct Config {
    l1_rpc: String,
    er_rpc: String,
    portal_program: Pubkey,
    bridge_program: Pubkey,
    deployer_keypair: String,
    fee_payer_keypair: String,
    state_path: String,
    report_path: String,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Self {
            l1_rpc: required_env("DEVNET_RPC")?,
            er_rpc: required_env("ER_RPC")?,
            portal_program: Pubkey::from_str(&required_env("PORTAL_ADDRESS")?)?,
            bridge_program: Pubkey::from_str(&required_env("TOKEN_BRIDGE_ADDRESS")?)?,
            deployer_keypair: required_env("DEPLOYER_KEYPAIR")?,
            fee_payer_keypair: required_env("ER_FEE_PAYER_KEYPAIR")?,
            state_path: required_env("STATE_PATH")?,
            report_path: required_env("REPORT_PATH")?,
        })
    }
}

fn prepare(config: &Config) -> Result<()> {
    let rpc = RpcClient::new_with_commitment(config.l1_rpc.clone(), CommitmentConfig::confirmed());
    let er_rpc =
        RpcClient::new_with_commitment(config.er_rpc.clone(), CommitmentConfig::processed());
    let payer = read_keypair(&config.deployer_keypair)?;
    let session = portal_pubkey(northstar_portal::find_session_pda(
        &config.portal_program.to_bytes(),
    ));
    let session_account = rpc.get_account(&session)?;
    let session_state = Session::try_from_slice(&session_account.data)?;
    let mint = Keypair::new();
    let source_token = Keypair::new();
    let destination_token = Keypair::new();
    let vault_token = Keypair::new();
    let er_fee_payer = Keypair::new();
    let session_bridge = portal_pubkey(northstar_portal::find_session_bridge_pda(
        &config.portal_program.to_bytes(),
        &session.to_bytes(),
        &mint.pubkey().to_bytes(),
    ));
    let (vault, _) = find_token_vault_pda(&config.bridge_program, &session_bridge);
    let (er_token_account, _) =
        find_er_token_account_pda(&config.bridge_program, &session_bridge, &payer.pubkey());

    let setup_signature = create_l1_mint_and_token_accounts(
        &rpc,
        &payer,
        &mint,
        &source_token,
        &destination_token,
        &vault_token,
        vault,
    )?;
    send_tx(
        &rpc,
        &[register_session_bridge_ix(
            config.portal_program,
            config.bridge_program,
            payer.pubkey(),
            session,
            session_bridge,
            mint.pubkey(),
            vault,
        )],
        &payer.pubkey(),
        &[&payer],
        false,
    )?;
    send_tx(
        &rpc,
        &[
            initialize_vault_ix(
                config.portal_program,
                config.bridge_program,
                payer.pubkey(),
                session_bridge,
                vault,
                vault_token.pubkey(),
            ),
            initialize_er_ix(
                config.portal_program,
                config.bridge_program,
                payer.pubkey(),
                session_bridge,
                payer.pubkey(),
                er_token_account,
            ),
        ],
        &payer.pubkey(),
        &[&payer],
        false,
    )?;
    fund_and_delegate_er_fee_payer(
        &rpc,
        &payer,
        &er_fee_payer,
        config.portal_program,
        session,
        session_state.grid_id,
    )?;
    send_tx(
        &rpc,
        &[delegate_er_ix(
            config.portal_program,
            config.bridge_program,
            payer.pubkey(),
            session,
            session_bridge,
            er_token_account,
            session_state.grid_id,
        )],
        &payer.pubkey(),
        &[&payer],
        false,
    )?;
    wait_for_er_amount(&er_rpc, er_token_account, 0)?;

    let deposit_signature = send_tx(
        &rpc,
        &[deposit_ix(
            config.portal_program,
            config.bridge_program,
            payer.pubkey(),
            vault,
            er_token_account,
            session_bridge,
            source_token.pubkey(),
            vault_token.pubkey(),
            mint.pubkey(),
            DEPOSIT_AMOUNT,
        )],
        &payer.pubkey(),
        &[&payer],
        false,
    )?;
    wait_token_amount(&rpc, source_token.pubkey(), 9_000_000)?;
    wait_token_amount(&rpc, vault_token.pubkey(), DEPOSIT_AMOUNT)?;
    wait_for_er_amount(&er_rpc, er_token_account, DEPOSIT_AMOUNT)?;
    write_keypair_file(&er_fee_payer, &config.fee_payer_keypair)
        .map_err(|err| format!("write ER fee payer keypair: {err}"))?;
    fs::write(
        &config.state_path,
        format!(
            "SESSION={session}\nMINT={}\nSESSION_BRIDGE={session_bridge}\nVAULT={vault}\\
             nVAULT_TOKEN={}\nSOURCE_TOKEN={}\nDESTINATION_TOKEN={}\\
             nER_TOKEN_ACCOUNT={er_token_account}\nSETUP_SIGNATURE={setup_signature}\\
             nDEPOSIT_SIGNATURE={deposit_signature}\n",
            mint.pubkey(),
            vault_token.pubkey(),
            source_token.pubkey(),
            destination_token.pubkey(),
        ),
    )?;
    println!("SPL deposit confirmed: {deposit_signature}");
    Ok(())
}

fn withdraw(config: &Config) -> Result<()> {
    let rpc = RpcClient::new_with_commitment(config.l1_rpc.clone(), CommitmentConfig::confirmed());
    let er_rpc =
        RpcClient::new_with_commitment(config.er_rpc.clone(), CommitmentConfig::processed());
    let payer = read_keypair(&config.deployer_keypair)?;
    let er_fee_payer = read_keypair(&config.fee_payer_keypair)?;
    let state = read_state(&config.state_path)?;
    let session_bridge = state_pubkey(&state, "SESSION_BRIDGE")?;
    let er_token_account = state_pubkey(&state, "ER_TOKEN_ACCOUNT")?;
    let destination_token = state_pubkey(&state, "DESTINATION_TOKEN")?;
    let vault_token = state_pubkey(&state, "VAULT_TOKEN")?;

    wait_for_er_amount(&er_rpc, er_token_account, DEPOSIT_AMOUNT)?;
    let withdrawal_signature = send_tx(
        &er_rpc,
        &[start_withdrawal_ix(
            config.portal_program,
            config.bridge_program,
            payer.pubkey(),
            er_token_account,
            session_bridge,
            destination_token,
            WITHDRAW_AMOUNT,
        )],
        &er_fee_payer.pubkey(),
        &[&er_fee_payer, &payer],
        true,
    )?;
    wait_for_er_amount(&er_rpc, er_token_account, 0)?;
    wait_token_amount(&rpc, destination_token, WITHDRAW_AMOUNT)?;
    wait_token_amount(&rpc, vault_token, 0)?;

    let setup_signature = state
        .get("SETUP_SIGNATURE")
        .ok_or("missing setup signature")?;
    let settlement_signature = rpc
        .get_signatures_for_address(&destination_token)?
        .into_iter()
        .map(|status| status.signature)
        .find(|signature| signature != setup_signature)
        .ok_or("could not find SPL settlement signature")?;
    let deposit_signature = state
        .get("DEPOSIT_SIGNATURE")
        .ok_or("missing deposit signature")?;
    fs::write(
        &config.report_path,
        format!(
            ":test_tube: Devnet SPL smoke status: *success*\n:coin: Mint: https://explorer.solana.com/address/{}?cluster=devnet\n:inbox_tray: L1 SPL deposit: https://explorer.solana.com/tx/{deposit_signature}?cluster=devnet\n:outbox_tray: ER SPL withdrawal: https://explorer.solana.com/tx/{withdrawal_signature}?cluster=custom&customUrl=https%3A%2F%2Fephemeral.devnet.sonic.game\n:classical_building: L1 SPL settlement: https://explorer.solana.com/tx/{settlement_signature}?cluster=devnet\n",
            state.get("MINT").ok_or("missing mint")?,
        ),
    )?;
    println!("SPL withdrawal confirmed: {withdrawal_signature}");
    println!("SPL settlement confirmed: {settlement_signature}");
    Ok(())
}

fn register_session_bridge_ix(
    portal_program: Pubkey,
    bridge_program: Pubkey,
    authority: Pubkey,
    session: Pubkey,
    session_bridge: Pubkey,
    mint: Pubkey,
    vault: Pubkey,
) -> Instruction {
    Instruction {
        program_id: portal_program,
        accounts: vec![
            AccountMeta::new(authority, true),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(session_bridge, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&PortalInstruction::RegisterSessionBridge(
            RegisterSessionBridge {
                mint: mint.to_bytes(),
                bridge_program: bridge_program.to_bytes(),
                vault: vault.to_bytes(),
                token_program: spl_token_interface::id().to_bytes(),
            },
        ))
        .unwrap(),
    }
}

fn initialize_vault_ix(
    portal_program: Pubkey,
    bridge_program: Pubkey,
    payer: Pubkey,
    session_bridge: Pubkey,
    vault: Pubkey,
    vault_token: Pubkey,
) -> Instruction {
    Instruction {
        program_id: bridge_program,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(portal_program, false),
            AccountMeta::new_readonly(vault_token, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::InitializeVault).unwrap(),
    }
}

fn initialize_er_ix(
    portal_program: Pubkey,
    bridge_program: Pubkey,
    payer: Pubkey,
    session_bridge: Pubkey,
    owner: Pubkey,
    er_account: Pubkey,
) -> Instruction {
    Instruction {
        program_id: bridge_program,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(portal_program, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::InitializeErTokenAccount {
            owner: owner.to_bytes(),
        })
        .unwrap(),
    }
}

#[allow(clippy::too_many_arguments)]
fn deposit_ix(
    portal_program: Pubkey,
    bridge_program: Pubkey,
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
        &bridge_program,
        &session_bridge,
        &er_account,
    );
    let delegation_record = portal_pubkey(northstar_portal::find_delegation_record_pda(
        &portal_program.to_bytes(),
        &er_account.to_bytes(),
    ));
    Instruction {
        program_id: bridge_program,
        accounts: vec![
            AccountMeta::new(owner, true),
            AccountMeta::new(vault, false),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(portal_program, false),
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

fn start_withdrawal_ix(
    portal_program: Pubkey,
    bridge_program: Pubkey,
    owner: Pubkey,
    er_account: Pubkey,
    session_bridge: Pubkey,
    destination_token: Pubkey,
    amount: u64,
) -> Instruction {
    Instruction {
        program_id: bridge_program,
        accounts: vec![
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(portal_program, false),
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
    portal_program: Pubkey,
    bridge_program: Pubkey,
    payer: Pubkey,
    session: Pubkey,
    session_bridge: Pubkey,
    er_account: Pubkey,
    grid_id: u64,
) -> Instruction {
    let delegation_record = portal_pubkey(northstar_portal::find_delegation_record_pda(
        &portal_program.to_bytes(),
        &er_account.to_bytes(),
    ));
    let (buffer, _) = find_buffer_pda(&bridge_program, &er_account);
    Instruction {
        program_id: bridge_program,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(er_account, false),
            AccountMeta::new_readonly(bridge_program, false),
            AccountMeta::new_readonly(session_bridge, false),
            AccountMeta::new_readonly(portal_program, false),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(delegation_record, false),
            AccountMeta::new(buffer, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::DelegateErTokenAccount { grid_id }).unwrap(),
    }
}

fn fund_and_delegate_er_fee_payer(
    rpc: &RpcClient,
    payer: &Keypair,
    er_fee_payer: &Keypair,
    portal_program: Pubkey,
    session: Pubkey,
    grid_id: u64,
) -> Result<()> {
    send_tx(
        rpc,
        &[system_instruction::transfer(
            &payer.pubkey(),
            &er_fee_payer.pubkey(),
            100_000_000,
        )],
        &payer.pubkey(),
        &[payer],
        false,
    )?;
    send_tx(
        rpc,
        &[system_instruction::assign(
            &er_fee_payer.pubkey(),
            &portal_program,
        )],
        &payer.pubkey(),
        &[payer, er_fee_payer],
        false,
    )?;
    let delegation_record = portal_pubkey(northstar_portal::find_delegation_record_pda(
        &portal_program.to_bytes(),
        &er_fee_payer.pubkey().to_bytes(),
    ));
    send_tx(
        rpc,
        &[Instruction {
            program_id: portal_program,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new(er_fee_payer.pubkey(), true),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new(delegation_record, false),
                AccountMeta::new_readonly(payer.pubkey(), false),
            ],
            data: borsh::to_vec(&PortalInstruction::Delegate { grid_id }).unwrap(),
        }],
        &payer.pubkey(),
        &[payer, er_fee_payer],
        false,
    )?;
    Ok(())
}

fn create_l1_mint_and_token_accounts(
    rpc: &RpcClient,
    payer: &Keypair,
    mint: &Keypair,
    source_token: &Keypair,
    destination_token: &Keypair,
    vault_token: &Keypair,
    vault: Pubkey,
) -> Result<solana_signature::Signature> {
    let mint_rent = rpc.get_minimum_balance_for_rent_exemption(Mint::LEN)?;
    let token_rent = rpc.get_minimum_balance_for_rent_exemption(SplTokenAccount::LEN)?;
    send_tx(
        rpc,
        &[
            create_account_ix(payer, mint, mint_rent, Mint::LEN),
            token_instruction::initialize_mint(
                &spl_token_interface::id(),
                &mint.pubkey(),
                &payer.pubkey(),
                None,
                DECIMALS,
            )?,
            create_account_ix(payer, source_token, token_rent, SplTokenAccount::LEN),
            token_instruction::initialize_account(
                &spl_token_interface::id(),
                &source_token.pubkey(),
                &mint.pubkey(),
                &payer.pubkey(),
            )?,
            create_account_ix(payer, destination_token, token_rent, SplTokenAccount::LEN),
            token_instruction::initialize_account(
                &spl_token_interface::id(),
                &destination_token.pubkey(),
                &mint.pubkey(),
                &payer.pubkey(),
            )?,
            create_account_ix(payer, vault_token, token_rent, SplTokenAccount::LEN),
            token_instruction::initialize_account(
                &spl_token_interface::id(),
                &vault_token.pubkey(),
                &mint.pubkey(),
                &vault,
            )?,
            token_instruction::mint_to(
                &spl_token_interface::id(),
                &mint.pubkey(),
                &source_token.pubkey(),
                &payer.pubkey(),
                &[],
                10_000_000,
            )?,
        ],
        &payer.pubkey(),
        &[payer, mint, source_token, destination_token, vault_token],
        false,
    )
}

fn create_account_ix(payer: &Keypair, account: &Keypair, lamports: u64, len: usize) -> Instruction {
    system_instruction::create_account(
        &payer.pubkey(),
        &account.pubkey(),
        lamports,
        len as u64,
        &spl_token_interface::id(),
    )
}

fn send_tx(
    rpc: &RpcClient,
    instructions: &[Instruction],
    payer: &Pubkey,
    signers: &[&Keypair],
    skip_preflight: bool,
) -> Result<solana_signature::Signature> {
    let blockhash = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(instructions, Some(payer), signers, blockhash);
    let signature = rpc.send_transaction_with_config(
        &tx,
        RpcSendTransactionConfig {
            skip_preflight,
            max_retries: Some(20),
            ..RpcSendTransactionConfig::default()
        },
    )?;
    rpc.poll_for_signature_with_commitment(&signature, CommitmentConfig::processed())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(status) = rpc.get_signature_status(&signature)? {
            return match status {
                Ok(()) => Ok(signature),
                Err(err) => Err(format!("transaction {signature} failed: {err}").into()),
            };
        }
        sleep(Duration::from_millis(100));
    }
    Err(format!("timed out waiting for transaction status {signature}").into())
}

fn wait_token_amount(rpc: &RpcClient, token_account: Pubkey, expected: u64) -> Result<()> {
    wait_until(
        || {
            rpc.get_account(&token_account)
                .ok()
                .and_then(|account| SplTokenAccount::unpack(&account.data).ok())
                .is_some_and(|state| state.amount == expected)
        },
        format!("token account {token_account} amount {expected}"),
    )
}

fn wait_for_er_amount(rpc: &RpcClient, er_account: Pubkey, expected: u64) -> Result<()> {
    wait_until(
        || {
            rpc.get_account(&er_account)
                .ok()
                .and_then(|account| ErTokenAccount::try_from_slice(&account.data).ok())
                .is_some_and(|state| state.amount == expected)
        },
        format!("ER token account {er_account} amount {expected}"),
    )
}

fn wait_until(mut condition: impl FnMut() -> bool, description: String) -> Result<()> {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if condition() {
            return Ok(());
        }
        sleep(Duration::from_secs(2));
    }
    Err(format!("timed out waiting for {description}").into())
}

fn read_keypair(path: &str) -> Result<Keypair> {
    read_keypair_file(path).map_err(|err| format!("read keypair {path}: {err}").into())
}

fn read_state(path: &str) -> Result<HashMap<String, String>> {
    Ok(fs::read_to_string(path)?
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect())
}

fn state_pubkey(state: &HashMap<String, String>, key: &str) -> Result<Pubkey> {
    Ok(Pubkey::from_str(
        state.get(key).ok_or_else(|| format!("missing {key}"))?,
    )?)
}

fn required_env(name: &str) -> Result<String> {
    env::var(name).map_err(|_| format!("missing environment variable {name}").into())
}

fn portal_pubkey((pubkey, _bump): ([u8; 32], u8)) -> Pubkey {
    Pubkey::new_from_array(pubkey)
}

#[allow(dead_code)]
fn path_exists(path: &str) -> bool {
    Path::new(path).exists()
}
