use {
    crate::{
        cli::{CliCommand, CliCommandInfo, CliConfig, CliError, ProcessResult},
        compute_budget::{ComputeUnitConfig, WithComputeUnitConfig},
        nonce::check_nonce_account,
    },
    clap::{App, AppSettings, Arg, ArgMatches, SubCommand},
    northstar_portal::{DelegationRecord, OpenSession, PortalInstruction},
    solana_account::Account,
    solana_clap_utils::{
        compute_budget::{COMPUTE_UNIT_PRICE_ARG, ComputeUnitLimit, compute_unit_price_arg},
        fee_payer::{FEE_PAYER_ARG, fee_payer_arg},
        input_parsers::{lamports_of_sol, pubkey_of, pubkey_of_signer, signer_of, value_of},
        input_validators::{is_amount, is_parsable, is_valid_pubkey},
        keypair::{DefaultSigner, SignerIndex},
        nonce::{NONCE_ARG, NONCE_AUTHORITY_ARG, NonceArgs},
        offline::{DUMP_TRANSACTION_MESSAGE, OfflineArgs, SIGN_ONLY_ARG},
    },
    solana_cli_output::{CliSignature, ReturnSignersConfig, return_signers_with_config},
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::Message,
    solana_pubkey::Pubkey,
    solana_remote_wallet::remote_wallet::RemoteWalletManager,
    solana_rpc_client::nonblocking::rpc_client::RpcClient,
    solana_rpc_client_nonce_utils::nonblocking::blockhash_query::BlockhashQuery,
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_system_interface::instruction as system_instruction,
    solana_transaction::Transaction,
    std::{rc::Rc, str::FromStr},
};

const LOCALNET_DEFAULT_PORTAL_PROGRAM_ID: &str = "5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf";
const DEVNET_DEFAULT_PORTAL_PROGRAM_ID: &str = "74iiMCqFw1afWyp3tdh9pUqfRfCRq7gfdC2YZoNGpovt";
const DEFAULT_GRID_ID: &str = "0";
const DEFAULT_SESSION_TTL_SLOTS: &str = "78840000";
const DEFAULT_FEE_CAP_SOL: &str = "1000000";

#[derive(Debug, PartialEq)]
pub enum PortalCliCommand {
    OpenSession {
        portal_program_id: Option<Pubkey>,
        owner: SignerIndex,
        grid_id: u64,
        ttl_slots: u64,
        fee_cap: u64,
        sign_only: bool,
        dump_transaction_message: bool,
        blockhash_query: BlockhashQuery,
        nonce_account: Option<Pubkey>,
        nonce_authority: SignerIndex,
        fee_payer: SignerIndex,
        compute_unit_price: Option<u64>,
    },
    CloseSession {
        portal_program_id: Option<Pubkey>,
        owner: SignerIndex,
        sign_only: bool,
        dump_transaction_message: bool,
        blockhash_query: BlockhashQuery,
        nonce_account: Option<Pubkey>,
        nonce_authority: SignerIndex,
        fee_payer: SignerIndex,
        compute_unit_price: Option<u64>,
    },
    DepositFee {
        portal_program_id: Option<Pubkey>,
        depositor: SignerIndex,
        recipient: Pubkey,
        lamports: u64,
        sign_only: bool,
        dump_transaction_message: bool,
        blockhash_query: BlockhashQuery,
        nonce_account: Option<Pubkey>,
        nonce_authority: SignerIndex,
        fee_payer: SignerIndex,
        compute_unit_price: Option<u64>,
    },
    Delegate {
        portal_program_id: Option<Pubkey>,
        authority: SignerIndex,
        delegated_account: SignerIndex,
        owner_program: Option<Pubkey>,
        grid_id: u64,
        sign_only: bool,
        dump_transaction_message: bool,
        blockhash_query: BlockhashQuery,
        nonce_account: Option<Pubkey>,
        nonce_authority: SignerIndex,
        fee_payer: SignerIndex,
        compute_unit_price: Option<u64>,
    },
    Undelegate {
        portal_program_id: Option<Pubkey>,
        authority: SignerIndex,
        delegated_account: Pubkey,
        owner_program: Option<Pubkey>,
        sign_only: bool,
        dump_transaction_message: bool,
        blockhash_query: BlockhashQuery,
        nonce_account: Option<Pubkey>,
        nonce_authority: SignerIndex,
        fee_payer: SignerIndex,
        compute_unit_price: Option<u64>,
    },
}

pub trait PortalSubCommands {
    fn portal_subcommands(self) -> Self;
}

impl PortalSubCommands for App<'_, '_> {
    fn portal_subcommands(self) -> Self {
        self.subcommand(
            SubCommand::with_name("portal")
                .alias("p")
                .about("Northstar Portal interaction")
                .setting(AppSettings::SubcommandRequiredElseHelp)
                .subcommand(portal_tx_subcommand(
                    SubCommand::with_name("open-session")
                        .alias("os")
                        .about("Open Portal session")
                        .arg(portal_program_id_arg())
                        .arg(grid_id_arg())
                        .arg(ttl_slots_arg())
                        .arg(fee_cap_arg()),
                ))
                .subcommand(portal_tx_subcommand(
                    SubCommand::with_name("close-session")
                        .alias("cs")
                        .about("Close Portal session")
                        .arg(portal_program_id_arg()),
                ))
                .subcommand(portal_tx_subcommand(
                    SubCommand::with_name("deposit-fee")
                        .alias("df")
                        .about("Deposit SOL into Portal session")
                        .arg(portal_program_id_arg())
                        .arg(lamports_arg().index(1))
                        .arg(
                            Arg::with_name("recipient")
                                .long("recipient")
                                .value_name("RECIPIENT")
                                .takes_value(true)
                                .help("Deposit receipt recipient pubkey [default: depositor]"),
                        ),
                ))
                .subcommand(portal_tx_subcommand(
                    SubCommand::with_name("delegate")
                        .alias("del")
                        .about("Create Portal delegation record for portal-owned account")
                        .arg(portal_program_id_arg())
                        .arg(grid_id_arg())
                        .arg(delegated_account_arg().index(1))
                        .arg(owner_program_arg().index(2)),
                ))
                .subcommand(portal_tx_subcommand(
                    SubCommand::with_name("undelegate")
                        .alias("undel")
                        .about("Undelegate portal-owned account back to original program")
                        .arg(portal_program_id_arg())
                        .arg(delegated_account_arg().index(1))
                        .arg(owner_program_arg().index(2)),
                )),
        )
    }
}

