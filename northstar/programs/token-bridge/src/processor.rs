use {
    crate::{
        instruction::TokenBridgeInstruction,
        portal_abi::{is_valid_settlement_checkpoint, is_valid_settlement_session},
        state::{account_size, BridgeBuffer, ErTokenAccount, TokenDepositReceipt, TokenVault},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_portal::{Checkpoint, DelegationRecord, Session, SessionBridge},
    pinocchio::{
        account_info::AccountInfo,
        entrypoint::deserialize,
        instruction::{AccountMeta, Instruction, Seed, Signer},
        no_allocator,
        program::{invoke, invoke_signed},
        program_error::ProgramError,
        pubkey::{find_program_address, Pubkey},
        sysvars::{rent::Rent, Sysvar},
        ProgramResult, SUCCESS,
    },
    pinocchio_system::instructions::CreateAccount,
    pinocchio_token::instructions::TransferChecked,
    solana_program_pack::Pack,
    spl_token_interface::state::Account as SplTokenAccount,
};

pinocchio_pubkey::declare_id!("HeVLVaSa9WnFai9aTRJ3UR2c4jwbMe5nbjagmDP1GbXR");

no_allocator!();

#[cfg_attr(not(feature = "no-entrypoint"), no_mangle)]
/// # Safety
/// `input` must point to a valid serialized Solana program input buffer.
pub unsafe extern "C" fn entrypoint(input: *mut u8) -> u64 {
    const MAX_TOKEN_BRIDGE_ACCOUNTS: usize = 16;
    const UNINIT: core::mem::MaybeUninit<AccountInfo> = core::mem::MaybeUninit::uninit();
    let mut account_storage = [UNINIT; MAX_TOKEN_BRIDGE_ACCOUNTS];
    let (program_id, account_count, instruction_data) =
        deserialize::<MAX_TOKEN_BRIDGE_ACCOUNTS>(input, &mut account_storage);
    let accounts = core::slice::from_raw_parts(
        account_storage.as_ptr() as *const AccountInfo,
        account_count,
    );
    match process_instruction(program_id, accounts, instruction_data) {
        Ok(()) => SUCCESS,
        Err(error) => error.into(),
    }
}

fn next_account_info<'a>(
    accounts: &mut core::slice::Iter<'a, AccountInfo>,
) -> Result<&'a AccountInfo, ProgramError> {
    accounts.next().ok_or(ProgramError::NotEnoughAccountKeys)
}

fn find_token_vault_pda(program_id: &Pubkey, session_bridge: &Pubkey) -> (Pubkey, u8) {
    find_program_address(
        &[TokenVault::SEED_PREFIX, session_bridge.as_ref()],
        program_id,
    )
}

fn find_er_token_account_pda(
    program_id: &Pubkey,
    session_bridge: &Pubkey,
    owner: &Pubkey,
) -> (Pubkey, u8) {
    find_program_address(
        &[
            ErTokenAccount::SEED_PREFIX,
            session_bridge.as_ref(),
            owner.as_ref(),
        ],
        program_id,
    )
}

fn find_token_deposit_receipt_pda(
    program_id: &Pubkey,
    session_bridge: &Pubkey,
    er_token_account: &Pubkey,
) -> (Pubkey, u8) {
    find_program_address(
        &[
            TokenDepositReceipt::SEED_PREFIX,
            session_bridge.as_ref(),
            er_token_account.as_ref(),
        ],
        program_id,
    )
}

