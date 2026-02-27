#![no_std]

mod error;
mod instruction;
mod instructions;
mod pda;
mod state;

use pinocchio::{
    account_info::AccountInfo, entrypoint::deserialize, no_allocator, MAX_TX_ACCOUNTS, SUCCESS,
};
pub use {error::*, instruction::*, pda::*, state::*};

no_allocator!();

#[cfg(target_os = "solana")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}

#[no_mangle]
/// # Safety
/// `input` must be a valid pointer to a serialized Solana program input buffer.
pub unsafe extern "C" fn entrypoint(input: *mut u8) -> u64 {
    const UNINIT: core::mem::MaybeUninit<AccountInfo> = core::mem::MaybeUninit::uninit();
    let mut accounts_arr = [UNINIT; MAX_TX_ACCOUNTS];

    let (program_id, count, instruction_data) = deserialize(input, &mut accounts_arr);

    if instruction_data.is_empty() {
        return pinocchio::program_error::ProgramError::InvalidInstructionData.into();
    }

    let accounts = core::slice::from_raw_parts(accounts_arr.as_ptr() as *const AccountInfo, count);

    let discriminator = instruction_data[0];
    let instruction = match PortalInstruction::try_from(discriminator) {
        Ok(inst) => inst,
        Err(()) => return pinocchio::program_error::ProgramError::InvalidInstructionData.into(),
    };

    let result = match instruction {
        PortalInstruction::OpenSession => {
            instructions::process_open_session(program_id, accounts, instruction_data)
        }
        PortalInstruction::CloseSession => {
            instructions::process_close_session(program_id, accounts, instruction_data)
        }
        PortalInstruction::DepositFee => {
            instructions::process_deposit_fee(program_id, accounts, instruction_data)
        }
        PortalInstruction::Delegate => {
            instructions::process_delegate(program_id, accounts, instruction_data)
        }
        PortalInstruction::Undelegate => {
            instructions::process_undelegate(program_id, accounts, instruction_data)
        }
    };

    match result {
        Ok(()) => SUCCESS,
        Err(e) => e.into(),
    }
}
