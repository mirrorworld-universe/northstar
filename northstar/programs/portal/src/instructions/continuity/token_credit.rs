use {
    super::{append_ingress_v2, hash_parts, load_session_v2},
    crate::{
        find_delegation_record_pda, find_session_bridge_v2_pda, DelegationRecord, PortalError,
        RecordTokenDepositV2, SessionBridgeV2,
    },
    borsh::BorshDeserialize,
    northstar_zk_types::{
        continuity::IngressPayloadV2,
        token_bridge::{ErTokenAccount, TokenDepositProgress, TokenDepositReceipt},
    },
    pinocchio::{
        error::ProgramError, AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
};

fn fixed_state<T: BorshDeserialize>(
    account: &AccountInfo,
    length: usize,
) -> Result<T, ProgramError> {
    let data = account.try_borrow()?;
    if data.len() != length {
        return Err(PortalError::InvalidAccountData.into());
    }
    T::try_from_slice(&data).map_err(|_| PortalError::InvalidAccountData.into())
}

#[p_instruction(id = 37,
    accounts = [payer(signer,mut),system_program,session(mut),session_bridge,vault(signer),
        er_token_account,delegation_record,receipt,origin,ingress_record(mut)],
    data = [amount:u64])]
pub fn process_record_token_deposit_v2(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    args: RecordTokenDepositV2,
) -> ProgramResult {
    let [payer, system, session, registration, vault, account, delegation, receipt, origin, ingress] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !payer.is_signer() || !vault.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }
    if system.address() != &pinocchio_system::ID || args.amount == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let frame = load_session_v2(program_id, session)?;
    let context = frame
        .identity(program_id, session.address())
        .commitment(hash_parts)
        .map_err(|_| PortalError::InvalidAccountData)?;
    if !registration.owned_by(program_id) || registration.executable() {
        return Err(PortalError::InvalidAccountData.into());
    }
    let registry = SessionBridgeV2::try_from_slice(&registration.try_borrow()?)
        .map_err(|_| PortalError::InvalidAccountData)?;
    let (expected, bump) = find_session_bridge_v2_pda(program_id, &context, &registry.base.mint);
    if !registry.is_valid()
        || registry.session_context != context
        || registry.base.session != *session.address()
        || *registration.address() != expected
        || registry.base.bump != bump
        || registry.base.token_program != pinocchio_token::ID
    {
        return Err(PortalError::InvalidAccountData.into());
    }
    let bridge = registry.base.bridge_program;
    let vault_key =
        Pubkey::find_program_address(&[b"token_vault", registration.address().as_ref()], &bridge).0;
    if *vault.address() != vault_key
        || registry.base.vault != vault_key
        || !vault.owned_by(&bridge)
        || vault.executable()
    {
        return Err(PortalError::Unauthorized.into());
    }
    let (expected, bump) = find_delegation_record_pda(program_id, account.address());
    if *delegation.address() != expected
        || !delegation.owned_by(program_id)
        || !account.owned_by(program_id)
        || account.executable()
    {
        return Err(PortalError::InvalidAccountData.into());
    }
    let binding = DelegationRecord::try_from_slice(&delegation.try_borrow()?)
        .map_err(|_| PortalError::InvalidAccountData)?;
    if !binding.is_valid()
        || binding.bump != bump
        || binding.owner_program != bridge
        || binding.grid_id != frame.base.grid_id
    {
        return Err(PortalError::InvalidAccountData.into());
    }
    let er: ErTokenAccount = fixed_state(account, ErTokenAccount::SERIALIZED_LEN)?;
    let (expected, bump) = Pubkey::find_program_address(
        &[
            ErTokenAccount::SEED_PREFIX,
            registration.address().as_ref(),
            er.owner.as_ref(),
        ],
        &bridge,
    );
    if !er.is_valid()
        || er.session_bridge != registration.address().to_bytes()
        || er.mint != registry.base.mint.to_bytes()
        || *account.address() != expected
        || er.bump != bump
    {
        return Err(PortalError::InvalidAccountData.into());
    }
    let (expected, bump) = Pubkey::find_program_address(
        &[
            TokenDepositReceipt::SEED_PREFIX,
            registration.address().as_ref(),
            account.address().as_ref(),
        ],
        &bridge,
    );
    if *receipt.address() != expected || !receipt.owned_by(&bridge) || receipt.executable() {
        return Err(PortalError::InvalidAccountData.into());
    }
    let deposited: TokenDepositReceipt = fixed_state(receipt, TokenDepositReceipt::SERIALIZED_LEN)?;
    if !deposited.is_valid()
        || deposited.bump != bump
        || deposited.session_bridge != registration.address().to_bytes()
        || deposited.er_token_account != account.address().to_bytes()
        || deposited.withdrawn > deposited.balance
    {
        return Err(PortalError::InvalidAccountData.into());
    }
    let (expected, bump) = Pubkey::find_program_address(
        &[
            TokenDepositProgress::ORIGIN_SEED,
            account.address().as_ref(),
        ],
        &bridge,
    );
    if *origin.address() != expected || !origin.owned_by(&bridge) || origin.executable() {
        return Err(PortalError::InvalidAccountData.into());
    }
    let baseline: TokenDepositProgress = fixed_state(origin, TokenDepositProgress::SERIALIZED_LEN)?;
    if baseline.discriminator != TokenDepositProgress::ORIGIN_DISCRIMINATOR
        || baseline.bump != bump
        || deposited
            .balance
            .checked_sub(baseline.balance)
            .is_none_or(|delta| delta < args.amount)
    {
        return Err(PortalError::InvalidAccountData.into());
    }
    append_ingress_v2(
        program_id,
        payer,
        session,
        ingress,
        IngressPayloadV2::TokenDeposit {
            bridge: bridge.to_bytes(),
            session_bridge: registration.address().to_bytes(),
            account: account.address().to_bytes(),
            receipt: receipt.address().to_bytes(),
            mint: registry.base.mint.to_bytes(),
            token_program: registry.base.token_program.to_bytes(),
            origin_deposited: baseline.balance,
            amount: args.amount,
            cumulative_deposited: deposited.balance,
        },
    )
}
