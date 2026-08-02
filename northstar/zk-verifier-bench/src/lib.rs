use {
    groth16_solana::groth16::{Groth16Verifier, Groth16Verifyingkey},
    solana_account_info::AccountInfo,
    solana_program_error::{ProgramError, ProgramResult},
    solana_pubkey::Pubkey,
};

mod verifying_keys;

pub const PROGRAM_ID_BYTES: [u8; 32] = [
    0x9a, 0x3d, 0x72, 0x11, 0x50, 0xa9, 0x42, 0x10, 0xba, 0x7c, 0x3e, 0x81, 0xf5, 0x6b, 0x99, 0x02,
    0x4c, 0x13, 0x67, 0x80, 0xd9, 0x5a, 0x2b, 0x34, 0x76, 0x18, 0xc0, 0xde, 0x55, 0xaa, 0x04, 0x12,
];

solana_program_entrypoint::entrypoint_no_alloc!(process_instruction);

pub fn process_instruction(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (selector, payload) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    match selector {
        0 => verify::<8>(payload, &verifying_keys::VERIFYING_KEY_8),
        1 => verify::<12>(payload, &verifying_keys::VERIFYING_KEY_12),
        2 => verify::<16>(payload, &verifying_keys::VERIFYING_KEY_16),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn take<'a, const N: usize>(data: &mut &'a [u8]) -> Result<&'a [u8; N], ProgramError> {
    let (head, tail) = data
        .split_at_checked(N)
        .ok_or(ProgramError::InvalidInstructionData)?;
    *data = tail;
    head.try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)
}

fn verify<const N: usize>(
    mut payload: &[u8],
    verifying_key: &Groth16Verifyingkey,
) -> ProgramResult {
    let proof_a = take::<64>(&mut payload)?;
    let proof_b = take::<128>(&mut payload)?;
    let proof_c = take::<64>(&mut payload)?;
    let expected_input_bytes = N
        .checked_mul(32)
        .ok_or(ProgramError::InvalidInstructionData)?;
    if payload.len() != expected_input_bytes {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut public_inputs = [[0; 32]; N];
    for (output, input) in public_inputs.iter_mut().zip(payload.chunks_exact(32)) {
        output.copy_from_slice(input);
    }
    let mut verifier =
        Groth16Verifier::<N>::new(proof_a, proof_b, proof_c, &public_inputs, verifying_key)
            .map_err(|error| ProgramError::Custom(u32::from(error)))?;
    verifier
        .verify()
        .map_err(|error| ProgramError::Custom(u32::from(error)))
}
