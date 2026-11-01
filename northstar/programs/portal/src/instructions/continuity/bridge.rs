use {
    super::{hash_parts, load_session_v2},
    crate::{
        find_session_bridge_v2_pda, PortalError, RegisterSessionBridge, SessionBridge,
        SessionBridgeV2,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        cpi::{Seed, Signer},
        error::ProgramError,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
};

#[p_instruction(
    id = 36,
    accounts = [payer(signer,mut), session, session_bridge(mut), mint_account,
        bridge_program_account, token_program_account, system_program],
    data = [mint: Pubkey, bridge_program: Pubkey, vault: Pubkey, token_program: Pubkey]
)]
pub fn process_register_session_bridge_v2(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    RegisterSessionBridge {
        mint,
        bridge_program,
        vault,
        token_program,
    }: RegisterSessionBridge,
) -> ProgramResult {
    let [payer, session, registration, mint_account, bridge_account, token_account, system] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let frame = load_session_v2(program_id, session)?;
    if !payer.is_signer() || payer.address() != &frame.base.authority {
        return Err(PortalError::Unauthorized.into());
    }
    if !payer.is_writable()
        || !registration.is_writable()
        || system.address() != &pinocchio_system::ID
    {
        return Err(PortalError::InvalidAccountData.into());
    }
    if frame.base.is_expired(Clock::get()?.slot) {
        return Err(PortalError::SessionExpired.into());
    }
    if mint_account.address() != &mint
        || bridge_account.address() != &bridge_program
        || token_account.address() != &token_program
        || token_program != pinocchio_token::ID
        || !bridge_account.executable()
        || !token_account.executable()
    {
        return Err(ProgramError::IncorrectProgramId);
    }
    let mint_state = pinocchio_token::state::Mint::from_account_view(mint_account)?;
    if !mint_state.is_initialized() {
        return Err(PortalError::InvalidAccountData.into());
    }
    let context = frame
        .identity(program_id, session.address())
        .commitment(hash_parts)
        .map_err(|_| PortalError::InvalidAccountData)?;
    let (expected, bump) = find_session_bridge_v2_pda(program_id, &context, &mint);
    let expected_vault =
        Pubkey::find_program_address(&[b"token_vault", expected.as_ref()], &bridge_program).0;
    if registration.address() != &expected || vault != expected_vault {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let state = SessionBridgeV2 {
        discriminator: SessionBridgeV2::DISCRIMINATOR,
        base: SessionBridge {
            discriminator: SessionBridge::DISCRIMINATOR,
            session: *session.address(),
            mint,
            bridge_program,
            vault,
            token_program,
            bump,
        },
        session_context: context,
    };
    if !registration.is_data_empty() {
        if !registration.owned_by(program_id) {
            return Err(PortalError::InvalidAccountData.into());
        }
        let existing = SessionBridgeV2::try_from_slice(&registration.try_borrow()?)
            .map_err(|_| PortalError::InvalidAccountData)?;
        return if existing.is_valid() && existing == state {
            Ok(())
        } else {
            Err(PortalError::InvalidAccountData.into())
        };
    }
    let size = crate::account_size(&state);
    let bump_bytes = [bump];
    let seeds = &[
        Seed::from(SessionBridgeV2::SEED_PREFIX),
        Seed::from(context.as_ref()),
        Seed::from(mint.as_ref()),
        Seed::from(bump_bytes.as_ref()),
    ];
    super::super::initialize_pda_account(
        payer,
        registration,
        Rent::get()?.try_minimum_balance(size)?,
        size as u64,
        program_id,
        Signer::from(seeds),
    )?;
    state
        .serialize(&mut &mut registration.try_borrow_mut()?[..])
        .map_err(|_| ProgramError::InvalidAccountData)
}
