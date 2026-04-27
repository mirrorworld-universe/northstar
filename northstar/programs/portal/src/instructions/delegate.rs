use {
    crate::{
        error::PortalError,
        pda::{find_delegate_buffer_pda, find_delegation_record_pda},
        state::DelegationRecord,
    },
    borsh::BorshSerialize,
    pinocchio::{
        ProgramResult,
        account_info::AccountInfo,
        instruction::{Seed, Signer},
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvars::{Sysvar, rent::Rent},
    },
    pinocchio_system::instructions::CreateAccount,
};

/// Delegate an account into a NorthStar Ephemeral Rollup session.
///
/// Two flows are supported:
///
/// 1. **Keypair-wallet delegation** (5 accounts). The original flow: `delegated_account`
///    is a fresh keypair-controlled, empty-data account that the caller has already
///    handed off to Portal via `system::Assign`. Used by the NorthStarSDK fast-trading
///    wallet pattern.
///
/// 2. **PDA-with-buffer delegation** (6 accounts). The new flow for stateful
///    program-owned PDAs (AMM pools, vaults, etc.). The owner program performs a
///    "buffer dance" before invoking this CPI: allocate a buffer PDA at
///    `["portal_buffer", delegated_account]` under `owner_program`, sized to the
///    delegated account's data length; copy data into the buffer; zero the
///    delegated account's data; reassign it from `owner_program` to
///    `system_program`; CPI `system::Assign(delegated_account, portal_program)`
///    signed by the owner program with the delegated account's PDA seeds; finally
///    CPI `Portal::Delegate` with the buffer at index 5. Inside this call, Portal
///    validates the buffer's derivation and ownership, then copies buffer →
///    delegated_account so the state is restored.
///
/// In both flows, the existing checks (payer signer, delegated_account Portal-owned,
/// delegation_record PDA correctness, not-already-initialized) apply. The new buffer
/// validation only runs when the buffer is supplied.
///
/// Authorization: `payer` must sign in both flows, and `delegated_account` must sign —
/// either as a keypair (flow 1) or via `invoke_signed` with the owner program's seeds
/// (flow 2). This is the proof that the original controller of the account consented
/// to delegation.
///
/// Accounts:
/// 0. `[signer, writable]` payer
/// 1. `[signer, writable]` delegated_account (must already be Portal-owned at CPI
///    time — caller does this via `system::Assign`)
/// 2. `[]` owner_program (the program that originally owned the account; recorded
///    in `DelegationRecord.owner_program`)
/// 3. `[writable]` delegation_record PDA (created here; seeded
///    `["delegation", delegated_account]` under Portal)
/// 4. `[]` system_program
/// 5. `[]` buffer (optional, for flow 2): PDA seeded
///    `["portal_buffer", delegated_account]` under `owner_program`, owned by
///    `owner_program`, containing the data to install into delegated_account
pub fn process_delegate(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    grid_id: u64,
) -> ProgramResult {
    pinocchio_log::log!("Instruction: Delegate, grid_id={}", grid_id);

    if accounts.len() < 5 {
        pinocchio_log::log!("ERROR: Delegate failed: not enough account keys");
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let payer = &accounts[0];
    let delegated_account = &accounts[1];
    let owner_program = &accounts[2];
    let delegation_record = &accounts[3];
    let _system_program = &accounts[4];
    let buffer = accounts.get(5);

    if !payer.is_signer() {
        pinocchio_log::log!("ERROR: Delegate failed: payer is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    // The delegated account must sign — either as a keypair (flow 1) or via
    // `invoke_signed` from the owner program (flow 2). This is the consent proof.
    if !delegated_account.is_signer() {
        pinocchio_log::log!("ERROR: Delegate failed: delegated account is not signer");
        return Err(PortalError::Unauthorized.into());
    }

    if delegated_account.owner() != program_id {
        pinocchio_log::log!("ERROR: Delegate failed: delegated account owner mismatch");
        return Err(PortalError::DelegatedAccountOwnerMismatch.into());
    }

    let delegated_key = *delegated_account.key();
    let (expected_delegation_key, bump) = find_delegation_record_pda(program_id, &delegated_key);

    if delegation_record.key() != &expected_delegation_key {
        pinocchio_log::log!("ERROR: Delegate failed: delegation record PDA mismatch");
        return Err(PortalError::InvalidPdaSeeds.into());
    }

    if delegation_record.lamports() > 0 {
        pinocchio_log::log!("ERROR: Delegate failed: delegation record already initialized");
        return Err(PortalError::DelegationRecordAlreadyInitialized.into());
    }

    // If a buffer is supplied (flow 2), validate it before doing any state mutation.
    // Validation order matters: we want to reject bad buffers before creating the
    // delegation_record so a failed Delegate leaves no orphaned PDAs behind.
    if let Some(buffer_acc) = buffer {
        let (expected_buffer_key, _) =
            find_delegate_buffer_pda(owner_program.key(), &delegated_key);

        if buffer_acc.key() != &expected_buffer_key {
            pinocchio_log::log!("ERROR: Delegate failed: buffer PDA mismatch");
            return Err(PortalError::DelegateBufferPdaMismatch.into());
        }

        if buffer_acc.owner() != owner_program.key() {
            pinocchio_log::log!("ERROR: Delegate failed: buffer not owned by owner_program");
            return Err(PortalError::DelegateBufferOwnerMismatch.into());
        }

        if buffer_acc.data_len() != delegated_account.data_len() {
            pinocchio_log::log!("ERROR: Delegate failed: buffer/delegated size mismatch");
            return Err(PortalError::DelegateBufferSizeMismatch.into());
        }
    }

    // Create the delegation_record PDA owned by Portal.
    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(DelegationRecord::LEN);

    let bump_bytes = [bump];
    let seeds = &[
        Seed::from(DelegationRecord::SEED_PREFIX),
        Seed::from(delegated_key.as_ref()),
        Seed::from(bump_bytes.as_ref()),
    ];
    let signer = Signer::from(seeds);

    CreateAccount {
        from: payer,
        to: delegation_record,
        lamports,
        space: DelegationRecord::LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[signer])?;

    let delegation_state = DelegationRecord {
        discriminator: DelegationRecord::DISCRIMINATOR,
        owner_program: *owner_program.key(),
        grid_id,
        bump,
    };
    let mut delegation_data = delegation_record.try_borrow_mut_data()?;
    BorshSerialize::serialize(
        &delegation_state,
        &mut &mut delegation_data[..DelegationRecord::LEN],
    )
    .unwrap();
    drop(delegation_data);

    // Flow 2: copy buffer → delegated_account, restoring the program-owned state
    // that was zeroed during the owner-reassign dance. Portal owns delegated_account
    // at this point, so writing its data is allowed.
    if let Some(buffer_acc) = buffer {
        let buffer_data = buffer_acc.try_borrow_data()?;
        let mut delegated_data = delegated_account.try_borrow_mut_data()?;
        delegated_data.copy_from_slice(&buffer_data);
    }

    pinocchio_log::log!("Delegate success");

    Ok(())
}
