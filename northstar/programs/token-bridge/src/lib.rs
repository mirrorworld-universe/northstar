pub mod instruction;
pub mod state;

use {
    borsh::{BorshDeserialize, BorshSerialize},
    instruction::TokenBridgeInstruction,
    solana_account_info::{next_account_info, AccountInfo},
    solana_instruction::{AccountMeta, Instruction},
    solana_program::{
        program::{invoke, invoke_signed},
        sysvar::{rent::Rent, Sysvar},
    },
    solana_program_error::{ProgramError, ProgramResult},
    solana_program_pack::Pack,
    solana_pubkey::Pubkey,
    solana_sdk_ids::system_program,
    solana_system_interface::instruction as system_instruction,
    spl_token_interface::{instruction as token_instruction, state::Account as SplTokenAccount},
    state::{BridgeBuffer, ErTokenAccount, TokenVault},
};

#[derive(BorshDeserialize)]
struct SessionBridge {
    discriminator: u8,
    _session: [u8; 32],
    mint: [u8; 32],
    bridge_program: [u8; 32],
    vault: [u8; 32],
    token_program: [u8; 32],
    _bump: u8,
}

impl SessionBridge {
    const DISCRIMINATOR: u8 = 5;

    fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

solana_pubkey::declare_id!("HeVLVaSa9WnFai9aTRJ3UR2c4jwbMe5nbjagmDP1GbXR");

#[cfg(not(feature = "no-entrypoint"))]
solana_program_entrypoint::entrypoint!(process_instruction);

pub fn find_token_vault_pda(program_id: &Pubkey, session_bridge: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[TokenVault::SEED_PREFIX, session_bridge.as_ref()],
        program_id,
    )
}

pub fn find_er_token_account_pda(
    program_id: &Pubkey,
    session_bridge: &Pubkey,
    owner: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            ErTokenAccount::SEED_PREFIX,
            session_bridge.as_ref(),
            owner.as_ref(),
        ],
        program_id,
    )
}

pub fn find_buffer_pda(program_id: &Pubkey, er_token_account: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[BridgeBuffer::SEED_PREFIX, er_token_account.as_ref()],
        program_id,
    )
}

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let instruction = TokenBridgeInstruction::try_from_slice(instruction_data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    match instruction {
        TokenBridgeInstruction::InitializeVault => process_initialize_vault(program_id, accounts),
        TokenBridgeInstruction::InitializeErTokenAccount { owner } => {
            process_initialize_er_token_account(program_id, accounts, owner)
        }
        TokenBridgeInstruction::Deposit { amount, decimals } => {
            process_deposit(program_id, accounts, amount, decimals)
        }
        TokenBridgeInstruction::Transfer { amount } => {
            process_transfer(program_id, accounts, amount)
        }
        TokenBridgeInstruction::Withdraw { amount, decimals } => {
            process_withdraw(program_id, accounts, amount, decimals)
        }
        TokenBridgeInstruction::DelegateErTokenAccount { grid_id } => {
            process_delegate_er_token_account(program_id, accounts, grid_id)
        }
        TokenBridgeInstruction::UndelegateErTokenAccount => {
            process_undelegate_er_token_account(program_id, accounts)
        }
    }
}

