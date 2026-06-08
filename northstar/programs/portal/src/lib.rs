#![no_std]

mod error;
mod instruction;
mod instructions;
mod pda;
mod state;

use {
    borsh::BorshDeserialize,
    pinocchio::{
        account_info::AccountInfo, entrypoint::deserialize, no_allocator,
        program_error::ProgramError, ProgramResult, SUCCESS,
    },
};
pub use {error::*, instruction::*, pda::*, state::*};

pub const MAX_SETTLEMENT_CHUNK: usize = 700;

no_allocator!();

#[cfg(target_os = "solana")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}

#[inline(never)]
fn deserialize_args<T: BorshDeserialize>(data: &[u8]) -> Result<T, ProgramError> {
    let mut data = data;
    T::deserialize(&mut data).map_err(|_| ProgramError::InvalidInstructionData)
}

#[inline(always)]
fn split_instruction(data: &[u8]) -> Result<(u8, &[u8]), ProgramError> {
    data.split_first()
        .map(|(tag, payload)| (*tag, payload))
        .ok_or(ProgramError::InvalidInstructionData)
}

#[inline(never)]
fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    // Sonic: Do not Borsh-deserialize the whole `PortalInstruction` enum here.
    // Large variants such as `WriteSettlementChunk` make the enum too large for
    // SBF's 4096-byte stack even when executing small instructions like
    // OpenSession. Dispatch on Borsh's one-byte enum tag, then deserialize only
    // the selected payload.
    match split_instruction(instruction_data) {
        Ok((0, payload)) => deserialize_args(payload).and_then(|open_session| {
            instructions::process_open_session(program_id, accounts, open_session)
        }),
        Ok((1, _)) => instructions::process_close_session(program_id, accounts),
        Ok((2, payload)) => deserialize_args::<u64>(payload)
            .and_then(|lamports| instructions::process_deposit_fee(program_id, accounts, lamports)),
        Ok((3, payload)) => deserialize_args::<u64>(payload)
            .and_then(|grid_id| instructions::process_delegate(program_id, accounts, grid_id)),
        Ok((4, _)) => instructions::process_undelegate(program_id, accounts),
        Ok((5, payload)) => deserialize_args(payload)
            .and_then(|begin| instructions::process_begin_settlement(program_id, accounts, begin)),
        Ok((6, payload)) => deserialize_args(payload).and_then(|chunk| {
            instructions::process_write_settlement_chunk(program_id, accounts, chunk)
        }),
        Ok((7, payload)) => deserialize_args(payload).and_then(|finish| {
            instructions::process_finish_settlement(program_id, accounts, finish)
        }),
        Ok((8, _)) => instructions::process_abort_settlement(program_id, accounts),
        Ok((9, payload)) => deserialize_args(payload).and_then(|settle| {
            instructions::process_settle_deposit_receipt(program_id, accounts, settle)
        }),
        Ok((10, _)) => instructions::process_undelegate_handoff(program_id, accounts),
        Ok((_, _)) | Err(_) => Err(ProgramError::InvalidInstructionData),
    }
}

// Sonic: Portal instructions need far fewer accounts than the transaction-wide
// maximum. Keeping the Pinocchio account scratch array at MAX_TX_ACCOUNTS burns
// the SBF stack before dispatch and causes live-validator Portal calls to exhaust
// compute units without logs. Sixteen accounts covers current Portal instructions,
// including batched Delegate calls, while keeping the stack scratch space small.
#[no_mangle]
/// # Safety
/// `input` must be a valid pointer to a serialized Solana program input buffer.
pub unsafe extern "C" fn entrypoint(input: *mut u8) -> u64 {
    const MAX_PORTAL_ACCOUNTS: usize = 16;
    const UNINIT: core::mem::MaybeUninit<AccountInfo> = core::mem::MaybeUninit::uninit();
    let mut accounts_arr = [UNINIT; MAX_PORTAL_ACCOUNTS];

    let (program_id, count, instruction_data) =
        deserialize::<MAX_PORTAL_ACCOUNTS>(input, &mut accounts_arr);

    let accounts: &[AccountInfo] =
        core::slice::from_raw_parts(accounts_arr.as_ptr() as *const AccountInfo, count);

    match process_instruction(&program_id, accounts, instruction_data) {
        Ok(()) => SUCCESS,
        Err(e) => e.into(),
    }
}
