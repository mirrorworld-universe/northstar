use {
    super::{
        account_size, create_pda, key_bytes, load_er_token_account, load_session_bridge,
        require_bridge_delegation, require_signer, require_system_program, store,
    },
    crate::state::{TokenDepositProgress, TokenDepositReceipt},
    borsh::BorshDeserialize,
    pinocchio::{
        cpi::Seed, error::ProgramError, AccountView as AccountInfo, Address as Pubkey,
        ProgramResult,
    },
};

fn receipt_balance(
    program: &Pubkey,
    bridge: &AccountInfo,
    account: &AccountInfo,
    receipt: &AccountInfo,
) -> Result<u64, ProgramError> {
    let (expected, bump) = Pubkey::find_program_address(
        &[
            TokenDepositReceipt::SEED_PREFIX,
            bridge.address().as_ref(),
            account.address().as_ref(),
        ],
        program,
    );
    if receipt.address() != &expected {
        return Err(ProgramError::InvalidSeeds);
    }
    if receipt.lamports() == 0 {
        return Ok(0);
    }
    if !receipt.owned_by(program) {
        return Err(ProgramError::InvalidAccountOwner);
    }
    let state = TokenDepositReceipt::try_from_slice(&receipt.try_borrow()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !state.is_valid()
        || state.bump != bump
        || state.session_bridge != key_bytes(bridge.address())
        || state.er_token_account != key_bytes(account.address())
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(state.balance)
}

pub(super) fn initialize_origin(
    program: &Pubkey,
    payer: &AccountInfo,
    bridge: &AccountInfo,
    account: &AccountInfo,
    receipt: &AccountInfo,
    origin: &mut AccountInfo,
    system: &AccountInfo,
) -> ProgramResult {
    let balance = receipt_balance(program, bridge, account, receipt)?;
    let (expected, bump) = Pubkey::find_program_address(
        &[
            TokenDepositProgress::ORIGIN_SEED,
            account.address().as_ref(),
        ],
        program,
    );
    if origin.address() != &expected {
        return Err(ProgramError::InvalidSeeds);
    }
    let state = TokenDepositProgress {
        discriminator: TokenDepositProgress::ORIGIN_DISCRIMINATOR,
        balance,
        bump,
    };
    if !origin.owned_by(program) {
        let bump_seed = [bump];
        create_pda(
            payer,
            origin,
            account_size(&state),
            program,
            [
                Seed::from(TokenDepositProgress::ORIGIN_SEED),
                Seed::from(account.address().as_ref()),
                Seed::from(&bump_seed),
            ],
            system,
        )?;
    } else {
        let previous = TokenDepositProgress::try_from_slice(&origin.try_borrow()?)
            .map_err(|_| ProgramError::InvalidAccountData)?;
        if previous.discriminator != state.discriminator
            || previous.bump != bump
            || previous.balance > balance
        {
            return Err(ProgramError::InvalidAccountData);
        }
    }
    store(origin, &state)
}

pub(super) fn apply(program: &Pubkey, accounts: &mut [AccountInfo], balance: u64) -> ProgramResult {
    let [payer, account, bridge, portal, session, delegation, receipt, origin, cursor, system, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_signer(payer)?;
    require_system_program(system)?;
    let bridge_state = load_session_bridge(program, bridge, portal)?;
    if session.address() != &bridge_state.session
        || session.address() != &Pubkey::find_program_address(&[b"session"], portal.address()).0
        || !session.owned_by(portal.address())
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let session_state = northstar_portal::Session::try_from_slice(&session.try_borrow()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if !session_state.is_valid() || session_state.validator != *payer.address() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // On L1 a delegated token account belongs to Portal, so this instruction is ER-only.
    let mut state = load_er_token_account(program, account)?;
    require_bridge_delegation(program, account, delegation, portal)?;
    let record = northstar_portal::DelegationRecord::try_from_slice(&delegation.try_borrow()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if record.grid_id != session_state.grid_id {
        return Err(ProgramError::InvalidAccountData);
    }
    if state.session_bridge != key_bytes(bridge.address())
        || state.mint != key_bytes(&bridge_state.mint)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let available = receipt_balance(program, bridge, account, receipt)?;
    let (expected_origin, origin_bump) = Pubkey::find_program_address(
        &[
            TokenDepositProgress::ORIGIN_SEED,
            account.address().as_ref(),
        ],
        program,
    );
    if origin.address() != &expected_origin || !origin.owned_by(program) {
        return Err(ProgramError::InvalidSeeds);
    }
    let baseline = TokenDepositProgress::try_from_slice(&origin.try_borrow()?)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    if baseline.discriminator != TokenDepositProgress::ORIGIN_DISCRIMINATOR
        || baseline.bump != origin_bump
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let baseline_bytes = baseline.balance.to_le_bytes();
    let (expected_cursor, bump) = Pubkey::find_program_address(
        &[
            TokenDepositProgress::CURSOR_SEED,
            account.address().as_ref(),
            &baseline_bytes,
        ],
        program,
    );
    if cursor.address() != &expected_cursor {
        return Err(ProgramError::InvalidSeeds);
    }
    let mut progress = if cursor.owned_by(program) {
        let progress = TokenDepositProgress::try_from_slice(&cursor.try_borrow()?)
            .map_err(|_| ProgramError::InvalidAccountData)?;
        if progress.discriminator != TokenDepositProgress::CURSOR_DISCRIMINATOR
            || progress.bump != bump
            || progress.balance < baseline.balance
        {
            return Err(ProgramError::InvalidAccountData);
        }
        progress
    } else {
        TokenDepositProgress {
            discriminator: TokenDepositProgress::CURSOR_DISCRIMINATOR,
            balance: baseline.balance,
            bump,
        }
    };
    if balance > available || balance <= progress.balance {
        return Err(ProgramError::InvalidArgument);
    }
    state.amount = state
        .amount
        .checked_add(balance - progress.balance)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    progress.balance = balance;
    if !cursor.owned_by(program) {
        let bump_seed = [bump];
        create_pda(
            payer,
            cursor,
            account_size(&progress),
            program,
            [
                Seed::from(TokenDepositProgress::CURSOR_SEED),
                Seed::from(account.address().as_ref()),
                Seed::from(&baseline_bytes),
                Seed::from(&bump_seed),
            ],
            system,
        )?;
    }
    store(cursor, &progress)?;
    store(account, &state)
}
