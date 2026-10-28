//! Public conformance fixtures, not a VRF verifier or an application ABI.
use {
    solana_account_info::AccountInfo,
    solana_cpi::{invoke, invoke_signed},
    solana_curve25519::{
        ristretto::{
            add_ristretto, multiply_ristretto, multiscalar_multiply_ristretto, subtract_ristretto,
            validate_ristretto, PodRistrettoPoint,
        },
        scalar::PodScalar,
    },
    solana_instruction::{AccountMeta, Instruction},
    solana_program_entrypoint::{entrypoint, ProgramResult},
    solana_program_error::ProgramError,
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hash,
};

pub const AUTHORITY_SEED: &[u8] = b"vrf-runtime-probe";
pub const CLOCK_ID: Pubkey = solana_pubkey::pubkey!("SysvarC1ock11111111111111111111111111111111");
pub const SYSVAR_OWNER: Pubkey =
    solana_pubkey::pubkey!("Sysvar1111111111111111111111111111111111111");
pub const QUEUE_LEN: usize = 41;
pub const STATE_LEN: usize = 56;
pub const CALLBACK_FAILURE: u32 = 0xfeed;
pub const SLOT_NOT_LATER: u32 = 0x100;
pub const NOT_PENDING: u32 = 0x101;
pub const GENERATOR: PodRistrettoPoint = PodRistrettoPoint([
    226, 242, 174, 10, 106, 188, 78, 113, 168, 132, 169, 97, 197, 0, 81, 95, 88, 227, 11, 106, 165,
    130, 221, 141, 182, 166, 89, 69, 224, 141, 45, 118,
]);
pub const DOUBLE: PodRistrettoPoint = PodRistrettoPoint([
    106, 73, 50, 16, 247, 73, 156, 209, 127, 236, 181, 16, 174, 12, 234, 35, 161, 16, 232, 213,
    185, 1, 248, 172, 173, 211, 9, 92, 115, 163, 185, 25,
]);
pub const TRIPLE: PodRistrettoPoint = PodRistrettoPoint([
    148, 116, 31, 93, 93, 82, 117, 94, 206, 79, 35, 240, 68, 238, 39, 213, 209, 234, 30, 43, 209,
    150, 180, 98, 22, 107, 22, 21, 42, 157, 2, 89,
]);

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    match data.first() {
        Some(0) if data.len() == 1 && accounts.is_empty() => curve_fixture(),
        Some(1) if data.len() == 18 => fulfill_fixture(program_id, accounts, data),
        Some(2) if data.len() == 10 => callback_fixture(program_id, accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

pub fn scalar(value: u8) -> PodScalar {
    let mut bytes = [0; 32];
    bytes[0] = value;
    PodScalar(bytes)
}

pub fn curve_fixture() -> ProgramResult {
    let valid = validate_ristretto(&GENERATOR)
        && !validate_ristretto(&PodRistrettoPoint([255; 32]))
        && multiply_ristretto(&scalar(2), &GENERATOR) == Some(DOUBLE)
        && add_ristretto(&GENERATOR, &DOUBLE) == Some(TRIPLE)
        && subtract_ristretto(&TRIPLE, &GENERATOR) == Some(DOUBLE)
        && multiscalar_multiply_ristretto(&[scalar(1), scalar(2)], &[GENERATOR, GENERATOR])
            == Some(TRIPLE)
        && hash(b"northstar-vrf-runtime-probe-v1").to_bytes()
            == [
                145, 136, 239, 51, 166, 191, 231, 137, 82, 91, 186, 237, 56, 59, 39, 208, 247, 96,
                90, 187, 211, 1, 70, 50, 180, 55, 188, 213, 199, 94, 90, 221,
            ];
    if valid {
        Ok(())
    } else {
        Err(ProgramError::InvalidInstructionData)
    }
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

fn clock_slot(clock: &AccountInfo) -> Result<u64, ProgramError> {
    if clock.key != &CLOCK_ID || clock.owner != &SYSVAR_OWNER || clock.data_len() != 40 {
        return Err(ProgramError::UnsupportedSysvar);
    }
    Ok(read_u64(&clock.try_borrow_data()?[..8]))
}

fn fulfill_fixture(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [queue, state, authority, consumer, clock] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data[1] > 2 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if queue.owner != program_id || state.owner != consumer.key || !consumer.executable {
        return Err(ProgramError::IncorrectProgramId);
    }
    if !queue.is_writable || !state.is_writable || queue.key == state.key {
        return Err(ProgramError::InvalidAccountData);
    }
    let (expected, bump) =
        Pubkey::find_program_address(&[AUTHORITY_SEED, queue.key.as_ref()], program_id);
    if authority.key != &expected {
        return Err(ProgramError::InvalidSeeds);
    }
    {
        let mut bytes = queue.try_borrow_mut_data()?;
        if bytes.len() != QUEUE_LEN || &bytes[..32] != consumer.key.as_ref() {
            return Err(ProgramError::InvalidAccountData);
        }
        if bytes[40] != 1 {
            return Err(ProgramError::Custom(NOT_PENDING));
        }
        if bytes[32..40] != data[2..10] {
            return Err(ProgramError::InvalidInstructionData);
        }
        if clock_slot(clock)? <= read_u64(&data[10..18]) {
            return Err(ProgramError::Custom(SLOT_NOT_LATER));
        }
        bytes[40] = 0;
    }
    let mut callback_data = vec![2, u8::from(data[1] == 2)];
    callback_data.extend_from_slice(&data[2..10]);
    let instruction = Instruction::new_with_bytes(
        *consumer.key,
        &callback_data,
        vec![
            AccountMeta::new_readonly(*queue.key, false),
            AccountMeta::new(*state.key, false),
            AccountMeta::new_readonly(*authority.key, true),
            AccountMeta::new_readonly(*clock.key, false),
        ],
    );
    if data[1] == 1 {
        invoke(&instruction, accounts)
    } else {
        invoke_signed(
            &instruction,
            accounts,
            &[&[AUTHORITY_SEED, queue.key.as_ref(), &[bump]]],
        )
    }
}

fn callback_fixture(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [queue, state, authority, clock] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if state.owner != program_id || !state.is_writable {
        return Err(ProgramError::IncorrectProgramId);
    }
    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let mut bytes = state.try_borrow_mut_data()?;
    if bytes.len() != STATE_LEN || queue.data_len() != QUEUE_LEN {
        return Err(ProgramError::InvalidAccountData);
    }
    let parent = Pubkey::new_from_array(bytes[..32].try_into().unwrap());
    let (expected, _) =
        Pubkey::find_program_address(&[AUTHORITY_SEED, queue.key.as_ref()], &parent);
    if queue.owner != &parent || authority.key != &expected {
        return Err(ProgramError::InvalidSeeds);
    }
    let queue_bytes = queue.try_borrow_data()?;
    if &queue_bytes[..32] != program_id.as_ref() || queue_bytes[32..40] != data[2..10] {
        return Err(ProgramError::InvalidAccountData);
    }
    let count = read_u64(&bytes[40..48])
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    bytes[32..40].copy_from_slice(&data[2..10]);
    bytes[40..48].copy_from_slice(&count.to_le_bytes());
    bytes[48..56].copy_from_slice(&clock_slot(clock)?.to_le_bytes());
    if data[1] != 0 {
        // Deliberately fail after both programs wrote data, to check transaction atomicity.
        Err(ProgramError::Custom(CALLBACK_FAILURE))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_curve_known_answers() {
        curve_fixture().unwrap();
    }

    #[test]
    fn fixture_dispatch_rejects_unrecognized_shapes() {
        let id = Pubkey::new_from_array([81; 32]);
        for data in [vec![], vec![255], vec![0, 0], vec![1]] {
            assert_eq!(
                process_instruction(&id, &[], &data),
                Err(ProgramError::InvalidInstructionData)
            );
        }
    }
}
