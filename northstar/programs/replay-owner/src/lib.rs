//! Test-only owner for fresh delegation and the supported one-byte replay transition.
//! This fixture has no application authority model and must not hold real assets.
use pinocchio::{
    address::Address,
    cpi::invoke,
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, ProgramResult,
};

pinocchio::entrypoint!(process_instruction);

fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    if data == [1] {
        let target = accounts
            .first_mut()
            .ok_or(ProgramError::NotEnoughAccountKeys)?;
        if !target.owned_by(program_id) || !target.is_writable() || target.data_len() != 8 {
            return Err(ProgramError::InvalidAccountData);
        }
        let mut target_data = target.try_borrow_mut()?;
        // The frozen replay profile requires exactly one memcpy syscall.
        #[cfg(target_os = "solana")]
        unsafe {
            pinocchio::syscalls::sol_memcpy_(target_data.as_mut_ptr(), [100u8].as_ptr(), 1);
        }
        #[cfg(not(target_os = "solana"))]
        {
            target_data[0] = 100;
        }
        return Ok(());
    }
    let [target, buffer, payer, session, record, owner_program, portal, system] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() != 9
        || data[0] != 0
        || !target.is_signer()
        || !payer.is_signer()
        || !target.owned_by(program_id)
        || !buffer.owned_by(program_id)
        || owner_program.address() != program_id
        || !owner_program.executable()
        || target.data_len() != 8
        || buffer.data_len() != 8
        || target.address() == buffer.address()
    {
        return Err(ProgramError::InvalidAccountData);
    }
    buffer
        .try_borrow_mut()?
        .copy_from_slice(&target.try_borrow()?);
    target.try_borrow_mut()?.fill(0);
    // All data borrows end before the owner changes and Portal receives the account.
    unsafe {
        target.assign(portal.address());
    }
    let metas = [
        InstructionAccount::writable_signer(payer.address()),
        InstructionAccount::readonly(system.address()),
        InstructionAccount::readonly(session.address()),
        InstructionAccount::writable_signer(target.address()),
        InstructionAccount::readonly(owner_program.address()),
        InstructionAccount::writable(record.address()),
        InstructionAccount::readonly(buffer.address()),
    ];
    let mut instruction_data = [0; 9];
    instruction_data[0] = 3;
    instruction_data[1..].copy_from_slice(&data[1..]);
    invoke(
        &InstructionView {
            program_id: portal.address(),
            accounts: &metas,
            data: &instruction_data,
        },
        &[
            &*payer,
            &*system,
            &*session,
            &*target,
            &*owner_program,
            &*record,
            &*buffer,
        ],
    )
}
