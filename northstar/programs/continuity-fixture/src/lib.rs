//! Test-only CPI fixture. It has no application authority policy and must not hold real assets.
use pinocchio::{
    cpi::invoke,
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};

#[cfg(target_os = "solana")]
pinocchio::no_allocator!();

/// # Safety
/// `input` must point to a valid serialized Solana program input buffer.
#[no_mangle]
pub unsafe extern "C" fn entrypoint(input: *mut u8) -> u64 {
    pinocchio::entrypoint::process_entrypoint::<8>(input, process_instruction)
}

fn process_instruction(
    _program: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    let [bridge, owner, account, registry, portal, destination, token, mint] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() != 11 || data[0] != 0 || !bridge.executable() || !owner.is_signer() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let metas = [
        InstructionAccount::readonly_signer(owner.address()),
        InstructionAccount::writable(account.address()),
        InstructionAccount::readonly(registry.address()),
        InstructionAccount::readonly(portal.address()),
        InstructionAccount::readonly(destination.address()),
        InstructionAccount::readonly(token.address()),
        InstructionAccount::readonly(mint.address()),
    ];
    invoke(
        &InstructionView {
            program_id: bridge.address(),
            accounts: &metas,
            data: &data[1..],
        },
        &[
            &*owner,
            &*account,
            &*registry,
            &*portal,
            &*destination,
            &*token,
            &*mint,
        ],
    )
}
