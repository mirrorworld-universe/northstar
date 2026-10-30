use {
    super::{
        initialize_pda_account,
        open_session::{process_open_session_initialized, V2SessionInitialization},
    },
    crate::{
        find_ingress_record_v2_pda, find_session_epoch_v2_pda, find_session_pda, OpenSessionV2,
        PortalError, SessionEpochV2, SessionV2,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_zk_types::continuity::{IngressPayloadV2, IngressRecordV2, INGRESS_RECORD_SEED_V2},
    pinocchio::{
        cpi::{Seed, Signer},
        error::ProgramError,
        sysvars::{clock::Clock, rent::Rent, Sysvar},
        AccountView as AccountInfo, Address as Pubkey, ProgramResult,
    },
    pinocchio_idl_macros::p_instruction,
    solana_sha256_hasher::hashv,
};

fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    hashv(parts).to_bytes()
}

#[p_instruction(
    id = 31,
    accounts = [payer(signer, mut), session(mut), fee_vault(mut), system_program, session_epoch(mut)],
    data = [session: crate::OpenSession, l1_genesis: crate::Hash32, profile: crate::Hash32]
)]
pub fn process_open_session_v2(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    args: OpenSessionV2,
) -> ProgramResult {
    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    if args.l1_genesis == [0; 32] || args.profile == [0; 32] {
        return Err(ProgramError::InvalidInstructionData);
    }
    let (expected_epoch, bump) = find_session_epoch_v2_pda(program_id);
    let payer = &accounts[0];
    let epoch = &accounts[4];
    if !payer.is_signer() || epoch.address() != &expected_epoch || !epoch.is_writable() {
        return Err(PortalError::Unauthorized.into());
    }
    let mut counter = if epoch.is_data_empty() && epoch.owned_by(&pinocchio_system::ID) {
        SessionEpochV2 {
            discriminator: SessionEpochV2::DISCRIMINATOR,
            last_nonce: 0,
            bump,
        }
    } else {
        if !epoch.owned_by(program_id) {
            return Err(PortalError::InvalidAccountData.into());
        }
        let value = SessionEpochV2::try_from_slice(&epoch.try_borrow()?)
            .map_err(|_| PortalError::InvalidAccountData)?;
        if value.discriminator != SessionEpochV2::DISCRIMINATOR || value.bump != bump {
            return Err(PortalError::InvalidAccountData.into());
        }
        value
    };
    counter.last_nonce = counter
        .last_nonce
        .checked_add(1)
        .ok_or(PortalError::ArithmeticOverflow)?;
    if epoch.is_data_empty() {
        let size = crate::account_size(&counter);
        let rent = Rent::get()?.try_minimum_balance(size)?;
        let bump_bytes = [bump];
        let seeds = &[
            Seed::from(SessionEpochV2::SEED_PREFIX),
            Seed::from(bump_bytes.as_ref()),
        ];
        initialize_pda_account(
            payer,
            epoch,
            rent,
            size as u64,
            program_id,
            Signer::from(seeds),
        )?;
    }
    process_open_session_initialized(
        program_id,
        &mut accounts[..4],
        args.session,
        Some(V2SessionInitialization {
            nonce: counter.last_nonce,
            l1_genesis: args.l1_genesis,
            profile: args.profile,
        }),
    )?;
    counter
        .serialize(&mut &mut accounts[4].try_borrow_mut()?[..])
        .map_err(|_| PortalError::InvalidAccountData)?;
    Ok(())
}

pub(super) fn load_session_v2(
    program_id: &Pubkey,
    session: &AccountInfo,
) -> Result<SessionV2, ProgramError> {
    if session.address() != &find_session_pda(program_id).0 || !session.owned_by(program_id) {
        return Err(PortalError::SessionAccountOwnerMismatch.into());
    }
    let frame = SessionV2::try_from_slice(&session.try_borrow()?)
        .map_err(|_| PortalError::SessionDeserializeFailed)?;
    if !frame.is_valid() {
        return Err(PortalError::SessionStateInvalid.into());
    }
    Ok(frame)
}

