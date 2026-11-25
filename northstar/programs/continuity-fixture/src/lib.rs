//! Test-only CPI fixture. It has no application authority policy and must not hold real assets.
use pinocchio::{
    cpi::{invoke, invoke_signed, Seed, Signer},
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
    pinocchio::entrypoint::process_entrypoint::<9>(input, process_instruction)
}

fn process_instruction(
    _program: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    let [bridge, owner, account, registry, portal, destination, token, mint, rest @ ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !bridge.executable() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let (payload, controller) = match (data, rest) {
        ([0, payload @ ..], []) if owner.is_signer() && payload.len() == 10 => (payload, None),
        ([1, bump, payload @ ..], [controller])
            if controller.is_signer() && payload.len() == 10 && payload[0] == 7 =>
        {
            (payload, Some((controller.address(), bump)))
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    };
    let metas = [
        InstructionAccount::readonly_signer(owner.address()),
        InstructionAccount::writable(account.address()),
        InstructionAccount::readonly(registry.address()),
        InstructionAccount::readonly(portal.address()),
        InstructionAccount::readonly(destination.address()),
        InstructionAccount::readonly(token.address()),
        InstructionAccount::readonly(mint.address()),
    ];
    let ix = InstructionView {
        program_id: bridge.address(),
        accounts: &metas,
        data: payload,
    };
    let infos = [
        &*owner,
        &*account,
        &*registry,
        &*portal,
        &*destination,
        &*token,
        &*mint,
    ];
    if let Some((authority, bump)) = controller {
        let seeds = [
            Seed::from(b"audit-owner".as_slice()),
            Seed::from(authority.as_ref()),
            Seed::from(core::slice::from_ref(bump)),
        ];
        invoke_signed(&ix, &infos, &[Signer::from(&seeds)])
    } else {
        invoke(&ix, &infos)
    }
}
