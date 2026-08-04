use {
    super::initialize_pda_account,
    crate::{
        find_fee_vault_pda, find_session_pda, FeeVault, OpenSession, PortalError, Session,
        SettlementStatus,
    },
    borsh::BorshSerialize,
    pinocchio::{
        cpi::{Seed, Signer},
        error::ProgramError,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
};

#[p_instruction(
    id = 0,
    accounts = [
        payer(signer, mut),
        session(mut, state = Session),
        fee_vault(mut, state = FeeVault),
        system_program
    ],
    data = [
        grid_id: u64,
        ttl_slots: u64,
        fee_cap: u64,
        validator: Pubkey,
        settlement_interval_slots: u64
    ]
)]
pub fn process_open_session(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    OpenSession {
        grid_id,
        ttl_slots,
        fee_cap,
        validator,
        settlement_interval_slots,
    }: OpenSession,
) -> ProgramResult {
    pinocchio_log::log!(
        "Instruction: OpenSession, grid_id={}, ttl_slots={}, fee_cap={}, \
         settlement_interval_slots={}",
        grid_id,
        ttl_slots,
        fee_cap,
        settlement_interval_slots
    );

    let [payer, session, fee_vault, _system_program, ..] = accounts else {
        pinocchio_log::log!("ERROR: OpenSession failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let payer_key = payer.address();

    if !payer.is_signer() {
        pinocchio_log::log!("ERROR: OpenSession failed: payer is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    let (expected_session_key, session_bump) = find_session_pda(program_id);
    let (expected_fee_vault_key, fee_vault_bump) = find_fee_vault_pda(program_id);

    if session.address() != &expected_session_key {
        pinocchio_log::log!("ERROR: OpenSession failed: session PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    if fee_vault.address() != &expected_fee_vault_key {
        pinocchio_log::log!("ERROR: OpenSession failed: fee vault PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    let clock = Clock::get()?;
    let current_slot = clock.slot;

    let session_state = Session {
        discriminator: Session::DISCRIMINATOR,
        grid_id,
        ttl_slots,
        fee_cap,
        created_at: current_slot,
        nonce: 0,
        authority: *payer_key,
        validator,
        settlement_interval_slots,
        last_settled_l1_slot: current_slot,
        last_settled_er_slot: 0,
        settlement_status: SettlementStatus::Idle,
        settlement_er_slot: 0,
        settlement_checksum: [0; 32],
        settlement_accumulator: [0; 32],
        settlement_started_l1_slot: 0,
        bump: session_bump,
    };
    let fee_vault_state = FeeVault {
        discriminator: FeeVault::DISCRIMINATOR,
        authority: payer_key.to_bytes(),
        bump: fee_vault_bump,
    };
    let rent = Rent::get()?;
    let session_size = crate::account_size(&session_state);
    let fee_vault_size = crate::account_size(&fee_vault_state);
    let session_lamports = rent.try_minimum_balance(session_size)?;
    let fee_vault_lamports = rent.try_minimum_balance(fee_vault_size)?;

    // Create Session PDA
    let session_bump_bytes = [session_bump];
    let session_seeds = &[
        Seed::from(Session::SEED_PREFIX),
        Seed::from(session_bump_bytes.as_ref()),
    ];
    let session_signer = Signer::from(session_seeds);

    initialize_pda_account(
        payer,
        session,
        session_lamports,
        session_size as u64,
        program_id,
        session_signer,
    )?;

    // Create FeeVault PDA
    let fee_vault_bump_bytes = [fee_vault_bump];
    let fee_vault_seeds = &[
        Seed::from(FeeVault::SEED_PREFIX),
        Seed::from(fee_vault_bump_bytes.as_ref()),
    ];
    let fee_vault_signer = Signer::from(fee_vault_seeds);

    initialize_pda_account(
        payer,
        fee_vault,
        fee_vault_lamports,
        fee_vault_size as u64,
        program_id,
        fee_vault_signer,
    )?;

    // Write Session state
    let mut session_data = session.try_borrow_mut()?;
    BorshSerialize::serialize(&session_state, &mut &mut session_data[..]).unwrap();

    // Write FeeVault state
    let mut fee_vault_data = fee_vault.try_borrow_mut()?;
    BorshSerialize::serialize(&fee_vault_state, &mut &mut fee_vault_data[..]).unwrap();

    pinocchio_log::log!("OpenSession success");

    Ok(())
}