fn find_buffer_pda(program_id: &Pubkey, er_token_account: &Pubkey) -> (Pubkey, u8) {
    find_program_address(
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
        TokenBridgeInstruction::StartWithdrawal { amount, decimals } => {
            process_start_withdrawal(program_id, accounts, amount, decimals)
        }
        TokenBridgeInstruction::SettleWithdrawal {
            er_slot,
            checksum,
            amount,
            withdrawn,
            decimals,
        } => process_settle_withdrawal(
            program_id, accounts, er_slot, checksum, amount, withdrawn, decimals,
        ),
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
    if bridge.vault != key_bytes(vault.key()) {
        return Err(ProgramError::InvalidSeeds);
    }

    let (expected_vault, bump) = find_token_vault_pda(program_id, session_bridge.key());
    if expected_vault != *vault.key() {
        return Err(ProgramError::InvalidSeeds);
    }

    let vault_token = unpack_token_account(vault_token_account)?;
    if vault_token.owner.to_bytes() != *vault.key() || vault_token.mint.to_bytes() != bridge.mint {
        return Err(ProgramError::InvalidAccountData);
    }

    let state = TokenVault {
        discriminator: TokenVault::DISCRIMINATOR,
        session_bridge: key_bytes(session_bridge.key()),
        mint: bridge.mint,
        vault_token_account: key_bytes(vault_token_account.key()),
        token_program: bridge.token_program,
        deposited: 0,
        withdrawn: 0,
        bump,
    };
    if vault.lamports() == 0 {
        let bump_seed = [bump];
        create_pda(
            payer,
            vault,
            account_size(&state),
            program_id,
            [
                Seed::from(TokenVault::SEED_PREFIX),
                Seed::from(session_bridge.key()),
                Seed::from(&bump_seed),
            ],
            system_program_info,
        )?;
    } else if vault.owner() != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }

    store(vault, &state)
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
    let owner_key = owner;
    let (expected, bump) = find_er_token_account_pda(program_id, session_bridge.key(), &owner_key);
    if expected != *er_token_account.key() {
        return Err(ProgramError::InvalidSeeds);
    }

    let state = ErTokenAccount {
        discriminator: ErTokenAccount::DISCRIMINATOR,
        session_bridge: key_bytes(session_bridge.key()),
        owner,
        mint: bridge.mint,
        amount: 0,
        bump,
    };
    if er_token_account.lamports() == 0 {
        let bump_seed = [bump];
        create_pda(
            payer,
            er_token_account,
            account_size(&state),
            program_id,
            [
                Seed::from(ErTokenAccount::SEED_PREFIX),
                Seed::from(session_bridge.key()),
                Seed::from(&owner),
                Seed::from(&bump_seed),
            ],
            system_program_info,
        )?;
    } else if er_token_account.owner() != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }

    store(er_token_account, &state)
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
    let deposit_receipt = next_account_info(account_info_iter)?;
    let delegation_record = next_account_info(account_info_iter)?;
    let system_program_info = next_account_info(account_info_iter)?;

    require_signer(owner)?;
    require_system_program(system_program_info)?;
    let bridge = load_session_bridge(program_id, session_bridge, portal_program)?;
    let mut vault_state = load_vault(program_id, vault, session_bridge.key())?;
    require_bridge_token_accounts(
        &bridge,
        &vault_state,
        vault_token_account,
        mint,
        token_program,
    )?;

    let mut er_state = load_er_token_account_data(er_token_account)?;
    require_er_account(&er_state, session_bridge.key(), owner.key(), &bridge.mint)?;
    let is_delegated = er_token_account.owner() == portal_program.key();
    if !is_delegated && er_token_account.owner() != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }
    if is_delegated {
        require_bridge_delegation(
            program_id,
            er_token_account,
            delegation_record,
            portal_program,
        )?;
    }

    let (expected_receipt, receipt_bump) =
        find_token_deposit_receipt_pda(program_id, session_bridge.key(), er_token_account.key());
    if expected_receipt != *deposit_receipt.key() {
        return Err(ProgramError::InvalidSeeds);
    }

    if token_program.key() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    TransferChecked {
        from: source_token_account,
        mint,
        to: vault_token_account,
        authority: owner,
        amount,
        decimals,
    }
    .invoke()?;

    let initial_receipt = TokenDepositReceipt {
        discriminator: TokenDepositReceipt::DISCRIMINATOR,
        session_bridge: key_bytes(session_bridge.key()),
        er_token_account: key_bytes(er_token_account.key()),
        balance: 0,
        withdrawn: 0,
        bump: receipt_bump,
    };
    if deposit_receipt.lamports() == 0 {
        let bump_seed = [receipt_bump];
        create_pda(
            owner,
            deposit_receipt,
            account_size(&initial_receipt),
            program_id,
            [
                Seed::from(TokenDepositReceipt::SEED_PREFIX),
                Seed::from(session_bridge.key()),
                Seed::from(er_token_account.key()),
                Seed::from(&bump_seed),
            ],
            system_program_info,
        )?;
    } else if deposit_receipt.owner() != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }

    let mut receipt = if deposit_receipt.try_borrow_data()?[0] == 0 {
        initial_receipt
    } else {
        TokenDepositReceipt::try_from_slice(&deposit_receipt.try_borrow_data()?)
            .map_err(|_| ProgramError::InvalidAccountData)?
    };
    if !receipt.is_valid()
        || receipt.session_bridge != key_bytes(session_bridge.key())
        || receipt.er_token_account != key_bytes(er_token_account.key())
        || receipt.bump != receipt_bump
    {
        return Err(ProgramError::InvalidAccountData);
    }
    receipt.balance = receipt
        .balance
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    store(deposit_receipt, &receipt)?;
    vault_state.deposited = vault_state
        .deposited
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    store(vault, &vault_state)?;

    if !is_delegated {
        er_state.amount = er_state
            .amount
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        store(er_token_account, &er_state)?;
    }
    Ok(())
}