#[inline(never)]
pub(super) fn append_ingress_v2(
    program_id: &Pubkey,
    payer: &AccountInfo,
    session: &mut AccountInfo,
    record_account: &mut AccountInfo,
    payload: IngressPayloadV2,
) -> ProgramResult {
    let mut frame = load_session_v2(program_id, session)?;
    if frame.base.is_expired(Clock::get()?.slot) {
        return Err(PortalError::SessionExpired.into());
    }
    let context = frame
        .identity(program_id, session.address())
        .commitment(hash_parts)
        .map_err(|_| PortalError::InvalidAccountData)?;
    let sequence = frame
        .ingress_count
        .checked_add(1)
        .ok_or(PortalError::ArithmeticOverflow)?;
    let (expected, bump) = find_ingress_record_v2_pda(program_id, &context, sequence);
    if record_account.address() != &expected
        || !session.is_writable()
        || !record_account.is_writable()
    {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let record = IngressRecordV2::new(
        context,
        sequence,
        Clock::get()?.slot,
        frame.ingress_root,
        payload,
        bump,
        hash_parts,
    )
    .map_err(|_| PortalError::InvalidAccountData)?;
    let size = crate::account_size(&record);
    let rent = Rent::get()?.try_minimum_balance(size)?;
    let sequence_bytes = sequence.to_le_bytes();
    let bump_bytes = [bump];
    let seeds = &[
        Seed::from(INGRESS_RECORD_SEED_V2),
        Seed::from(context.as_ref()),
        Seed::from(sequence_bytes.as_ref()),
        Seed::from(bump_bytes.as_ref()),
    ];
    initialize_pda_account(
        payer,
        record_account,
        rent,
        size as u64,
        program_id,
        Signer::from(seeds),
    )?;
    record
        .serialize(&mut &mut record_account.try_borrow_mut()?[..])
        .map_err(|_| PortalError::InvalidAccountData)?;
    frame.ingress_count = sequence;
    frame.ingress_root = record.root;
    frame
        .serialize(&mut &mut session.try_borrow_mut()?[..])
        .map_err(|_| PortalError::InvalidAccountData)?;
    Ok(())
}

#[p_instruction(
    id = 32,
    accounts = [payer(signer, mut), system_program, session(mut), delegated_account(signer, mut),
        owner_program, delegation_record(mut), buffer, ingress_record(mut)],
    data = [grid_id: u64]
)]
pub fn process_delegate_v2(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    grid_id: u64,
) -> ProgramResult {
    let [payer, system_program, session, account, owner_program, record, buffer, ingress] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let frame = load_session_v2(program_id, session)?;
    if frame.base.grid_id != grid_id || !owner_program.executable() || account.executable() {
        return Err(PortalError::Unauthorized.into());
    }
    if frame.base.settlement_status != crate::SettlementStatus::Idle {
        return Err(PortalError::SettlementInProgress.into());
    }
    super::delegate::process_delegate_account(
        program_id,
        payer,
        account,
        owner_program,
        record,
        system_program,
        buffer,
        &frame.base,
    )?;
    let payload = IngressPayloadV2::Delegation {
        account: account.address().to_bytes(),
        record: record.address().to_bytes(),
        owner: owner_program.address().to_bytes(),
        lamports: account.lamports(),
        data_len: u32::try_from(account.data_len()).map_err(|_| PortalError::InvalidAccountData)?,
        data_hash: hash_parts(&[&account.try_borrow()?]),
        executable: false,
        // Rent epoch is normalized on import, not supplied by an untrusted observer.
        rent_epoch: 0,
    };
    append_ingress_v2(program_id, payer, session, ingress, payload)
}