fn process_initialize_vault(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let payer = next_account_info(account_info_iter)?;
    let vault = next_account_info(account_info_iter)?;
    let session_bridge = next_account_info(account_info_iter)?;
    let portal_program = next_account_info(account_info_iter)?;
    let vault_token_account = next_account_info(account_info_iter)?;
    let system_program_info = next_account_info(account_info_iter)?;

    require_signer(payer)?;
    require_system_program(system_program_info)?;
    let bridge = load_session_bridge(program_id, session_bridge, portal_program)?;
    if bridge.vault != key_bytes(vault.key) {
        return Err(ProgramError::InvalidSeeds);
    }

    let (expected_vault, bump) = find_token_vault_pda(program_id, session_bridge.key);
    if expected_vault != *vault.key {
        return Err(ProgramError::InvalidSeeds);
    }

    let vault_token = unpack_token_account(vault_token_account)?;
    if vault_token.owner != *vault.key || vault_token.mint.to_bytes() != bridge.mint {
        return Err(ProgramError::InvalidAccountData);
    }

    if vault.lamports() == 0 {
        create_pda(
            payer,
            vault,
            TokenVault::LEN,
            program_id,
            &[
                TokenVault::SEED_PREFIX,
                session_bridge.key.as_ref(),
                &[bump],
            ],
            system_program_info,
        )?;
    } else if vault.owner != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }

    let state = TokenVault {
        discriminator: TokenVault::DISCRIMINATOR,
        session_bridge: key_bytes(session_bridge.key),
        mint: bridge.mint,
        vault_token_account: key_bytes(vault_token_account.key),
        token_program: bridge.token_program,
        bump,
    };
    store(vault, &state, TokenVault::LEN)
}

fn process_initialize_er_token_account(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    owner: [u8; 32],
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let payer = next_account_info(account_info_iter)?;
    let er_token_account = next_account_info(account_info_iter)?;
    let session_bridge = next_account_info(account_info_iter)?;
    let portal_program = next_account_info(account_info_iter)?;
    let system_program_info = next_account_info(account_info_iter)?;

    require_signer(payer)?;
    require_system_program(system_program_info)?;
    let bridge = load_session_bridge(program_id, session_bridge, portal_program)?;
    let owner_key = Pubkey::new_from_array(owner);
    let (expected, bump) = find_er_token_account_pda(program_id, session_bridge.key, &owner_key);
    if expected != *er_token_account.key {
        return Err(ProgramError::InvalidSeeds);
    }

    if er_token_account.lamports() == 0 {
        create_pda(
            payer,
            er_token_account,
            ErTokenAccount::LEN,
            program_id,
            &[
                ErTokenAccount::SEED_PREFIX,
                session_bridge.key.as_ref(),
                owner.as_ref(),
                &[bump],
            ],
            system_program_info,
        )?;
    } else if er_token_account.owner != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }

    let state = ErTokenAccount {
        discriminator: ErTokenAccount::DISCRIMINATOR,
        session_bridge: key_bytes(session_bridge.key),
        owner,
        mint: bridge.mint,
        amount: 0,
        bump,
    };
    store(er_token_account, &state, ErTokenAccount::LEN)
}

fn process_deposit(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    amount: u64,
    decimals: u8,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let owner = next_account_info(account_info_iter)?;
    let vault = next_account_info(account_info_iter)?;
    let er_token_account = next_account_info(account_info_iter)?;
    let session_bridge = next_account_info(account_info_iter)?;
    let portal_program = next_account_info(account_info_iter)?;
    let source_token_account = next_account_info(account_info_iter)?;
    let vault_token_account = next_account_info(account_info_iter)?;
    let mint = next_account_info(account_info_iter)?;
    let token_program = next_account_info(account_info_iter)?;

    require_signer(owner)?;
    let bridge = load_session_bridge(program_id, session_bridge, portal_program)?;
    let vault_state = load_vault(program_id, vault, session_bridge.key)?;
    require_bridge_token_accounts(
        &bridge,
        &vault_state,
        vault_token_account,
        mint,
        token_program,
    )?;

    let mut er_state = load_er_token_account(program_id, er_token_account)?;
    require_er_account(&er_state, session_bridge.key, owner.key, &bridge.mint)?;

    let ix = token_instruction::transfer_checked(
        token_program.key,
        source_token_account.key,
        mint.key,
        vault_token_account.key,
        owner.key,
        &[],
        amount,
        decimals,
    )?;
    invoke(
        &ix,
        &[
            source_token_account.clone(),
            mint.clone(),
            vault_token_account.clone(),
            owner.clone(),
            token_program.clone(),
        ],
    )?;

    er_state.amount = er_state
        .amount
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    store(er_token_account, &er_state, ErTokenAccount::LEN)
}

