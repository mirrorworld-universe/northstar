#![no_std]

mod error;
mod instruction;
mod instructions;
mod pda;
mod state;

use pinocchio::{
    MAX_TX_ACCOUNTS, SUCCESS, account_info::AccountInfo, entrypoint::deserialize, no_allocator,
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

    let accounts: &[AccountInfo] =
        core::slice::from_raw_parts(accounts_arr.as_ptr() as *const AccountInfo, count);

    let instruction = match borsh::from_slice(instruction_data) {
        Ok(inst) => inst,
        Err(_) => return pinocchio::program_error::ProgramError::InvalidInstructionData.into(),
    };

    let result = match instruction {
        PortalInstruction::OpenSession(open_session) => {
            instructions::process_open_session(program_id, accounts, open_session)
        }
        PortalInstruction::CloseSession { grid_id } => {
            instructions::process_close_session(program_id, accounts, grid_id)
        }
        PortalInstruction::DepositFee { lamports } => {
            instructions::process_deposit_fee(program_id, accounts, lamports)
        }
        PortalInstruction::Delegate { grid_id } => {
            instructions::process_delegate(program_id, accounts, grid_id)
        }
        PortalInstruction::Undelegate => instructions::process_undelegate(program_id, accounts),
    };

    match result {
        Ok(()) => SUCCESS,
        Err(e) => e.into(),
    }
}