#[p_instruction(
    id = 33,
    accounts = [depositor(signer, mut), session(mut), deposit_receipt(mut), recipient,
        system_program, ingress_record(mut)],
    data = [lamports: u64]
)]
pub fn process_deposit_fee_v2(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    lamports: u64,
) -> ProgramResult {
    use {
        crate::{find_deposit_receipt_v2_pda, DepositReceipt, DepositReceiptV2},
        pinocchio_system::instructions::Transfer,
    };
    let [depositor, session, receipt, recipient, _system_program, ingress] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !depositor.is_signer() {
        return Err(PortalError::Unauthorized.into());
    }
    if lamports == 0
        || recipient.address() == &crate::WITHDRAWAL_SINK
        || !recipient.owned_by(&pinocchio_system::ID)
        || !recipient.is_data_empty()
        || recipient.executable()
    {
        return Err(ProgramError::InvalidInstructionData);
    }
    let frame = load_session_v2(program_id, session)?;
    let context = frame
        .identity(program_id, session.address())
        .commitment(hash_parts)
        .map_err(|_| PortalError::InvalidAccountData)?;
    let (expected, bump) = find_deposit_receipt_v2_pda(program_id, &context, recipient.address());
    if receipt.address() != &expected {
        return Err(PortalError::InvalidPdaSeeds.into());
    }
    let fresh = receipt.is_data_empty() && receipt.owned_by(&pinocchio_system::ID);
    let mut state = if fresh {
        DepositReceiptV2 {
            discriminator: DepositReceiptV2::DISCRIMINATOR,
            base: DepositReceipt {
                discriminator: DepositReceipt::DISCRIMINATOR,
                session: *session.address(),
                recipient: *recipient.address(),
                balance: 0,
                withdrawn: 0,
                bump,
            },
            session_context: context,
            total_deposited: 0,
        }
    } else {
        if !receipt.owned_by(program_id) {
            return Err(PortalError::InvalidAccountData.into());
        }
        let state = DepositReceiptV2::try_from_slice(&receipt.try_borrow()?)
            .map_err(|_| PortalError::DepositReceiptDeserializeFailed)?;
        if state.discriminator != DepositReceiptV2::DISCRIMINATOR
            || !state.base.is_valid()
            || state.session_context != context
            || state.base.session != *session.address()
            || state.base.recipient != *recipient.address()
            || state.base.bump != bump
        {
            return Err(PortalError::DepositReceiptStateInvalid.into());
        }
        state
    };
    state.total_deposited = state
        .total_deposited
        .checked_add(lamports)
        .ok_or(PortalError::ArithmeticOverflow)?;
    if fresh {
        let size = crate::account_size(&state);
        let rent = Rent::get()?.try_minimum_balance(size)?;
        let bump_bytes = [bump];
        let seeds = &[
            Seed::from(DepositReceiptV2::SEED_PREFIX),
            Seed::from(context.as_ref()),
            Seed::from(recipient.address().as_ref()),
            Seed::from(bump_bytes.as_ref()),
        ];
        initialize_pda_account(
            depositor,
            receipt,
            rent,
            size as u64,
            program_id,
            Signer::from(seeds),
        )?;
    }
    Transfer {
        from: depositor,
        to: receipt,
        lamports,
    }
    .invoke()?;
    state
        .serialize(&mut &mut receipt.try_borrow_mut()?[..])
        .map_err(|_| PortalError::InvalidAccountData)?;
    append_ingress_v2(
        program_id,
        depositor,
        session,
        ingress,
        IngressPayloadV2::SolDeposit {
            recipient: recipient.address().to_bytes(),
            receipt: receipt.address().to_bytes(),
            amount: lamports,
            cumulative_deposited: state.total_deposited,
        },
    )
}

