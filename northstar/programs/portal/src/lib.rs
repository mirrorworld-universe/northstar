#![no_std]
#[cfg(not(target_os = "solana"))]
extern crate alloc;

mod error;
mod events;
mod instruction;
mod instructions;
mod pda;
mod state;

#[cfg(not(feature = "no-entrypoint"))]
use pinocchio::no_allocator;
use {
    borsh::BorshDeserialize,
    pinocchio::{error::ProgramError, AccountView as AccountInfo, ProgramResult},
};
pub use {error::*, events::*, instruction::*, pda::*, state::*};

#[cfg(all(
    feature = "test-verifier",
    target_os = "solana",
    not(northstar_allow_test_verifier_sbf)
))]
compile_error!(
    "Portal test-verifier dummy must not be enabled in SBF deploy builds. Set \
     NORTHSTAR_ALLOW_TEST_VERIFIER_SBF=1 only for local program-test fixtures."
);

pub const MAX_SETTLEMENT_CHUNK: usize = 700;
pub const MAX_SETTLEMENT_LAMPORT_ACCOUNTS: usize = 7;
pub const CHECKPOINT_PROPOSER_BOND_LAMPORTS: u64 = 1_000_000;
/// About one hour at Solana's target 400ms slot time.
pub const MAX_CHALLENGE_WINDOW_SLOTS: u64 = 9_000;
/// Five-minute response budget, capped by checkpoint's hard deadline.
pub const CHALLENGE_TURN_WINDOW_SLOTS: u64 = 750;
// Groth16-class v1 cap. Larger zkVM/STARK receipts need a future multi-account proof store.
pub const MAX_STEP_PROOF_BYTES: usize = 256;
pub const MAX_STEP_PROOF_CHUNK: usize = 128;

#[cfg(not(feature = "no-entrypoint"))]
no_allocator!();

#[cfg(all(target_os = "solana", not(feature = "no-entrypoint")))]
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
    program_id: &pinocchio::Address,
    accounts: &mut [AccountInfo],
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
        Ok((11, payload)) => deserialize_args(payload).and_then(|owner| {
            instructions::process_settle_account_owner(program_id, accounts, owner)
        }),
        Ok((12, payload)) => deserialize_args(payload).and_then(|lamports| {
            instructions::process_settle_account_lamports(program_id, accounts, lamports)
        }),
        Ok((13, payload)) => deserialize_args::<u64>(payload)
            .and_then(|lamports| instructions::process_start_withdrawal(accounts, lamports)),
        Ok((14, payload)) => deserialize_args(payload).and_then(|checkpoint| {
            instructions::process_propose_checkpoint(program_id, accounts, checkpoint)
        }),
        Ok((15, payload)) => deserialize_args(payload).and_then(|checkpoint| {
            instructions::process_commit_checkpoint(program_id, accounts, checkpoint)
        }),
        Ok((16, payload)) => deserialize_args(payload).and_then(|checkpoint| {
            instructions::process_cancel_checkpoint(program_id, accounts, checkpoint)
        }),
        Ok((17, payload)) => deserialize_args(payload).and_then(|checkpoint| {
            instructions::process_open_challenge(program_id, accounts, checkpoint)
        }),
        Ok((18, payload)) => deserialize_args(payload)
            .and_then(|proof| instructions::process_create_step_proof(program_id, accounts, proof)),
        Ok((19, payload)) => deserialize_args(payload)
            .and_then(|proof| instructions::process_write_step_proof(program_id, accounts, proof)),
        Ok((20, payload)) => deserialize_args(payload)
            .and_then(|proof| instructions::process_seal_step_proof(program_id, accounts, proof)),
        Ok((21, payload)) => deserialize_args(payload)
            .and_then(|proof| instructions::process_resolve_challenge(program_id, accounts, proof)),
        Ok((22, payload)) => deserialize_args(payload).and_then(|register| {
            instructions::process_register_session_bridge(program_id, accounts, register)
        }),
        Ok((23, payload)) => deserialize_args(payload).and_then(|response| {
            instructions::process_respond_challenge(program_id, accounts, response)
        }),
        Ok((24, payload)) => deserialize_args(payload).and_then(|bisect| {
            instructions::process_bisect_challenge(program_id, accounts, bisect)
        }),
        Ok((25, payload)) => deserialize_args(payload).and_then(|timeout| {
            instructions::process_timeout_challenge(program_id, accounts, timeout)
        }),
        #[cfg(feature = "zk-verifier-prototype")]
        Ok((26, payload)) => deserialize_args(payload)
            .and_then(|proof| instructions::process_verify_er_step_proof_v1(accounts, proof)),
        Ok((_, _)) | Err(_) => Err(ProgramError::InvalidInstructionData),
    }
}

// Sonic: Portal instructions need far fewer accounts than the transaction-wide
// maximum. Keeping the Pinocchio account scratch array at MAX_TX_ACCOUNTS burns
// the SBF stack before dispatch and causes live-validator Portal calls to exhaust
// compute units without logs. Sixteen accounts covers current Portal instructions,
// including batched Delegate calls, while keeping the stack scratch space small.
#[cfg_attr(not(feature = "no-entrypoint"), no_mangle)]
/// # Safety
/// `input` must be a valid pointer to a serialized Solana program input buffer.
pub unsafe extern "C" fn entrypoint(input: *mut u8) -> u64 {
    pinocchio::entrypoint::process_entrypoint::<16>(input, process_instruction)
}
