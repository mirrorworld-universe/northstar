use {
    crate::{
        find_session_bridge_pda, find_session_pda, PortalError, RegisterSessionBridge, Session,
        SessionBridge,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::{
        account_info::AccountInfo,
        instruction::{Seed, Signer},
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{rent::Rent, Sysvar},
        ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
    pinocchio_system::instructions::CreateAccount,
};

/// Register the SPL bridge/vault used by a session and mint.
///
/// This is Portal-owned discovery state for SDKs and routers. Asset custody and
/// accounting remain in the bridge program.
///
/// Accounts:
/// 0. `[signer, writable]` authority / payer (must match `session.authority`)
/// 1. `[]` session PDA
/// 2. `[writable]` session_bridge PDA (`["session_bridge", session, mint]`)
/// 3. `[]` system_program
#[p_instruction(
    id = 22,
    accounts = [
        authority(signer, mut),
        session(state = Session),
        session_bridge(mut, state = SessionBridge),
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
    accounts: &[AccountInfo],
    RegisterSessionBridge {
        mint,
        bridge_program,
        vault,
        token_program,
    }: RegisterSessionBridge,
) -> ProgramResult {
    if accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let authority = &accounts[0];
    let session = &accounts[1];
    let session_bridge = &accounts[2];
    let _system_program = &accounts[3];

    if !authority.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }

    let (expected_session, _) = find_session_pda(program_id);
    if session.key() != &expected_session {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if session.owner() != program_id {
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }

    let session_state = Session::try_from_slice(&session.try_borrow_data()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !session_state.is_valid() {
        return Err(PortalError::SessionStateInvalid.into());
    }
    if &session_state.authority != authority.key() {
        return Err(PortalError::Unauthorized.into());
    }

    let (expected_bridge, bridge_bump) = find_session_bridge_pda(program_id, session.key(), &mint);
    if session_bridge.key() != &expected_bridge {
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let bridge_state = SessionBridge {
        discriminator: SessionBridge::DISCRIMINATOR,
        session: *session.key(),
        mint,
        bridge_program,
        vault,
        token_program,
        bump: bridge_bump,
    };

    if session_bridge.lamports() == 0 {
        let rent = Rent::get()?;
        let bridge_size = crate::account_size(&bridge_state);
        let lamports = rent.minimum_balance(bridge_size);
        let bridge_bump_bytes = [bridge_bump];
        let signer_seeds = &[
            Seed::from(SessionBridge::SEED_PREFIX),
            Seed::from(session.key().as_ref()),
            Seed::from(mint.as_ref()),
            Seed::from(bridge_bump_bytes.as_ref()),
        ];
        let signer = Signer::from(signer_seeds);

        CreateAccount {
            from: authority,
            to: session_bridge,
            lamports,
            space: bridge_size as u64,
            owner: program_id,
        }
        .invoke_signed(&[signer])?;
    } else {
        if session_bridge.owner() != program_id {
            return Err(PortalError::InvalidAccountData.into());
        }
        let existing = SessionBridge::try_from_slice(&session_bridge.try_borrow_data()?)
            .map_err(|_| PortalError::InvalidAccountData)?;
        if existing.is_valid() && existing == bridge_state {
            return Ok(());
        }
        if existing.is_valid() {
            return Err(PortalError::InvalidAccountData.into());
        }
    }

    let mut bridge_data = session_bridge.try_borrow_mut_data()?;
    BorshSerialize::serialize(&bridge_state, &mut &mut bridge_data[..]).unwrap();

    Ok(())
}