fn process_transfer(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let authority = next_account_info(account_info_iter)?;
    let source = next_account_info(account_info_iter)?;
    let destination = next_account_info(account_info_iter)?;

    require_signer(authority)?;
    let mut source_state = load_er_token_account(program_id, source)?;
    let mut destination_state = load_er_token_account(program_id, destination)?;
    if source_state.owner != key_bytes(authority.key)
        || source_state.session_bridge != destination_state.session_bridge
        || source_state.mint != destination_state.mint
    {
        return Err(ProgramError::InvalidAccountData);
    }
    source_state.amount = source_state
        .amount
        .checked_sub(amount)
        .ok_or(ProgramError::InsufficientFunds)?;
    destination_state.amount = destination_state
        .amount
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    store(source, &source_state, ErTokenAccount::LEN)?;
    store(destination, &destination_state, ErTokenAccount::LEN)
}

fn process_withdraw(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    amount: u64,
    decimals: u8,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let owner = next_account_info(account_info_iter)?;
    let vault = next_account_info(account_info_iter)?;
    let er_token_account = next_account_info(account_info_iter)?;
    let session_bridge = next_account_info(account_info_iter)?;
    let portal_program = next_account_info(account_info_iter)?;
    let vault_token_account = next_account_info(account_info_iter)?;
    let destination_token_account = next_account_info(account_info_iter)?;
    let mint = next_account_info(account_info_iter)?;
    let token_program = next_account_info(account_info_iter)?;

    require_signer(owner)?;
    let bridge = load_session_bridge(program_id, session_bridge, portal_program)?;
    let vault_state = load_vault(program_id, vault, session_bridge.key)?;
    require_bridge_token_accounts(
        &bridge,
        &vault_state,
        vault_token_account,
        mint,
        token_program,
    )?;

    let mut er_state = load_er_token_account(program_id, er_token_account)?;
    require_er_account(&er_state, session_bridge.key, owner.key, &bridge.mint)?;
    er_state.amount = er_state
        .amount
        .checked_sub(amount)
        .ok_or(ProgramError::InsufficientFunds)?;

    let ix = token_instruction::transfer_checked(
        token_program.key,
        vault_token_account.key,
        mint.key,
        destination_token_account.key,
        vault.key,
        &[],
        amount,
        decimals,
    )?;
    invoke_signed(
        &ix,
        &[
            vault_token_account.clone(),
            mint.clone(),
            destination_token_account.clone(),
            vault.clone(),
            token_program.clone(),
        ],
        &[&[
            TokenVault::SEED_PREFIX,
            session_bridge.key.as_ref(),
            &[vault_state.bump],
        ]],
    )?;

    store(er_token_account, &er_state, ErTokenAccount::LEN)
}

fn process_delegate_er_token_account(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    grid_id: u64,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let payer = next_account_info(account_info_iter)?;
    let er_token_account = next_account_info(account_info_iter)?;
    let bridge_program = next_account_info(account_info_iter)?;
    let session_bridge = next_account_info(account_info_iter)?;
    let portal_program = next_account_info(account_info_iter)?;
    let session = next_account_info(account_info_iter)?;
    let delegation_record = next_account_info(account_info_iter)?;
    let buffer = next_account_info(account_info_iter)?;
    let system_program_info = next_account_info(account_info_iter)?;

    require_signer(payer)?;
    require_self_program(program_id, bridge_program)?;
    require_system_program(system_program_info)?;
    load_session_bridge(program_id, session_bridge, portal_program)?;
    let er_state = load_er_token_account(program_id, er_token_account)?;
    if er_state.session_bridge != key_bytes(session_bridge.key) {
        return Err(ProgramError::InvalidAccountData);
    }

    let (expected_record, _) = Pubkey::find_program_address(
        &[b"delegation", er_token_account.key.as_ref()],
        portal_program.key,
    );
    if expected_record != *delegation_record.key {
        return Err(ProgramError::InvalidSeeds);
    }

    create_buffer(
        program_id,
        payer,
        buffer,
        er_token_account,
        system_program_info,
    )?;
    copy_data(er_token_account, buffer)?;
    er_token_account.try_borrow_mut_data()?.fill(0);
    er_token_account.assign(&system_program::id());

    let er_seeds = er_signer_seeds(&er_state);
    invoke_signed(
        &system_instruction::assign(er_token_account.key, portal_program.key),
        &[er_token_account.clone(), system_program_info.clone()],
        &[&er_seeds],
    )?;

    let ix = Instruction {
        program_id: *portal_program.key,
        accounts: vec![
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(*session.key, false),
            AccountMeta::new(*er_token_account.key, true),
            AccountMeta::new_readonly(*program_id, false),
            AccountMeta::new(*delegation_record.key, false),
            AccountMeta::new_readonly(*buffer.key, false),
        ],
        data: encode_portal_delegate(grid_id),
    };
    invoke_signed(
        &ix,
        &[
            payer.clone(),
            system_program_info.clone(),
            session.clone(),
            er_token_account.clone(),
            bridge_program.clone(),
            delegation_record.clone(),
            buffer.clone(),
        ],
        &[&er_seeds],
    )?;

    close_buffer(buffer, payer)
}