#[p_instruction(
    id = 34,
    accounts = [proposer(signer, mut), session, checkpoint(mut), cursor(mut), system_program,
        binding(mut), prefix_record],
    data = [proposal: crate::ProposeCheckpoint, pre: northstar_zk_types::continuity::StateRootOpeningV2,
        post: northstar_zk_types::continuity::StateRootOpeningV2, readonly_l1_root: crate::Hash32]
)]
pub fn process_propose_checkpoint_v2(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    args: crate::ProposeCheckpointV2,
) -> ProgramResult {
    use northstar_zk_types::continuity::{empty_state_opening_v2, InputContextV2};
    let binding_state = {
        let [proposer, session, checkpoint, cursor, _system, binding, prefix] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        let frame = load_session_v2(program_id, session)?;
        let context = frame
            .identity(program_id, session.address())
            .commitment(hash_parts)
            .map_err(|_| PortalError::InvalidAccountData)?;
        if args.pre.session_context != context
            || args.post.session_context != context
            || args.pre.ingress_count != frame.consumed_ingress_count
            || args.pre.ingress_root != frame.consumed_ingress_root
            || args.post.ingress_count > frame.ingress_count
            || args
                .post
                .ingress_count
                .checked_sub(args.pre.ingress_count)
                .is_none_or(|count| count > args.proposal.step_count)
            || args.proposal.previous_state_root
                != args
                    .pre
                    .commitment(hash_parts)
                    .map_err(|_| PortalError::InvalidAccountData)?
            || args.proposal.new_state_root
                != args
                    .post
                    .commitment(hash_parts)
                    .map_err(|_| PortalError::InvalidAccountData)?
        {
            return Err(PortalError::CheckpointPreviousRootMismatch.into());
        }
        let first = if cursor.is_data_empty() {
            true
        } else {
            if !cursor.owned_by(program_id) {
                return Err(PortalError::InvalidAccountData.into());
            }
            let cursor = crate::CheckpointCursor::try_from_slice(&cursor.try_borrow()?)
                .map_err(|_| PortalError::InvalidAccountData)?;
            cursor.latest_finalized_checkpoint == Pubkey::default()
        };
        if first
            && args.pre
                != empty_state_opening_v2(context, hash_parts)
                    .map_err(|_| PortalError::InvalidAccountData)?
        {
            return Err(PortalError::CheckpointPreviousRootMismatch.into());
        }
        if args.post.ingress_count > 0 {
            let (expected, bump) =
                find_ingress_record_v2_pda(program_id, &context, args.post.ingress_count);
            if prefix.address() != &expected || !prefix.owned_by(program_id) {
                return Err(PortalError::InvalidPdaSeeds.into());
            }
            let record = IngressRecordV2::try_from_slice(&prefix.try_borrow()?)
                .map_err(|_| PortalError::InvalidAccountData)?;
            record
                .validate_next(
                    context,
                    args.post.ingress_count - 1,
                    record.previous_root,
                    hash_parts,
                )
                .map_err(|_| PortalError::InvalidAccountData)?;
            if record.bump != bump
                || record.root != args.post.ingress_root
                || record.l1_slot > Clock::get()?.slot
            {
                return Err(PortalError::InvalidAccountData.into());
            }
        } else if prefix.address() != &pinocchio_system::ID {
            return Err(PortalError::InvalidPdaSeeds.into());
        }
        let input = InputContextV2 {
            session_context: context,
            readonly_l1_root: args.readonly_l1_root,
            ingress_start: args.pre.ingress_count,
            ingress_start_root: args.pre.ingress_root,
            ingress_end: args.post.ingress_count,
            ingress_end_root: args.post.ingress_root,
        }
        .commitment(hash_parts)
        .map_err(|_| PortalError::InvalidAccountData)?;
        if input != args.proposal.readonly_l1_root {
            return Err(PortalError::InvalidAccountData.into());
        }
        let (expected, bump) =
            crate::find_checkpoint_binding_v2_pda(program_id, checkpoint.address());
        if binding.address() != &expected {
            return Err(PortalError::InvalidPdaSeeds.into());
        }
        let state = crate::CheckpointBindingV2 {
            discriminator: crate::CheckpointBindingV2::DISCRIMINATOR,
            checkpoint: checkpoint.address().to_bytes(),
            pre: args.pre,
            post: args.post,
            prefix_record: prefix.address().to_bytes(),
            input_context_root: input,
            bump,
        };
        let size = crate::account_size(&state);
        let rent = Rent::get()?.try_minimum_balance(size)?;
        let bump_bytes = [bump];
        let seeds = &[
            Seed::from(crate::CheckpointBindingV2::SEED_PREFIX),
            Seed::from(checkpoint.address().as_ref()),
            Seed::from(bump_bytes.as_ref()),
        ];
        initialize_pda_account(
            proposer,
            binding,
            rent,
            size as u64,
            program_id,
            Signer::from(seeds),
        )?;
        state
    };
    super::checkpoint::process_propose_checkpoint_versioned(
        program_id,
        &mut accounts[..5],
        args.proposal,
        true,
    )?;
    binding_state
        .serialize(&mut &mut accounts[5].try_borrow_mut()?[..])
        .map_err(|_| PortalError::InvalidAccountData)?;
    Ok(())
}