fn portal_tx_subcommand<'a, 'b>(app: App<'a, 'b>) -> App<'a, 'b> {
    app.arg(fee_payer_arg())
        .arg(compute_unit_price_arg())
        .offline_args()
        .nonce_args(false)
}

fn portal_program_id_arg<'a, 'b>() -> Arg<'a, 'b> {
    Arg::with_name("portal_program_id")
        .long("portal")
        .short("p")
        .value_name("PORTAL_PROGRAM_ID")
        .takes_value(true)
        .validator(is_valid_pubkey)
        .help("Northstar Portal program id [default: local/devnet]")
}

fn grid_id_arg<'a, 'b>() -> Arg<'a, 'b> {
    Arg::with_name("grid_id")
        .long("grid")
        .value_name("GRID_ID")
        .takes_value(true)
        .default_value(DEFAULT_GRID_ID)
        .validator(is_parsable::<u64>)
        .help("Portal grid id [default: 0]")
}

fn ttl_slots_arg<'a, 'b>() -> Arg<'a, 'b> {
    Arg::with_name("ttl_slots")
        .long("ttl")
        .value_name("TTL_SLOTS")
        .takes_value(true)
        .default_value(DEFAULT_SESSION_TTL_SLOTS)
        .validator(is_parsable::<u64>)
        .help("Session TTL in slots [default: ~1 year]")
}

fn fee_cap_arg<'a, 'b>() -> Arg<'a, 'b> {
    Arg::with_name("fee_cap")
        .long("fee-cap")
        .value_name("FEE_CAP_SOL")
        .takes_value(true)
        .default_value(DEFAULT_FEE_CAP_SOL)
        .validator(is_amount)
        .help("Session fee cap in SOL [default: big]")
}

fn lamports_arg<'a, 'b>() -> Arg<'a, 'b> {
    Arg::with_name("lamports")
        .value_name("AMOUNT_SOL")
        .takes_value(true)
        .required(true)
        .validator(is_amount)
        .help("Amount in SOL")
}

fn delegated_account_arg<'a, 'b>() -> Arg<'a, 'b> {
    Arg::with_name("delegated_account")
        .value_name("DELEGATED_ACCOUNT")
        .takes_value(true)
        .required(true)
        .help("Account being delegated or undelegated")
}

fn owner_program_arg<'a, 'b>() -> Arg<'a, 'b> {
    Arg::with_name("owner_program")
        .value_name("OWNER_PROGRAM")
        .takes_value(true)
        .required(false)
        .help(
            "Original owner program id for delegated account [default: account owner or system \
             program]",
        )
}

pub fn parse_portal_subcommand(
    matches: &ArgMatches<'_>,
    default_signer: &DefaultSigner,
    wallet_manager: &mut Option<Rc<RemoteWalletManager>>,
) -> Result<CliCommandInfo, CliError> {
    match matches.subcommand() {
        ("open-session", Some(matches)) => {
            parse_open_session(matches, default_signer, wallet_manager)
        }
        ("close-session", Some(matches)) => {
            parse_close_session(matches, default_signer, wallet_manager)
        }
        ("deposit-fee", Some(matches)) => {
            parse_deposit_fee(matches, default_signer, wallet_manager)
        }
        ("delegate", Some(matches)) => parse_delegate(matches, default_signer, wallet_manager),
        ("undelegate", Some(matches)) => parse_undelegate(matches, default_signer, wallet_manager),
        _ => unreachable!(),
    }
}

fn parse_open_session(
    matches: &ArgMatches<'_>,
    default_signer: &DefaultSigner,
    wallet_manager: &mut Option<Rc<RemoteWalletManager>>,
) -> Result<CliCommandInfo, CliError> {
    let portal_program_id = pubkey_of(matches, "portal_program_id");
    let owner = default_signer.signer_from_path(matches, wallet_manager)?;
    let owner_pubkey = owner.pubkey();
    let fee_cap = lamports_of_sol(matches, "fee_cap")
        .ok_or_else(|| CliError::BadParameter("Invalid fee cap amount".to_string()))?;
    let sign_only = matches.is_present(SIGN_ONLY_ARG.name);
    let dump_transaction_message = matches.is_present(DUMP_TRANSACTION_MESSAGE.name);
    let blockhash_query = BlockhashQuery::new_from_matches(matches);
    let nonce_account = pubkey_of_signer(matches, NONCE_ARG.name, wallet_manager)?;
    let (nonce_authority, nonce_authority_pubkey) =
        signer_of(matches, NONCE_AUTHORITY_ARG.name, wallet_manager)?;
    let (fee_payer, fee_payer_pubkey) = signer_of(matches, FEE_PAYER_ARG.name, wallet_manager)?;
    let compute_unit_price = value_of(matches, COMPUTE_UNIT_PRICE_ARG.name);

    let mut bulk_signers = vec![Some(owner), fee_payer];
    if nonce_account.is_some() {
        bulk_signers.push(nonce_authority);
    }
    let signer_info =
        default_signer.generate_unique_signers(bulk_signers, matches, wallet_manager)?;

    Ok(CliCommandInfo {
        command: CliCommand::Portal(PortalCliCommand::OpenSession {
            portal_program_id,
            owner: signer_info.index_of(Some(owner_pubkey)).unwrap(),
            grid_id: value_of(matches, "grid_id").unwrap(),
            ttl_slots: value_of(matches, "ttl_slots").unwrap(),
            fee_cap,
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority: signer_info.index_of(nonce_authority_pubkey).unwrap(),
            fee_payer: signer_info.index_of(fee_payer_pubkey).unwrap(),
            compute_unit_price,
        }),
        signers: signer_info.signers,
    })
}