fn process_undelegate_er_token_account(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let authority = next_account_info(account_info_iter)?;
    let er_token_account = next_account_info(account_info_iter)?;
    let bridge_program = next_account_info(account_info_iter)?;
    let portal_program = next_account_info(account_info_iter)?;
    let session = next_account_info(account_info_iter)?;
    let delegation_record = next_account_info(account_info_iter)?;
    let buffer = next_account_info(account_info_iter)?;
    let system_program_info = next_account_info(account_info_iter)?;

    require_signer(authority)?;
    require_self_program(program_id, bridge_program)?;
    require_system_program(system_program_info)?;
    if er_token_account.owner != portal_program.key {
        return Err(ProgramError::InvalidAccountOwner);
    }

    create_buffer(
        program_id,
        authority,
        buffer,
        er_token_account,
        system_program_info,
    )?;
    copy_data(er_token_account, buffer)?;

    let ix = Instruction {
        program_id: *portal_program.key,
        accounts: vec![
            AccountMeta::new(*authority.key, true),
            AccountMeta::new(*er_token_account.key, false),
            AccountMeta::new_readonly(*program_id, false),
            AccountMeta::new(*delegation_record.key, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(*session.key, false),
        ],
        data: vec![10],
    };
    invoke(
        &ix,
        &[
            authority.clone(),
            er_token_account.clone(),
            bridge_program.clone(),
            delegation_record.clone(),
            system_program_info.clone(),
            session.clone(),
        ],
    )?;

    if er_token_account.owner != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }
    copy_data(buffer, er_token_account)?;
    close_buffer(buffer, authority)
}