fn process_transfer(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let authority = next_account_info(account_info_iter)?;
    let source = next_account_info(account_info_iter)?;
    let destination = next_account_info(account_info_iter)?;

    require_signer(authority)?;
    let mut source_state = load_er_token_account(program_id, source)?;
    let mut destination_state = load_er_token_account(program_id, destination)?;
    if source_state.owner != key_bytes(authority.key())
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
    store(source, &source_state)?;
    store(destination, &destination_state)
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
    let mut vault_state = load_vault(program_id, vault, session_bridge.key())?;
    require_bridge_token_accounts(
        &bridge,
        &vault_state,
        vault_token_account,
        mint,
        token_program,
    )?;

    let mut er_state = load_er_token_account(program_id, er_token_account)?;
    require_er_account(&er_state, session_bridge.key(), owner.key(), &bridge.mint)?;
    er_state.amount = er_state
        .amount
        .checked_sub(amount)
        .ok_or(ProgramError::InsufficientFunds)?;
    vault_state.withdrawn = vault_state
        .withdrawn
        .checked_add(amount)
        .filter(|withdrawn| *withdrawn <= vault_state.deposited)
        .ok_or(ProgramError::InsufficientFunds)?;

    if token_program.key() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let vault_bump = [vault_state.bump];
    let vault_seeds = [
        Seed::from(TokenVault::SEED_PREFIX),
        Seed::from(session_bridge.key()),
        Seed::from(&vault_bump),
    ];
    let vault_signer = Signer::from(&vault_seeds);
    TransferChecked {
        from: vault_token_account,
        mint,
        to: destination_token_account,
        authority: vault,
        amount,
        decimals,
    }
    .invoke_signed(&[vault_signer])?;

    store(vault, &vault_state)?;
    store(er_token_account, &er_state)
}

fn process_start_withdrawal(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    amount: u64,
    _decimals: u8,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let owner = next_account_info(account_info_iter)?;
    let er_token_account = next_account_info(account_info_iter)?;
    let session_bridge = next_account_info(account_info_iter)?;
    let portal_program = next_account_info(account_info_iter)?;
    let destination_token_account = next_account_info(account_info_iter)?;
    let token_program = next_account_info(account_info_iter)?;

    require_signer(owner)?;
    let bridge = load_session_bridge(program_id, session_bridge, portal_program)?;
    if bridge.token_program != key_bytes(token_program.key()) {
        return Err(ProgramError::InvalidAccountData);
    }
    let destination = unpack_token_account(destination_token_account)?;
    if destination.mint.to_bytes() != bridge.mint {
        return Err(ProgramError::InvalidAccountData);
    }

    let mut er_state = load_er_token_account(program_id, er_token_account)?;
    require_er_account(&er_state, session_bridge.key(), owner.key(), &bridge.mint)?;
    er_state.amount = er_state
        .amount
        .checked_sub(amount)
        .ok_or(ProgramError::InsufficientFunds)?;
    store(er_token_account, &er_state)
}

