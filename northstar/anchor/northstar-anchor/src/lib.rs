#![allow(deprecated)]

pub use northstar_anchor_macros::delegate;

pub mod cpi {
    use anchor_lang::solana_program::{
        account_info::AccountInfo,
        entrypoint::ProgramResult,
        instruction::{AccountMeta, Instruction},
        program::{invoke, invoke_signed},
        program_error::ProgramError,
        pubkey::Pubkey,
        rent::Rent,
        system_instruction, system_program,
        sysvar::Sysvar,
    };

    /// Portal seed for `Session` PDA.
    pub const SESSION_SEED: &[u8] = b"session";
    /// Portal seed for `DelegationRecord` PDA.
    pub const DELEGATION_RECORD_SEED: &[u8] = b"delegation";
    /// Owner-program seed for temporary delegation handoff buffers.
    pub const BUFFER_SEED: &[u8] = b"northstar-buffer";

    const PORTAL_DELEGATE_TAG: u8 = 3;
    const PORTAL_UNDELEGATE_HANDOFF_TAG: u8 = 10;

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct DelegateConfig {
        pub grid_id: u64,
    }

    pub struct DelegateAccounts<'info> {
        pub payer: AccountInfo<'info>,
        pub pda: AccountInfo<'info>,
        pub owner_program: AccountInfo<'info>,
        pub buffer: AccountInfo<'info>,
        pub delegation_record: AccountInfo<'info>,
        pub portal_program: AccountInfo<'info>,
        pub session: AccountInfo<'info>,
        pub system_program: AccountInfo<'info>,
    }

    pub struct UndelegateAccounts<'info> {
        pub authority: AccountInfo<'info>,
        pub pda: AccountInfo<'info>,
        pub owner_program: AccountInfo<'info>,
        pub buffer: AccountInfo<'info>,
        pub delegation_record: AccountInfo<'info>,
        pub portal_program: AccountInfo<'info>,
        pub session: AccountInfo<'info>,
        pub system_program: AccountInfo<'info>,
    }

    pub fn delegate_account<'info>(
        accounts: DelegateAccounts<'info>,
        pda_seeds: &[&[u8]],
        config: DelegateConfig,
    ) -> ProgramResult {
        require_system_program(&accounts.system_program)?;
        require_pda(&accounts.pda, accounts.owner_program.key, pda_seeds)?;
        require_portal_pdas(
            accounts.portal_program.key,
            &accounts.session,
            &accounts.delegation_record,
            accounts.pda.key,
        )?;

        if accounts.pda.owner != accounts.owner_program.key {
            return Err(ProgramError::InvalidAccountOwner);
        }

        let (buffer_seeds, buffer_bump) =
            buffer_seeds(accounts.pda.key, accounts.owner_program.key);
        let buffer_bump = [buffer_bump];
        let buffer_signer_seeds = seeds_with_bump(&buffer_seeds, &buffer_bump);
        let buffer_signers = [buffer_signer_seeds.as_slice()];

        create_pda(
            &accounts.buffer,
            accounts.owner_program.key,
            accounts.pda.data_len(),
            &buffer_signers,
            &accounts.system_program,
            &accounts.payer,
        )?;
        copy_account_data(&accounts.pda, &accounts.buffer)?;
        zero_account_data(&accounts.pda)?;

        let (_, pda_bump) = Pubkey::find_program_address(pda_seeds, accounts.owner_program.key);
        let pda_bump = [pda_bump];
        let pda_signer_seeds = seeds_with_bump(pda_seeds, &pda_bump);
        let pda_signers = [pda_signer_seeds.as_slice()];

        accounts.pda.assign(&system_program::id());
        invoke_signed(
            &system_instruction::assign(accounts.pda.key, accounts.portal_program.key),
            &[accounts.pda.clone(), accounts.system_program.clone()],
            &pda_signers,
        )?;

        let ix = Instruction {
            program_id: *accounts.portal_program.key,
            accounts: vec![
                AccountMeta::new(*accounts.payer.key, true),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new_readonly(*accounts.session.key, false),
                AccountMeta::new(*accounts.pda.key, true),
                AccountMeta::new_readonly(*accounts.owner_program.key, false),
                AccountMeta::new(*accounts.delegation_record.key, false),
                AccountMeta::new_readonly(*accounts.buffer.key, false),
            ],
            data: encode_delegate(config.grid_id),
        };
        invoke_signed(
            &ix,
            &[
                accounts.payer.clone(),
                accounts.system_program.clone(),
                accounts.session.clone(),
                accounts.pda.clone(),
                accounts.owner_program.clone(),
                accounts.delegation_record.clone(),
                accounts.buffer.clone(),
            ],
            &pda_signers,
        )?;

        close_pda_with_system_transfer(
            &accounts.buffer,
            &buffer_signers,
            &accounts.payer,
            &accounts.system_program,
        )?;
        Ok(())
    }

    pub fn undelegate_account<'info>(
        accounts: UndelegateAccounts<'info>,
        pda_seeds: &[&[u8]],
    ) -> ProgramResult {
        require_system_program(&accounts.system_program)?;
        require_pda(&accounts.pda, accounts.owner_program.key, pda_seeds)?;
        require_portal_pdas(
            accounts.portal_program.key,
            &accounts.session,
            &accounts.delegation_record,
            accounts.pda.key,
        )?;

        if accounts.pda.owner != accounts.portal_program.key {
            return Err(ProgramError::InvalidAccountOwner);
        }

        let (buffer_seeds, buffer_bump) =
            buffer_seeds(accounts.pda.key, accounts.owner_program.key);
        let buffer_bump = [buffer_bump];
        let buffer_signer_seeds = seeds_with_bump(&buffer_seeds, &buffer_bump);
        let buffer_signers = [buffer_signer_seeds.as_slice()];

        create_pda(
            &accounts.buffer,
            accounts.owner_program.key,
            accounts.pda.data_len(),
            &buffer_signers,
            &accounts.system_program,
            &accounts.authority,
        )?;
        copy_account_data(&accounts.pda, &accounts.buffer)?;

        let ix = Instruction {
            program_id: *accounts.portal_program.key,
            accounts: vec![
                AccountMeta::new(*accounts.authority.key, true),
                AccountMeta::new(*accounts.pda.key, false),
                AccountMeta::new_readonly(*accounts.owner_program.key, false),
                AccountMeta::new(*accounts.delegation_record.key, false),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new_readonly(*accounts.session.key, false),
            ],
            data: encode_undelegate_handoff(),
        };
        invoke(
            &ix,
            &[
                accounts.authority.clone(),
                accounts.pda.clone(),
                accounts.owner_program.clone(),
                accounts.delegation_record.clone(),
                accounts.system_program.clone(),
                accounts.session.clone(),
            ],
        )?;

        if accounts.pda.owner != accounts.owner_program.key {
            return Err(ProgramError::InvalidAccountOwner);
        }
        copy_account_data(&accounts.buffer, &accounts.pda)?;
        close_pda_with_system_transfer(
            &accounts.buffer,
            &buffer_signers,
            &accounts.authority,
            &accounts.system_program,
        )?;
        Ok(())
    }

    pub fn encode_delegate(grid_id: u64) -> Vec<u8> {
        let mut data = Vec::with_capacity(9);
        data.push(PORTAL_DELEGATE_TAG);
        data.extend_from_slice(&grid_id.to_le_bytes());
        data
    }

    pub fn encode_undelegate_handoff() -> Vec<u8> {
        vec![PORTAL_UNDELEGATE_HANDOFF_TAG]
    }

    pub fn session_pda(portal_program: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[SESSION_SEED], portal_program)
    }

    pub fn delegation_record_pda(
        portal_program: &Pubkey,
        delegated_account: &Pubkey,
    ) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[DELEGATION_RECORD_SEED, delegated_account.as_ref()],
            portal_program,
        )
    }

    pub fn buffer_pda(owner_program: &Pubkey, delegated_account: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[BUFFER_SEED, delegated_account.as_ref()], owner_program)
    }

    fn require_system_program(system_program: &AccountInfo<'_>) -> ProgramResult {
        if system_program.key != &system_program::id() {
            return Err(ProgramError::IncorrectProgramId);
        }
        Ok(())
    }

    fn require_pda(
        account: &AccountInfo<'_>,
        owner_program: &Pubkey,
        seeds: &[&[u8]],
    ) -> ProgramResult {
        let (expected, _) = Pubkey::find_program_address(seeds, owner_program);
        if account.key != &expected {
            return Err(ProgramError::InvalidSeeds);
        }
        Ok(())
    }

    fn require_portal_pdas(
        portal_program: &Pubkey,
        session: &AccountInfo<'_>,
        delegation_record: &AccountInfo<'_>,
        delegated_account: &Pubkey,
    ) -> ProgramResult {
        let (expected_session, _) = Pubkey::find_program_address(&[SESSION_SEED], portal_program);
        if session.key != &expected_session {
            return Err(ProgramError::InvalidSeeds);
        }
        let (expected_record, _) = Pubkey::find_program_address(
            &[DELEGATION_RECORD_SEED, delegated_account.as_ref()],
            portal_program,
        );
        if delegation_record.key != &expected_record {
            return Err(ProgramError::InvalidSeeds);
        }
        Ok(())
    }

    fn buffer_seeds<'a>(
        delegated_account: &'a Pubkey,
        owner_program: &Pubkey,
    ) -> ([&'a [u8]; 2], u8) {
        let seeds = [BUFFER_SEED, delegated_account.as_ref()];
        let (_, bump) = buffer_pda(owner_program, delegated_account);
        (seeds, bump)
    }

    fn seeds_with_bump<'a>(seeds: &[&'a [u8]], bump: &'a [u8]) -> Vec<&'a [u8]> {
        let mut out = seeds.to_vec();
        out.push(bump);
        out
    }

    fn create_pda<'info>(
        target: &AccountInfo<'info>,
        owner: &Pubkey,
        space: usize,
        signer_seeds: &[&[&[u8]]],
        system_program: &AccountInfo<'info>,
        payer: &AccountInfo<'info>,
    ) -> ProgramResult {
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(space);

        if target.lamports() == 0 {
            invoke_signed(
                &system_instruction::create_account(
                    payer.key,
                    target.key,
                    required_lamports,
                    space as u64,
                    owner,
                ),
                &[payer.clone(), target.clone(), system_program.clone()],
                signer_seeds,
            )?;
            return Ok(());
        }

        if target.lamports() < required_lamports {
            invoke(
                &system_instruction::transfer(
                    payer.key,
                    target.key,
                    required_lamports.saturating_sub(target.lamports()),
                ),
                &[payer.clone(), target.clone(), system_program.clone()],
            )?;
        }

        if target.data_len() != space {
            if target.owner != &system_program::id() {
                return Err(ProgramError::InvalidAccountOwner);
            }
            invoke_signed(
                &system_instruction::allocate(target.key, space as u64),
                &[target.clone(), system_program.clone()],
                signer_seeds,
            )?;
        }

        if target.owner != owner {
            if target.owner != &system_program::id() {
                return Err(ProgramError::InvalidAccountOwner);
            }
            invoke_signed(
                &system_instruction::assign(target.key, owner),
                &[target.clone(), system_program.clone()],
                signer_seeds,
            )?;
        }

        Ok(())
    }

    fn copy_account_data(src: &AccountInfo<'_>, dst: &AccountInfo<'_>) -> ProgramResult {
        if src.data_len() != dst.data_len() {
            return Err(ProgramError::AccountDataTooSmall);
        }
        let src_data = src.try_borrow_data()?;
        let mut dst_data = dst.try_borrow_mut_data()?;
        dst_data.copy_from_slice(&src_data);
        Ok(())
    }

    fn zero_account_data(account: &AccountInfo<'_>) -> ProgramResult {
        account.try_borrow_mut_data()?.fill(0);
        Ok(())
    }

    fn close_pda_with_system_transfer<'info>(
        target: &AccountInfo<'info>,
        signer_seeds: &[&[&[u8]]],
        destination: &AccountInfo<'info>,
        system_program: &AccountInfo<'info>,
    ) -> ProgramResult {
        target.realloc(0, false)?;
        target.assign(&system_program::id());
        let lamports = target.lamports();
        if lamports > 0 {
            invoke_signed(
                &system_instruction::transfer(target.key, destination.key, lamports),
                &[target.clone(), destination.clone(), system_program.clone()],
                signer_seeds,
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn portal_delegate_encoding_matches_portal_dispatch() {
            let data = encode_delegate(7);
            assert_eq!(data, [3, 7, 0, 0, 0, 0, 0, 0, 0]);
        }

        #[test]
        fn portal_undelegate_handoff_encoding_matches_portal_dispatch() {
            assert_eq!(encode_undelegate_handoff(), [10]);
        }

        #[test]
        fn pda_helpers_use_northstar_portal_seeds() {
            let portal_program = Pubkey::new_unique();
            let owner_program = Pubkey::new_unique();
            let delegated_account = Pubkey::new_unique();

            assert_eq!(
                session_pda(&portal_program),
                Pubkey::find_program_address(&[b"session"], &portal_program)
            );
            assert_eq!(
                delegation_record_pda(&portal_program, &delegated_account),
                Pubkey::find_program_address(
                    &[b"delegation", delegated_account.as_ref()],
                    &portal_program,
                )
            );
            assert_eq!(
                buffer_pda(&owner_program, &delegated_account),
                Pubkey::find_program_address(
                    &[b"northstar-buffer", delegated_account.as_ref()],
                    &owner_program,
                )
            );
        }
    }
}

pub use cpi::{
    BUFFER_SEED, DELEGATION_RECORD_SEED, SESSION_SEED, buffer_pda, delegation_record_pda,
    session_pda,
};
