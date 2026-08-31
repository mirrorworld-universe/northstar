use {
    crate::{
        find_session_bridge_pda, find_session_pda, PortalError, RegisterSessionBridge, Session,
        SessionBridge,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        cpi::{Seed, Signer},
        error::ProgramError,
        sysvars::{rent::Rent, Sysvar},
        AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
    pinocchio_system::instructions::CreateAccount,
};

const TOKEN_VAULT_SEED_PREFIX: &[u8] = b"token_vault";

/// Register the SPL bridge/vault used by a session and mint.
///
/// This is Portal-owned discovery state for SDKs and routers. Asset custody and
/// accounting remain in the bridge program.
///
/// Accounts:
/// 0. `[signer, writable]` payer
/// 1. `[]` session PDA
/// 2. `[writable]` session_bridge PDA (`["session_bridge", session, mint]`)
/// 3. `[]` initialized SPL mint
/// 4. `[executable]` bridge program
/// 5. `[executable]` SPL Token program
/// 6. `[]` system_program
#[p_instruction(
    id = 22,
    accounts = [
        payer(signer, mut),
        session(state = Session),
        session_bridge(mut, state = SessionBridge),
        mint_account,
        bridge_program_account,
        token_program_account,
        system_program
    ],
    data = [
        mint: Pubkey,
        bridge_program: Pubkey,
        vault: Pubkey,
        token_program: Pubkey
    ]
)]
pub fn process_register_session_bridge(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    RegisterSessionBridge {
        mint,
        bridge_program,
        vault,
        token_program,
    }: RegisterSessionBridge,
) -> ProgramResult {
    let [payer, session, session_bridge, mint_account, bridge_program_account, token_program_account, system_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !payer.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let (expected_session, _) = find_session_pda(program_id);
    if session.address() != &expected_session {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if !session.owned_by(program_id) {
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }

    let session_state = Session::try_from_slice(&session.try_borrow()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !session_state.is_valid() {
        return Err(PortalError::SessionStateInvalid.into());
    }

    let (expected_bridge, bridge_bump) =
        find_session_bridge_pda(program_id, session.address(), &mint);
    if session_bridge.address() != &expected_bridge {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    if mint_account.address() != &mint
        || bridge_program_account.address() != &bridge_program
        || token_program_account.address() != &token_program
    {
        return Err(PortalError::InvalidAccountData.into());
    }
    if token_program != pinocchio_token::ID
        || !token_program_account.executable()
        || !bridge_program_account.executable()
    {
        return Err(ProgramError::IncorrectProgramId);
    }

    let mint_state = pinocchio_token::state::Mint::from_account_view(mint_account)?;
    if !mint_state.is_initialized() {
        return Err(PortalError::InvalidAccountData.into());
    }

    let (expected_vault, _) = Pubkey::find_program_address(
        &[TOKEN_VAULT_SEED_PREFIX, session_bridge.address().as_ref()],
        &bridge_program,
    );
    if vault != expected_vault {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let bridge_state = SessionBridge {
        discriminator: SessionBridge::DISCRIMINATOR,
        session: *session.address(),
        mint,
        bridge_program,
        vault,
        token_program,
        bump: bridge_bump,
    };

    if session_bridge.lamports() == 0 {
        let rent = Rent::get()?;
        let bridge_size = crate::account_size(&bridge_state);
        let lamports = rent.try_minimum_balance(bridge_size)?;
        let bridge_bump_bytes = [bridge_bump];
        let signer_seeds = &[
            Seed::from(SessionBridge::SEED_PREFIX),
            Seed::from(session.address().as_ref()),
            Seed::from(mint.as_ref()),
            Seed::from(bridge_bump_bytes.as_ref()),
        ];
        let signer = Signer::from(signer_seeds);

        CreateAccount {
            from: payer,
            to: session_bridge,
            lamports,
            space: bridge_size as u64,
            owner: program_id,
        }
        .invoke_signed(&[signer])?;
    } else {
        if !session_bridge.owned_by(program_id) {
            return Err(PortalError::InvalidAccountData.into());
        }
        let existing = SessionBridge::try_from_slice(&session_bridge.try_borrow()?)
            .map_err(|_| PortalError::InvalidAccountData)?;
        if existing.is_valid() && existing == bridge_state {
            return Ok(());
        }
        if existing.is_valid() {
            return Err(PortalError::InvalidAccountData.into());
        }
    }

    let mut bridge_data = session_bridge.try_borrow_mut()?;
    BorshSerialize::serialize(&bridge_state, &mut &mut bridge_data[..]).unwrap();

    Ok(())
}