#[p_instruction(
    id = 35,
    accounts = [committer(signer, mut), session(mut), checkpoint(mut), cursor(mut), proposer(mut), binding],
    data = [er_slot: u64]
)]
pub fn process_commit_checkpoint_v2(
    program_id: &Pubkey,
    accounts: &mut [AccountInfo],
    args: crate::CommitCheckpoint,
) -> ProgramResult {
    let (mut frame, binding_state) = {
        let [_committer, session, checkpoint, _cursor, _proposer, binding] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        let frame = load_session_v2(program_id, session)?;
        let (expected, bump) =
            crate::find_checkpoint_binding_v2_pda(program_id, checkpoint.address());
        if binding.address() != &expected || !binding.owned_by(program_id) {
            return Err(PortalError::InvalidPdaSeeds.into());
        }
        let state = crate::CheckpointBindingV2::try_from_slice(&binding.try_borrow()?)
            .map_err(|_| PortalError::InvalidAccountData)?;
        let context = frame
            .identity(program_id, session.address())
            .commitment(hash_parts)
            .map_err(|_| PortalError::InvalidAccountData)?;
        let checkpoint_state = crate::Checkpoint::try_from_slice(&checkpoint.try_borrow()?)
            .map_err(|_| PortalError::InvalidAccountData)?;
        if state.discriminator != crate::CheckpointBindingV2::DISCRIMINATOR
            || state.bump != bump
            || state.checkpoint != checkpoint.address().to_bytes()
            || state.pre.session_context != context
            || state.post.session_context != context
            || state.pre.ingress_count != frame.consumed_ingress_count
            || state.pre.ingress_root != frame.consumed_ingress_root
            || state.post.ingress_count > frame.ingress_count
            || checkpoint_state.previous_state_root
                != state
                    .pre
                    .commitment(hash_parts)
                    .map_err(|_| PortalError::InvalidAccountData)?
            || checkpoint_state.new_state_root
                != state
                    .post
                    .commitment(hash_parts)
                    .map_err(|_| PortalError::InvalidAccountData)?
            || checkpoint_state.readonly_l1_root != state.input_context_root
        {
            return Err(PortalError::CheckpointPreviousRootMismatch.into());
        }
        (frame, state)
    };
    super::checkpoint::process_commit_checkpoint_versioned(
        program_id,
        &mut accounts[..5],
        args,
        true,
    )?;
    frame.consumed_ingress_count = binding_state.post.ingress_count;
    frame.consumed_ingress_root = binding_state.post.ingress_root;
    frame
        .serialize(&mut &mut accounts[1].try_borrow_mut()?[..])
        .map_err(|_| PortalError::InvalidAccountData)?;
    Ok(())
}
