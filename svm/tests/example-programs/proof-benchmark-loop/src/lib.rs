use {
    solana_account_info::{next_account_info, AccountInfo},
    solana_program_entrypoint::entrypoint,
    solana_program_error::{ProgramError, ProgramResult},
    solana_pubkey::Pubkey,
};

entrypoint!(process_instruction);

fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let [1, iterations @ ..] = data else {
        return Err(ProgramError::InvalidInstructionData);
    };
    let iterations = u32::from_le_bytes(
        iterations
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let mut accumulator = 0x9e37_79b9_7f4a_7c15_u64;
    for iteration in 0..iterations {
        let next = accumulator
            .rotate_left(7)
            .wrapping_mul(0x100_0000_01b3)
            .wrapping_add(u64::from(iteration));
        unsafe { core::ptr::write_volatile(&mut accumulator, next) };
    }
    unsafe { core::ptr::read_volatile(&accumulator) };

    let target = next_account_info(&mut accounts.iter())?;
    target.try_borrow_mut_data()?[0] = 100;
    Ok(())
}