fn process_settle_withdrawal(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    er_slot: u64,
    checksum: [u8; 32],
    amount: u64,
    withdrawn: u64,
    decimals: u8,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let validator = next_account_info(account_info_iter)?;
    let session = next_account_info(account_info_iter)?;
    let checkpoint = next_account_info(account_info_iter)?;
    let session_bridge = next_account_info(account_info_iter)?;
    let vault = next_account_info(account_info_iter)?;
    let _er_token_account = next_account_info(account_info_iter)?;
    let vault_token_account = next_account_info(account_info_iter)?;
    let destination_token_account = next_account_info(account_info_iter)?;
    let mint = next_account_info(account_info_iter)?;
    let token_program = next_account_info(account_info_iter)?;

    require_signer(validator)?;
    if session.owner() != checkpoint.owner() || session_bridge.owner() != session.owner() {
        return Err(ProgramError::InvalidAccountOwner);
    }
    let expected_checkpoint = find_program_address(
        &[
            b"checkpoint",
            session.key().as_ref(),
            &er_slot.to_le_bytes(),
        ],
        session.owner(),
    )
    .0;
    if expected_checkpoint != *checkpoint.key() {
        return Err(ProgramError::InvalidSeeds);
    }
    let session_state = Session::try_from_slice(&session.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    let checkpoint_state = Checkpoint::try_from_slice(&checkpoint.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !is_valid_settlement_session(&session_state, validator.key())
        || !is_valid_settlement_checkpoint(&checkpoint_state, session.key(), er_slot, &checksum)
    {
        return Err(ProgramError::InvalidAccountData);
    }

    let bridge = SessionBridge::try_from_slice(&session_bridge.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    let expected_session_bridge = find_program_address(
        &[
            b"session_bridge",
            session.key().as_ref(),
            bridge.mint.as_ref(),
        ],
        session.owner(),
    )
    .0;
    if !bridge.is_valid()
        || bridge.bridge_program != key_bytes(program_id)
        || bridge.session != key_bytes(session.key())
        || expected_session_bridge != *session_bridge.key()
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut vault_state = load_vault(program_id, vault, session_bridge.key())?;
    require_bridge_token_accounts(
        &bridge,
        &vault_state,
        vault_token_account,
        mint,
        token_program,
    )?;
    let destination = unpack_token_account(destination_token_account)?;
    if destination.mint.to_bytes() != bridge.mint {
        return Err(ProgramError::InvalidAccountData);
    }

    let expected_withdrawn = vault_state
        .withdrawn
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if withdrawn != expected_withdrawn || withdrawn > vault_state.deposited {
        return Err(ProgramError::InvalidArgument);
    }
    vault_state.withdrawn = withdrawn;

    if token_program.key() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let vault_bump = [vault_state.bump];
    let vault_seeds = [
        Seed::from(TokenVault::SEED_PREFIX),
        Seed::from(session_bridge.key()),
        Seed::from(&vault_bump),
    ];
    let vault_signer = Signer::from(&vault_seeds);
    TransferChecked {
        from: vault_token_account,
        mint,
        to: destination_token_account,
        authority: vault,
        amount,
        decimals,
    }
    .invoke_signed(&[vault_signer])?;
    store(vault, &vault_state)
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
    if er_state.session_bridge != key_bytes(session_bridge.key()) {
        return Err(ProgramError::InvalidAccountData);
    }

    let (expected_record, _) = find_program_address(
        &[b"delegation", er_token_account.key().as_ref()],
        portal_program.key(),
    );
    if expected_record != *delegation_record.key() {
        return Err(ProgramError::InvalidSeeds);
    }

    create_buffer(
        program_id,
        payer,
        buffer,
        er_token_account,
        system_program_info,
    )?;
    let er_account_space = er_token_account.data_len();
    copy_data(er_token_account, buffer)?;
    close_buffer(er_token_account, payer)?;
    create_pda(
        payer,
        er_token_account,
        er_account_space,
        portal_program.key(),
        er_signer_seeds(&er_state),
        system_program_info,
    )?;
    let er_seeds = er_signer_seeds(&er_state);
    let er_signer = Signer::from(&er_seeds);

    let portal_accounts = [
        AccountMeta::writable_signer(payer.key()),
        AccountMeta::readonly(&pinocchio_system::ID),
        AccountMeta::readonly(session.key()),
        AccountMeta::writable_signer(er_token_account.key()),
        AccountMeta::readonly(program_id),
        AccountMeta::writable(delegation_record.key()),
        AccountMeta::readonly(buffer.key()),
    ];
    let portal_data = encode_portal_delegate(grid_id);
    let portal_instruction = Instruction {
        program_id: portal_program.key(),
        accounts: &portal_accounts,
        data: &portal_data,
    };
    invoke_signed(
        &portal_instruction,
        &[
            payer,
            system_program_info,
            session,
            er_token_account,
            bridge_program,
            delegation_record,
            buffer,
        ],
        &[er_signer],
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
    if er_token_account.owner() != portal_program.key() {
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

    let portal_accounts = [
        AccountMeta::writable_signer(authority.key()),
        AccountMeta::writable(er_token_account.key()),
        AccountMeta::readonly(program_id),
        AccountMeta::writable(delegation_record.key()),
        AccountMeta::readonly(&pinocchio_system::ID),
        AccountMeta::readonly(session.key()),
    ];
    let portal_data = [10];
    let portal_instruction = Instruction {
        program_id: portal_program.key(),
        accounts: &portal_accounts,
        data: &portal_data,
    };
    invoke(
        &portal_instruction,
        &[
            authority,
            er_token_account,
            bridge_program,
            delegation_record,
            system_program_info,
            session,
        ],
    )?;

    if er_token_account.owner() != program_id {
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
    if session_bridge.owner() != portal_program.key() {
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
    if vault.owner() != program_id {
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
    if account.owner() != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }
    let state = ErTokenAccount::try_from_slice(&account.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !state.is_valid() {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(state)
}

fn load_er_token_account_data(account: &AccountInfo) -> Result<ErTokenAccount, ProgramError> {
    let state = ErTokenAccount::try_from_slice(&account.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !state.is_valid() {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(state)
}

fn require_bridge_delegation(
    program_id: &Pubkey,
    er_token_account: &AccountInfo,
    delegation_record: &AccountInfo,
    portal_program: &AccountInfo,
) -> ProgramResult {
    let (expected, _) = find_program_address(
        &[b"delegation", er_token_account.key().as_ref()],
        portal_program.key(),
    );
    if expected != *delegation_record.key() || delegation_record.owner() != portal_program.key() {
        return Err(ProgramError::InvalidSeeds);
    }
    let record = DelegationRecord::try_from_slice(&delegation_record.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !record.is_valid() || record.owner_program != key_bytes(program_id) {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
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
    if bridge.mint != key_bytes(mint.key())
        || bridge.token_program != key_bytes(token_program.key())
        || vault.mint != bridge.mint
        || vault.token_program != bridge.token_program
        || vault.vault_token_account != key_bytes(vault_token_account.key())
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn unpack_token_account(account: &AccountInfo) -> Result<SplTokenAccount, ProgramError> {
    SplTokenAccount::unpack(&account.try_borrow_data()?)
        .map_err(|_| ProgramError::InvalidAccountData)
}

fn store<T: BorshSerialize>(account: &AccountInfo, state: &T) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let mut output = &mut data[..];
    BorshSerialize::serialize(state, &mut output).map_err(|_| ProgramError::InvalidAccountData)?;
    if !output.is_empty() {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn create_pda<const N: usize>(
    payer: &AccountInfo,
    pda: &AccountInfo,
    space: usize,
    owner: &Pubkey,
    seeds: [Seed; N],
    system_program_info: &AccountInfo,
) -> ProgramResult {
    require_system_program(system_program_info)?;
    let lamports = Rent::get()?.minimum_balance(space);
    let signer = Signer::from(&seeds);
    CreateAccount {
        from: payer,
        to: pda,
        lamports,
        space: space as u64,
        owner,
    }
    .invoke_signed(&[signer])
}

fn create_buffer(
    program_id: &Pubkey,
    payer: &AccountInfo,
    buffer: &AccountInfo,
    er_token_account: &AccountInfo,
    system_program_info: &AccountInfo,
) -> ProgramResult {
    let (expected, bump) = find_buffer_pda(program_id, er_token_account.key());
    if expected != *buffer.key() {
        return Err(ProgramError::InvalidSeeds);
    }
    if buffer.lamports() == 0 {
        let bump_seed = [bump];
        create_pda(
            payer,
            buffer,
            er_token_account.data_len(),
            program_id,
            [
                Seed::from(BridgeBuffer::SEED_PREFIX),
                Seed::from(er_token_account.key()),
                Seed::from(&bump_seed),
            ],
            system_program_info,
        )?;
    }
    if buffer.owner() != program_id || buffer.data_len() != er_token_account.data_len() {
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
    *recipient.try_borrow_mut_lamports()? = recipient
        .lamports()
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    *buffer.try_borrow_mut_lamports()? = 0;
    buffer.close()
}

fn er_signer_seeds(state: &ErTokenAccount) -> [Seed<'_>; 4] {
    [
        Seed::from(ErTokenAccount::SEED_PREFIX),
        Seed::from(&state.session_bridge),
        Seed::from(&state.owner),
        Seed::from(core::slice::from_ref(&state.bump)),
    ]
}

fn require_signer(account: &AccountInfo) -> ProgramResult {
    if !account.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    Ok(())
}

fn require_self_program(program_id: &Pubkey, account: &AccountInfo) -> ProgramResult {
    if account.key() != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(())
}

fn require_system_program(account: &AccountInfo) -> ProgramResult {
    if *account.key() != pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(())
}

fn key_bytes(key: &Pubkey) -> [u8; 32] {
    *key
}

fn encode_portal_delegate(grid_id: u64) -> [u8; 9] {
    let mut data = [0; 9];
    data[0] = 3;
    data[1..].copy_from_slice(&grid_id.to_le_bytes());
    data
}