fn parse_close_session(
    matches: &ArgMatches<'_>,
    default_signer: &DefaultSigner,
    wallet_manager: &mut Option<Rc<RemoteWalletManager>>,
) -> Result<CliCommandInfo, CliError> {
    let portal_program_id = pubkey_of(matches, "portal_program_id");
    let owner = default_signer.signer_from_path(matches, wallet_manager)?;
    let owner_pubkey = owner.pubkey();
    let sign_only = matches.is_present(SIGN_ONLY_ARG.name);
    let dump_transaction_message = matches.is_present(DUMP_TRANSACTION_MESSAGE.name);
    let blockhash_query = BlockhashQuery::new_from_matches(matches);
    let nonce_account = pubkey_of_signer(matches, NONCE_ARG.name, wallet_manager)?;
    let (nonce_authority, nonce_authority_pubkey) =
        signer_of(matches, NONCE_AUTHORITY_ARG.name, wallet_manager)?;
    let (fee_payer, fee_payer_pubkey) = signer_of(matches, FEE_PAYER_ARG.name, wallet_manager)?;
    let compute_unit_price = value_of(matches, COMPUTE_UNIT_PRICE_ARG.name);

    let mut bulk_signers = vec![Some(owner), fee_payer];
    if nonce_account.is_some() {
        bulk_signers.push(nonce_authority);
    }
    let signer_info =
        default_signer.generate_unique_signers(bulk_signers, matches, wallet_manager)?;

    Ok(CliCommandInfo {
        command: CliCommand::Portal(PortalCliCommand::CloseSession {
            portal_program_id,
            owner: signer_info.index_of(Some(owner_pubkey)).unwrap(),
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority: signer_info.index_of(nonce_authority_pubkey).unwrap(),
            fee_payer: signer_info.index_of(fee_payer_pubkey).unwrap(),
            compute_unit_price,
        }),
        signers: signer_info.signers,
    })
}

fn parse_deposit_fee(
    matches: &ArgMatches<'_>,
    default_signer: &DefaultSigner,
    wallet_manager: &mut Option<Rc<RemoteWalletManager>>,
) -> Result<CliCommandInfo, CliError> {
    let portal_program_id = pubkey_of(matches, "portal_program_id");
    let depositor = default_signer.signer_from_path(matches, wallet_manager)?;
    let depositor_pubkey = depositor.pubkey();
    let recipient =
        pubkey_of_signer(matches, "recipient", wallet_manager)?.unwrap_or(depositor_pubkey);
    let lamports = lamports_of_sol(matches, "lamports")
        .ok_or_else(|| CliError::BadParameter("Invalid amount".to_string()))?;
    let sign_only = matches.is_present(SIGN_ONLY_ARG.name);
    let dump_transaction_message = matches.is_present(DUMP_TRANSACTION_MESSAGE.name);
    let blockhash_query = BlockhashQuery::new_from_matches(matches);
    let nonce_account = pubkey_of_signer(matches, NONCE_ARG.name, wallet_manager)?;
    let (nonce_authority, nonce_authority_pubkey) =
        signer_of(matches, NONCE_AUTHORITY_ARG.name, wallet_manager)?;
    let (fee_payer, fee_payer_pubkey) = signer_of(matches, FEE_PAYER_ARG.name, wallet_manager)?;
    let compute_unit_price = value_of(matches, COMPUTE_UNIT_PRICE_ARG.name);

    let mut bulk_signers = vec![Some(depositor), fee_payer];
    if nonce_account.is_some() {
        bulk_signers.push(nonce_authority);
    }
    let signer_info =
        default_signer.generate_unique_signers(bulk_signers, matches, wallet_manager)?;

    Ok(CliCommandInfo {
        command: CliCommand::Portal(PortalCliCommand::DepositFee {
            portal_program_id,
            depositor: signer_info.index_of(Some(depositor_pubkey)).unwrap(),
            recipient,
            lamports,
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority: signer_info.index_of(nonce_authority_pubkey).unwrap(),
            fee_payer: signer_info.index_of(fee_payer_pubkey).unwrap(),
            compute_unit_price,
        }),
        signers: signer_info.signers,
    })
}

fn parse_delegate(
    matches: &ArgMatches<'_>,
    default_signer: &DefaultSigner,
    wallet_manager: &mut Option<Rc<RemoteWalletManager>>,
) -> Result<CliCommandInfo, CliError> {
    let portal_program_id = pubkey_of(matches, "portal_program_id");
    // Portal::Delegate requires delegated_account as a co-signer. Caller must pass a
    // keypair file (or hardware wallet path), not a bare pubkey.
    let (delegated_account_signer, delegated_account_pubkey) =
        signer_of(matches, "delegated_account", wallet_manager)?;
    let delegated_account_pubkey = delegated_account_pubkey.unwrap();
    let owner_program = pubkey_of_signer(matches, "owner_program", wallet_manager)?;
    let authority = default_signer.signer_from_path(matches, wallet_manager)?;
    let authority_pubkey = authority.pubkey();
    let sign_only = matches.is_present(SIGN_ONLY_ARG.name);
    let dump_transaction_message = matches.is_present(DUMP_TRANSACTION_MESSAGE.name);
    let blockhash_query = BlockhashQuery::new_from_matches(matches);
    let nonce_account = pubkey_of_signer(matches, NONCE_ARG.name, wallet_manager)?;
    let (nonce_authority, nonce_authority_pubkey) =
        signer_of(matches, NONCE_AUTHORITY_ARG.name, wallet_manager)?;
    let (fee_payer, fee_payer_pubkey) = signer_of(matches, FEE_PAYER_ARG.name, wallet_manager)?;
    let compute_unit_price = value_of(matches, COMPUTE_UNIT_PRICE_ARG.name);

    let mut bulk_signers = vec![Some(authority), delegated_account_signer, fee_payer];
    if nonce_account.is_some() {
        bulk_signers.push(nonce_authority);
    }
    let signer_info =
        default_signer.generate_unique_signers(bulk_signers, matches, wallet_manager)?;

    Ok(CliCommandInfo {
        command: CliCommand::Portal(PortalCliCommand::Delegate {
            portal_program_id,
            authority: signer_info.index_of(Some(authority_pubkey)).unwrap(),
            delegated_account: signer_info
                .index_of(Some(delegated_account_pubkey))
                .unwrap(),
            owner_program,
            grid_id: value_of(matches, "grid_id").unwrap(),
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority: signer_info.index_of(nonce_authority_pubkey).unwrap(),
            fee_payer: signer_info.index_of(fee_payer_pubkey).unwrap(),
            compute_unit_price,
        }),
        signers: signer_info.signers,
    })
}

