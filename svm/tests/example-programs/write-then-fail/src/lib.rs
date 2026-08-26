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
    _data: &[u8],
) -> ProgramResult {
    let target = next_account_info(&mut accounts.iter())?;
    target.try_borrow_mut_data()?[0] = 100;
    Err(ProgramError::InvalidArgument)
}
