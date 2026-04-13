#![no_std]

mod error;
mod instruction;
mod instructions;
mod pda;
mod state;

use pinocchio::{AccountView, Address, ProgramResult, error::ProgramError};
pub use {error::*, instruction::*, pda::*, state::*};

pinocchio::program_entrypoint!(process_instruction);
pinocchio::no_allocator!();
pinocchio::nostd_panic_handler!();

#[inline(never)]
fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let instruction = match borsh::from_slice(instruction_data) {
        Ok(inst) => inst,
        Err(_) => return Err(ProgramError::InvalidInstructionData),
    };

    match instruction {
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
    }
}