fn parse_undelegate(
    matches: &ArgMatches<'_>,
    default_signer: &DefaultSigner,
    wallet_manager: &mut Option<Rc<RemoteWalletManager>>,
) -> Result<CliCommandInfo, CliError> {
    let portal_program_id = pubkey_of(matches, "portal_program_id");
    let delegated_account =
        pubkey_of_signer(matches, "delegated_account", wallet_manager)?.unwrap();
    let owner_program = pubkey_of_signer(matches, "owner_program", wallet_manager)?;
    let authority = default_signer.signer_from_path(matches, wallet_manager)?;
    let authority_pubkey = authority.pubkey();
    let sign_only = matches.is_present(SIGN_ONLY_ARG.name);
    let dump_transaction_message = matches.is_present(DUMP_TRANSACTION_MESSAGE.name);
    let blockhash_query = BlockhashQuery::new_from_matches(matches);
    let nonce_account = pubkey_of_signer(matches, NONCE_ARG.name, wallet_manager)?;
    let (nonce_authority, nonce_authority_pubkey) =
        signer_of(matches, NONCE_AUTHORITY_ARG.name, wallet_manager)?;
    let (fee_payer, fee_payer_pubkey) = signer_of(matches, FEE_PAYER_ARG.name, wallet_manager)?;
    let compute_unit_price = value_of(matches, COMPUTE_UNIT_PRICE_ARG.name);

    let mut bulk_signers = vec![Some(authority), fee_payer];
    if nonce_account.is_some() {
        bulk_signers.push(nonce_authority);
    }
    let signer_info =
        default_signer.generate_unique_signers(bulk_signers, matches, wallet_manager)?;

    Ok(CliCommandInfo {
        command: CliCommand::Portal(PortalCliCommand::Undelegate {
            portal_program_id,
            authority: signer_info.index_of(Some(authority_pubkey)).unwrap(),
            delegated_account,
            owner_program,
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority: signer_info.index_of(nonce_authority_pubkey).unwrap(),
            fee_payer: signer_info.index_of(fee_payer_pubkey).unwrap(),
            compute_unit_price,
        }),
        signers: signer_info.signers,
    })
}

fn default_portal_program_id_for_rpc_url(json_rpc_url: &str) -> Option<Pubkey> {
    if json_rpc_url.contains("api.devnet.solana.com") {
        Some(Pubkey::from_str(DEVNET_DEFAULT_PORTAL_PROGRAM_ID).unwrap())
    } else if json_rpc_url.contains("localhost:") || json_rpc_url.contains("127.0.0.1:") {
        Some(Pubkey::from_str(LOCALNET_DEFAULT_PORTAL_PROGRAM_ID).unwrap())
    } else {
        None
    }
}

fn resolve_portal_program_id(
    config: &CliConfig<'_>,
    explicit_portal_program_id: Option<Pubkey>,
) -> Result<Pubkey, CliError> {
    explicit_portal_program_id
        .or_else(|| default_portal_program_id_for_rpc_url(&config.json_rpc_url))
        .ok_or_else(|| {
            CliError::BadParameter(format!(
                "Portal program id not set for RPC URL {}. Pass --portal <PUBKEY>",
                config.json_rpc_url
            ))
        })
}

fn find_session_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"session"], program_id).0
}

fn find_fee_vault_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"fee_vault"], program_id).0
}

fn find_delegation_record_pda(program_id: &Pubkey, delegated_account: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"delegation", delegated_account.as_ref()], program_id).0
}

fn find_deposit_receipt_pda(program_id: &Pubkey, session: &Pubkey, recipient: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"deposit_receipt", session.as_ref(), recipient.as_ref()],
        program_id,
    )
    .0
}

fn delegation_record_owner_program(account: &Account) -> Result<Pubkey, CliError> {
    if account.data.len() != DelegationRecord::LEN {
        return Err(CliError::BadParameter(format!(
            "Delegation record has invalid size: expected {}, got {}",
            DelegationRecord::LEN,
            account.data.len()
        )));
    }
    if account.data[0] != DelegationRecord::DISCRIMINATOR {
        return Err(CliError::BadParameter(format!(
            "Delegation record has invalid discriminator: expected {}, got {}",
            DelegationRecord::DISCRIMINATOR,
            account.data[0]
        )));
    }

    let mut owner_program = [0; 32];
    owner_program.copy_from_slice(&account.data[1..33]);
    Ok(Pubkey::from(owner_program))
}

fn default_owner_program_for_delegate(
    account: Option<&Account>,
    portal_program_id: &Pubkey,
) -> Pubkey {
    account
        .filter(|account| account.owner != *portal_program_id)
        .map(|account| account.owner)
        .unwrap_or_else(system_program::id)
}