fn load_session_bridge(
    program_id: &Pubkey,
    session_bridge: &AccountInfo,
    portal_program: &AccountInfo,
) -> Result<SessionBridge, ProgramError> {
    if session_bridge.owner != portal_program.key {
        return Err(ProgramError::InvalidAccountOwner);
    }
    let bridge = SessionBridge::try_from_slice(&session_bridge.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !bridge.is_valid() || bridge.bridge_program != key_bytes(program_id) {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(bridge)
}

fn load_vault(
    program_id: &Pubkey,
    vault: &AccountInfo,
    session_bridge: &Pubkey,
) -> Result<TokenVault, ProgramError> {
    if vault.owner != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }
    let state = TokenVault::try_from_slice(&vault.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !state.is_valid() || state.session_bridge != key_bytes(session_bridge) {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(state)
}

fn load_er_token_account(
    program_id: &Pubkey,
    account: &AccountInfo,
) -> Result<ErTokenAccount, ProgramError> {
    if account.owner != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }
    let state = ErTokenAccount::try_from_slice(&account.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !state.is_valid() {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(state)
}

fn require_er_account(
    state: &ErTokenAccount,
    session_bridge: &Pubkey,
    owner: &Pubkey,
    mint: &[u8; 32],
) -> ProgramResult {
    if state.session_bridge != key_bytes(session_bridge)
        || state.owner != key_bytes(owner)
        || &state.mint != mint
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn require_bridge_token_accounts(
    bridge: &SessionBridge,
    vault: &TokenVault,
    vault_token_account: &AccountInfo,
    mint: &AccountInfo,
    token_program: &AccountInfo,
) -> ProgramResult {
    if bridge.mint != key_bytes(mint.key)
        || bridge.token_program != key_bytes(token_program.key)
        || vault.mint != bridge.mint
        || vault.token_program != bridge.token_program
        || vault.vault_token_account != key_bytes(vault_token_account.key)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn unpack_token_account(account: &AccountInfo) -> Result<SplTokenAccount, ProgramError> {
    SplTokenAccount::unpack(&account.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)
}

fn store<T: BorshSerialize>(account: &AccountInfo, state: &T, len: usize) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    BorshSerialize::serialize(state, &mut &mut data[..len])
        .map_err(|_| ProgramError::InvalidAccountData)
}

fn create_pda<'a>(
    payer: &AccountInfo<'a>,
    pda: &AccountInfo<'a>,
    space: usize,
    owner: &Pubkey,
    seeds: &[&[u8]],
    system_program_info: &AccountInfo<'a>,
) -> ProgramResult {
    let lamports = Rent::get()?.minimum_balance(space);
    invoke_signed(
        &system_instruction::create_account(payer.key, pda.key, lamports, space as u64, owner),
        &[payer.clone(), pda.clone(), system_program_info.clone()],
        &[seeds],
    )
}

fn create_buffer<'a>(
    program_id: &Pubkey,
    payer: &AccountInfo<'a>,
    buffer: &AccountInfo<'a>,
    er_token_account: &AccountInfo<'a>,
    system_program_info: &AccountInfo<'a>,
) -> ProgramResult {
    let (expected, bump) = find_buffer_pda(program_id, er_token_account.key);
    if expected != *buffer.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if buffer.lamports() == 0 {
        create_pda(
            payer,
            buffer,
            er_token_account.data_len(),
            program_id,
            &[
                BridgeBuffer::SEED_PREFIX,
                er_token_account.key.as_ref(),
                &[bump],
            ],
            system_program_info,
        )?;
    }
    if buffer.owner != program_id || buffer.data_len() != er_token_account.data_len() {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn copy_data(from: &AccountInfo, to: &AccountInfo) -> ProgramResult {
    let source = from.try_borrow_data()?;
    let mut destination = to.try_borrow_mut_data()?;
    if source.len() != destination.len() {
        return Err(ProgramError::InvalidAccountData);
    }
    destination.copy_from_slice(&source);
    Ok(())
}

fn close_buffer(buffer: &AccountInfo, recipient: &AccountInfo) -> ProgramResult {
    let lamports = buffer.lamports();
    **recipient.try_borrow_mut_lamports()? = recipient
        .lamports()
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    **buffer.try_borrow_mut_lamports()? = 0;
    buffer.try_borrow_mut_data()?.fill(0);
    Ok(())
}

fn er_signer_seeds(state: &ErTokenAccount) -> [&[u8]; 4] {
    [
        ErTokenAccount::SEED_PREFIX,
        state.session_bridge.as_ref(),
        state.owner.as_ref(),
        core::slice::from_ref(&state.bump),
    ]
}

fn require_signer(account: &AccountInfo) -> ProgramResult {
    if !account.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    Ok(())
}

fn require_self_program(program_id: &Pubkey, account: &AccountInfo) -> ProgramResult {
    if account.key != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(())
}

fn require_system_program(account: &AccountInfo) -> ProgramResult {
    if *account.key != system_program::id() {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(())
}

fn key_bytes(key: &Pubkey) -> [u8; 32] {
    key.to_bytes()
}

fn encode_portal_delegate(grid_id: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(9);
    data.push(3);
    data.extend_from_slice(&grid_id.to_le_bytes());
    data
}