fn delegated_account_staging_instruction(
    authority: &Pubkey,
    delegated_account: &Pubkey,
    portal_program_id: &Pubkey,
    account: Option<&Account>,
    rent_exempt_lamports: u64,
) -> Result<Option<Instruction>, CliError> {
    match account {
        None => Ok(Some(system_instruction::create_account(
            authority,
            delegated_account,
            rent_exempt_lamports,
            0,
            portal_program_id,
        ))),
        Some(account) if account.owner == *portal_program_id => Ok(None),
        Some(account) if account.owner == system_program::id() && account.data.is_empty() => {
            Ok(Some(system_instruction::assign(
                delegated_account,
                portal_program_id,
            )))
        }
        Some(account) if account.owner == system_program::id() => {
            Err(CliError::BadParameter(format!(
                "Portal delegate can only auto-assign zero-data system accounts; account \
                 {delegated_account} has {} data bytes",
                account.data.len()
            )))
        }
        Some(account) => Err(CliError::BadParameter(format!(
            "Portal delegate requires delegated account {delegated_account} to be portal-owned \
             before delegate; found owner {}",
            account.owner
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_portal_instructions(
    rpc_client: &RpcClient,
    config: &CliConfig<'_>,
    instructions: Vec<Instruction>,
    extra_signers: &[&dyn Signer],
    sign_only: bool,
    dump_transaction_message: bool,
    blockhash_query: &BlockhashQuery,
    nonce_account: Option<&Pubkey>,
    nonce_authority: SignerIndex,
    fee_payer: SignerIndex,
    compute_unit_price: Option<u64>,
) -> ProcessResult {
    let recent_blockhash = blockhash_query
        .get_blockhash(rpc_client, config.commitment)
        .await?;

    let nonce_authority = config.signers[nonce_authority];
    let fee_payer = config.signers[fee_payer];

    let instructions = instructions.with_compute_unit_config(&ComputeUnitConfig {
        compute_unit_price,
        compute_unit_limit: ComputeUnitLimit::Default,
    });

    let message = if let Some(nonce_account) = nonce_account {
        Message::new_with_nonce(
            instructions,
            Some(&fee_payer.pubkey()),
            nonce_account,
            &nonce_authority.pubkey(),
        )
    } else {
        Message::new(&instructions, Some(&fee_payer.pubkey()))
    };

    let mut tx = Transaction::new_unsigned(message);

    let mut all_signers: Vec<&dyn Signer> = config.signers.to_vec();
    all_signers.extend_from_slice(extra_signers);

    if sign_only {
        tx.try_partial_sign(&all_signers, recent_blockhash)?;
        return return_signers_with_config(
            &tx,
            &config.output_format,
            &ReturnSignersConfig {
                dump_transaction_message,
            },
        );
    }

    if let Some(nonce_account) = nonce_account {
        let nonce_account =
            solana_rpc_client_nonce_utils::nonblocking::get_account_with_commitment(
                rpc_client,
                nonce_account,
                config.commitment,
            )
            .await?;
        check_nonce_account(&nonce_account, &nonce_authority.pubkey(), &recent_blockhash)?;
    }

    tx.try_sign(&all_signers, recent_blockhash)?;
    let signature = rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            config.commitment,
            config.send_transaction_config,
        )
        .await?;

    Ok(config.output_format.formatted_string(&CliSignature {
        signature: signature.to_string(),
    }))
}

#[allow(clippy::too_many_arguments)]
async fn process_portal_instruction(
    rpc_client: &RpcClient,
    config: &CliConfig<'_>,
    instruction: Instruction,
    sign_only: bool,
    dump_transaction_message: bool,
    blockhash_query: &BlockhashQuery,
    nonce_account: Option<&Pubkey>,
    nonce_authority: SignerIndex,
    fee_payer: SignerIndex,
    compute_unit_price: Option<u64>,
) -> ProcessResult {
    process_portal_instructions(
        rpc_client,
        config,
        vec![instruction],
        &[],
        sign_only,
        dump_transaction_message,
        blockhash_query,
        nonce_account,
        nonce_authority,
        fee_payer,
        compute_unit_price,
    )
    .await
}

pub async fn process_portal_subcommand(
    rpc_client: &RpcClient,
    config: &CliConfig<'_>,
    portal_subcommand: &PortalCliCommand,
) -> ProcessResult {
    match portal_subcommand {
        PortalCliCommand::OpenSession {
            portal_program_id,
            owner,
            grid_id,
            ttl_slots,
            fee_cap,
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority,
            fee_payer,
            compute_unit_price,
        } => {
            let portal_program_id = resolve_portal_program_id(config, *portal_program_id)?;
            let owner = config.signers[*owner];
            let session_pda = find_session_pda(&portal_program_id);
            let fee_vault_pda = find_fee_vault_pda(&portal_program_id);
            let instruction = Instruction {
                program_id: portal_program_id,
                accounts: vec![
                    AccountMeta::new(owner.pubkey(), true),
                    AccountMeta::new(session_pda, false),
                    AccountMeta::new(fee_vault_pda, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                ],
                data: borsh::to_vec(&PortalInstruction::OpenSession(OpenSession {
                    grid_id: *grid_id,
                    ttl_slots: *ttl_slots,
                    fee_cap: *fee_cap,
                }))
                .unwrap(),
            };
            process_portal_instruction(
                rpc_client,
                config,
                instruction,
                *sign_only,
                *dump_transaction_message,
                blockhash_query,
                nonce_account.as_ref(),
                *nonce_authority,
                *fee_payer,
                *compute_unit_price,
            )
            .await
        }
        PortalCliCommand::CloseSession {
            portal_program_id,
            owner,
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority,
            fee_payer,
            compute_unit_price,
        } => {
            let portal_program_id = resolve_portal_program_id(config, *portal_program_id)?;
            let owner = config.signers[*owner];
            let session_pda = find_session_pda(&portal_program_id);
            let fee_vault_pda = find_fee_vault_pda(&portal_program_id);
            let instruction = Instruction {
                program_id: portal_program_id,
                accounts: vec![
                    AccountMeta::new(owner.pubkey(), true),
                    AccountMeta::new(session_pda, false),
                    AccountMeta::new(fee_vault_pda, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                ],
                data: borsh::to_vec(&PortalInstruction::CloseSession).unwrap(),
            };
            process_portal_instruction(
                rpc_client,
                config,
                instruction,
                *sign_only,
                *dump_transaction_message,
                blockhash_query,
                nonce_account.as_ref(),
                *nonce_authority,
                *fee_payer,
                *compute_unit_price,
            )
            .await
        }
        PortalCliCommand::DepositFee {
            portal_program_id,
            depositor,
            recipient,
            lamports,
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority,
            fee_payer,
            compute_unit_price,
        } => {
            let portal_program_id = resolve_portal_program_id(config, *portal_program_id)?;
            let depositor = config.signers[*depositor];
            let session_pda = find_session_pda(&portal_program_id);
            let deposit_receipt_pda =
                find_deposit_receipt_pda(&portal_program_id, &session_pda, recipient);
            let instruction = Instruction {
                program_id: portal_program_id,
                accounts: vec![
                    AccountMeta::new(depositor.pubkey(), true),
                    AccountMeta::new_readonly(session_pda, false),
                    AccountMeta::new(deposit_receipt_pda, false),
                    AccountMeta::new_readonly(*recipient, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                ],
                data: borsh::to_vec(&PortalInstruction::DepositFee {
                    lamports: *lamports,
                })
                .unwrap(),
            };
            process_portal_instruction(
                rpc_client,
                config,
                instruction,
                *sign_only,
                *dump_transaction_message,
                blockhash_query,
                nonce_account.as_ref(),
                *nonce_authority,
                *fee_payer,
                *compute_unit_price,
            )
            .await
        }
        PortalCliCommand::Delegate {
            portal_program_id,
            authority,
            delegated_account,
            owner_program,
            grid_id,
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority,
            fee_payer,
            compute_unit_price,
        } => {
            let portal_program_id = resolve_portal_program_id(config, *portal_program_id)?;
            let authority = config.signers[*authority];
            let delegated_account_pubkey = config.signers[*delegated_account].pubkey();
            let delegation_record_pda =
                find_delegation_record_pda(&portal_program_id, &delegated_account_pubkey);

            // Sonic: Stage keypair-wallet delegation in one transaction. Portal::Delegate
            // requires the delegated account to already be Portal-owned, so create
            // a missing keypair account as Portal-owned or assign an existing empty
            // system account before invoking Portal.
            let rent_exempt_lamports = rpc_client.get_minimum_balance_for_rent_exemption(0).await?;
            let delegated_account = rpc_client
                .get_account_with_commitment(&delegated_account_pubkey, config.commitment)
                .await?
                .value;
            let owner_program = (*owner_program).unwrap_or_else(|| {
                default_owner_program_for_delegate(delegated_account.as_ref(), &portal_program_id)
            });

            let mut instructions = vec![];
            if let Some(instruction) = delegated_account_staging_instruction(
                &authority.pubkey(),
                &delegated_account_pubkey,
                &portal_program_id,
                delegated_account.as_ref(),
                rent_exempt_lamports,
            )? {
                instructions.push(instruction);
            }

            // Generate a fresh buffer keypair, create a 0-byte account owned by
            // owner_program in the same tx. Portal's buffer-data copy is a no-op
            // for the keypair-wallet flow (both delegated_account and buffer have
            // 0 bytes).
            let buffer_keypair = Keypair::new();
            let create_buffer_ix = system_instruction::create_account(
                &authority.pubkey(),
                &buffer_keypair.pubkey(),
                rent_exempt_lamports,
                0,
                &owner_program,
            );
            instructions.push(create_buffer_ix);

            let delegate_ix = Instruction {
                program_id: portal_program_id,
                accounts: vec![
                    AccountMeta::new(authority.pubkey(), true),
                    AccountMeta::new_readonly(system_program::id(), false),
                    AccountMeta::new(delegated_account_pubkey, true),
                    AccountMeta::new_readonly(owner_program, false),
                    AccountMeta::new(delegation_record_pda, false),
                    AccountMeta::new_readonly(buffer_keypair.pubkey(), false),
                ],
                data: borsh::to_vec(&PortalInstruction::Delegate { grid_id: *grid_id }).unwrap(),
            };
            instructions.push(delegate_ix);
            process_portal_instructions(
                rpc_client,
                config,
                instructions,
                &[&buffer_keypair],
                *sign_only,
                *dump_transaction_message,
                blockhash_query,
                nonce_account.as_ref(),
                *nonce_authority,
                *fee_payer,
                *compute_unit_price,
            )
            .await
        }
        PortalCliCommand::Undelegate {
            portal_program_id,
            authority,
            delegated_account,
            owner_program,
            sign_only,
            dump_transaction_message,
            blockhash_query,
            nonce_account,
            nonce_authority,
            fee_payer,
            compute_unit_price,
        } => {
            let portal_program_id = resolve_portal_program_id(config, *portal_program_id)?;
            let authority = config.signers[*authority];
            let delegation_record_pda =
                find_delegation_record_pda(&portal_program_id, delegated_account);
            let owner_program = if let Some(owner_program) = *owner_program {
                owner_program
            } else {
                let delegation_record = rpc_client
                    .get_account_with_commitment(&delegation_record_pda, config.commitment)
                    .await?
                    .value
                    .ok_or_else(|| {
                        CliError::BadParameter(format!(
                            "Delegation record {delegation_record_pda} not found; pass \
                             OWNER_PROGRAM explicitly"
                        ))
                    })?;
                delegation_record_owner_program(&delegation_record)?
            };
            let instruction = Instruction {
                program_id: portal_program_id,
                accounts: vec![
                    AccountMeta::new(authority.pubkey(), true),
                    AccountMeta::new(*delegated_account, false),
                    AccountMeta::new_readonly(owner_program, false),
                    AccountMeta::new(delegation_record_pda, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                ],
                data: borsh::to_vec(&PortalInstruction::Undelegate).unwrap(),
            };
            process_portal_instruction(
                rpc_client,
                config,
                instruction,
                *sign_only,
                *dump_transaction_message,
                blockhash_query,
                nonce_account.as_ref(),
                *nonce_authority,
                *fee_payer,
                *compute_unit_price,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{clap_app::get_clap_app, cli::parse_command},
        solana_clap_utils::keypair::DefaultSigner,
        solana_keypair::{Keypair, write_keypair_file},
        tempfile::NamedTempFile,
    };

    fn make_default_signer() -> (DefaultSigner, Keypair, NamedTempFile) {
        let keypair = Keypair::new();
        let file = NamedTempFile::new().unwrap();
        write_keypair_file(&keypair, file.path()).unwrap();
        let path = file.path().to_str().unwrap().to_string();
        (DefaultSigner::new("keypair", path), keypair, file)
    }

    #[test]
    fn test_parse_portal_open_session() {
        let (default_signer, _keypair, _tmp) = make_default_signer();
        let matches = get_clap_app("test", "desc", "version").get_matches_from(vec![
            "test",
            "portal",
            "open-session",
            "--portal",
            "11111111111111111111111111111111",
            "--grid",
            "7",
            "--ttl",
            "100",
            "--fee-cap",
            "0.000005",
            "--sign-only",
            "--blockhash",
            "11111111111111111111111111111111",
        ]);

        let command = parse_command(&matches, &default_signer, &mut None)
            .unwrap()
            .command;
        assert!(matches!(
            command,
            CliCommand::Portal(PortalCliCommand::OpenSession {
                portal_program_id,
                grid_id: 7,
                ttl_slots: 100,
                fee_cap: 5_000,
                sign_only: true,
                ..
            }) if portal_program_id == Some(Pubkey::default())
        ));
    }

    #[test]
    fn test_parse_portal_deposit_fee_defaults_recipient_to_depositor() {
        let (default_signer, keypair, _tmp) = make_default_signer();
        let matches = get_clap_app("test", "desc", "version").get_matches_from(vec![
            "test",
            "portal",
            "deposit-fee",
            "0.000000042",
            "--sign-only",
            "--blockhash",
            "11111111111111111111111111111111",
        ]);

        let command = parse_command(&matches, &default_signer, &mut None)
            .unwrap()
            .command;
        assert!(matches!(
            command,
            CliCommand::Portal(PortalCliCommand::DepositFee {
                recipient,
                lamports: 42,
                ..
            }) if recipient == keypair.pubkey()
        ));
    }

    #[test]
    fn test_parse_portal_aliases() {
        let (default_signer, _keypair, _tmp) = make_default_signer();
        let matches = get_clap_app("test", "desc", "version").get_matches_from(vec![
            "test",
            "p",
            "os",
            "-p",
            "11111111111111111111111111111111",
            "--grid",
            "7",
            "--ttl",
            "100",
            "--fee-cap",
            "0.000005",
            "--sign-only",
            "--blockhash",
            "11111111111111111111111111111111",
        ]);

        let command = parse_command(&matches, &default_signer, &mut None)
            .unwrap()
            .command;
        assert!(matches!(
            command,
            CliCommand::Portal(PortalCliCommand::OpenSession {
                portal_program_id,
                grid_id: 7,
                ttl_slots: 100,
                fee_cap: 5_000,
                sign_only: true,
                ..
            }) if portal_program_id == Some(Pubkey::default())
        ));
    }

    #[test]
    fn test_parse_portal_delegate_defaults_owner_program() {
        let (default_signer, _keypair, _tmp) = make_default_signer();
        let delegated_keypair = Keypair::new();
        let delegated_file = NamedTempFile::new().unwrap();
        write_keypair_file(&delegated_keypair, delegated_file.path()).unwrap();
        let delegated_path = delegated_file.path().to_str().unwrap();
        let matches = get_clap_app("test", "desc", "version").get_matches_from(vec![
            "test",
            "portal",
            "delegate",
            delegated_path,
            "--sign-only",
            "--blockhash",
            "11111111111111111111111111111111",
        ]);

        let command = parse_command(&matches, &default_signer, &mut None)
            .unwrap()
            .command;
        assert!(matches!(
            command,
            CliCommand::Portal(PortalCliCommand::Delegate {
                owner_program: None,
                grid_id: 0,
                sign_only: true,
                ..
            })
        ));
    }

    #[test]
    fn test_parse_portal_undelegate_defaults_owner_program() {
        let (default_signer, _keypair, _tmp) = make_default_signer();
        let delegated_account = Pubkey::new_unique().to_string();
        let matches = get_clap_app("test", "desc", "version").get_matches_from(vec![
            "test",
            "portal",
            "undelegate",
            delegated_account.as_str(),
            "--sign-only",
            "--blockhash",
            "11111111111111111111111111111111",
        ]);

        let command = parse_command(&matches, &default_signer, &mut None)
            .unwrap()
            .command;
        assert!(matches!(
            command,
            CliCommand::Portal(PortalCliCommand::Undelegate {
                owner_program: None,
                sign_only: true,
                ..
            })
        ));
    }

    #[test]
    fn test_parse_portal_open_session_default_fee_cap() {
        let (default_signer, _keypair, _tmp) = make_default_signer();
        let matches = get_clap_app("test", "desc", "version").get_matches_from(vec![
            "test",
            "portal",
            "open-session",
            "--sign-only",
            "--blockhash",
            "11111111111111111111111111111111",
        ]);

        let command = parse_command(&matches, &default_signer, &mut None)
            .unwrap()
            .command;
        assert!(matches!(
            command,
            CliCommand::Portal(PortalCliCommand::OpenSession {
                grid_id: 0,
                ttl_slots: 78_840_000,
                fee_cap: 1_000_000_000_000_000,
                sign_only: true,
                ..
            })
        ));
    }

    #[test]
    fn test_delegation_record_owner_program_decodes_owner() {
        let owner_program = Pubkey::new_unique();
        let mut data = vec![0; DelegationRecord::LEN];
        data[0] = DelegationRecord::DISCRIMINATOR;
        data[1..33].copy_from_slice(owner_program.as_ref());
        let account = Account {
            lamports: 1,
            data,
            owner: Pubkey::new_unique(),
            executable: false,
            rent_epoch: 0,
        };

        assert_eq!(
            delegation_record_owner_program(&account).unwrap(),
            owner_program
        );
    }

    #[test]
    fn test_default_owner_program_for_delegate_uses_system_for_missing_account() {
        assert_eq!(
            default_owner_program_for_delegate(None, &Pubkey::new_unique()),
            system_program::id()
        );
    }

    #[test]
    fn test_default_owner_program_for_delegate_uses_existing_owner() {
        let owner_program = Pubkey::new_unique();
        let account = Account {
            lamports: 1,
            data: vec![],
            owner: owner_program,
            executable: false,
            rent_epoch: 0,
        };

        assert_eq!(
            default_owner_program_for_delegate(Some(&account), &Pubkey::new_unique()),
            owner_program
        );
    }

    #[test]
    fn test_default_owner_program_for_delegate_uses_system_for_portal_owned_account() {
        let portal_program_id = Pubkey::new_unique();
        let account = Account {
            lamports: 1,
            data: vec![],
            owner: portal_program_id,
            executable: false,
            rent_epoch: 0,
        };

        assert_eq!(
            default_owner_program_for_delegate(Some(&account), &portal_program_id),
            system_program::id()
        );
    }

    #[test]
    fn test_delegate_stages_missing_account_as_portal_owned() {
        let authority = Pubkey::new_unique();
        let delegated_account = Pubkey::new_unique();
        let portal_program_id = Pubkey::new_unique();
        let instruction = delegated_account_staging_instruction(
            &authority,
            &delegated_account,
            &portal_program_id,
            None,
            123,
        )
        .unwrap()
        .unwrap();

        assert_eq!(instruction.program_id, system_program::id());
        assert_eq!(instruction.accounts[0], AccountMeta::new(authority, true));
        assert_eq!(
            instruction.accounts[1],
            AccountMeta::new(delegated_account, true)
        );
        let system_instruction: system_instruction::SystemInstruction =
            bincode::deserialize(&instruction.data).unwrap();
        assert_eq!(
            system_instruction,
            system_instruction::SystemInstruction::CreateAccount {
                lamports: 123,
                space: 0,
                owner: portal_program_id,
            }
        );
    }

    #[test]
    fn test_delegate_assigns_existing_empty_system_account() {
        let authority = Pubkey::new_unique();
        let delegated_account = Pubkey::new_unique();
        let portal_program_id = Pubkey::new_unique();
        let account = Account {
            lamports: 1,
            data: vec![],
            owner: system_program::id(),
            executable: false,
            rent_epoch: 0,
        };
        let instruction = delegated_account_staging_instruction(
            &authority,
            &delegated_account,
            &portal_program_id,
            Some(&account),
            123,
        )
        .unwrap()
        .unwrap();

        assert_eq!(instruction.program_id, system_program::id());
        assert_eq!(
            instruction.accounts,
            vec![AccountMeta::new(delegated_account, true)]
        );
        let system_instruction: system_instruction::SystemInstruction =
            bincode::deserialize(&instruction.data).unwrap();
        assert_eq!(
            system_instruction,
            system_instruction::SystemInstruction::Assign {
                owner: portal_program_id,
            }
        );
    }

    #[test]
    fn test_delegate_skips_stage_for_portal_owned_account() {
        let authority = Pubkey::new_unique();
        let delegated_account = Pubkey::new_unique();
        let portal_program_id = Pubkey::new_unique();
        let account = Account {
            lamports: 1,
            data: vec![],
            owner: portal_program_id,
            executable: false,
            rent_epoch: 0,
        };

        assert!(
            delegated_account_staging_instruction(
                &authority,
                &delegated_account,
                &portal_program_id,
                Some(&account),
                123,
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn test_delegate_rejects_non_empty_system_account() {
        let authority = Pubkey::new_unique();
        let delegated_account = Pubkey::new_unique();
        let portal_program_id = Pubkey::new_unique();
        let account = Account {
            lamports: 1,
            data: vec![1],
            owner: system_program::id(),
            executable: false,
            rent_epoch: 0,
        };

        let error = delegated_account_staging_instruction(
            &authority,
            &delegated_account,
            &portal_program_id,
            Some(&account),
            123,
        )
        .unwrap_err();
        assert!(error.to_string().contains("zero-data system accounts"));
    }

    #[test]
    fn test_default_portal_program_id_for_rpc_url() {
        assert_eq!(
            default_portal_program_id_for_rpc_url("http://localhost:8899"),
            Some(Pubkey::from_str(LOCALNET_DEFAULT_PORTAL_PROGRAM_ID).unwrap())
        );
        assert_eq!(
            default_portal_program_id_for_rpc_url("https://api.devnet.solana.com"),
            Some(Pubkey::from_str(DEVNET_DEFAULT_PORTAL_PROGRAM_ID).unwrap())
        );
        assert_eq!(
            default_portal_program_id_for_rpc_url("https://api.mainnet-beta.solana.com"),
            None
        );
    }
}
