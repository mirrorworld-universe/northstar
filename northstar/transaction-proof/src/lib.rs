pub mod commitment;
#[cfg(feature = "host")]
pub mod fixture;

use {
    ark_bn254::Fr,
    borsh::{BorshDeserialize, BorshSerialize},
    commitment::{
        bytes, fold, fr_to_bytes, list, CommitmentError, ACCOUNT_LIST_TAG, ACCOUNT_TAG,
        READONLY_TAG, RESULT_TAG, RUNTIME_TAG, SESSION_CONTEXT_TAG, SETTLEMENT_TAG,
        TRACE_SCHEMA_TAG, TRANSACTION_TAG, TX_EFFECT_TAG, VM_TABLE_TAG,
    },
    ed25519_dalek::{Signature, Verifier, VerifyingKey},
    northstar_zk_types::{
        ErStepPublicInputsV1, FrBytes, FullTransactionPublicInputsV1,
        ER_STEP_PROOF_KIND_FULL_TRANSACTION, ER_STEP_PROOF_VERSION_V1,
    },
    sha2::{Digest, Sha256},
    std::collections::BTreeMap,
};

pub const WITNESS_MAGIC_V1: [u8; 8] = *b"NSTXPF01";
pub const WITNESS_VERSION_V1: u16 = 1;
pub const TRACE_SCHEMA_VERSION_V1: u16 = 1;
pub const SBPF_VERSION_V0: u8 = 0;
pub const SOL_MEMCPY_KEY: u32 = 0x717c_c4a3;
pub const MM_PROGRAM_START: u64 = 0x1_0000_0000;
pub const MM_STACK_START: u64 = 0x2_0000_0000;
pub const MM_HEAP_START: u64 = 0x3_0000_0000;
pub const MM_INPUT_START: u64 = 0x4_0000_0000;

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct AccountWitnessV1 {
    pub key: [u8; 32],
    pub signer: bool,
    pub writable: bool,
    pub invoked: bool,
    pub lamports: u64,
    pub owner: [u8; 32],
    pub executable: bool,
    pub rent_epoch: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct ProgramWordV1 {
    pub pc: u64,
    pub instruction: [u8; 8],
    pub lddw_high: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct VmRowV1 {
    pub registers: [u64; 12],
    pub instruction: [u8; 8],
    pub syscall_key: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct RuntimeWitnessV1 {
    pub agave_revision: [u8; 20],
    pub northstar_revision: [u8; 20],
    pub feature_set_hash: [u8; 32],
    pub recent_blockhashes: Vec<[u8; 32]>,
    pub lamports_per_signature: u64,
    pub slot: u64,
    pub sbpf_version: u8,
    pub vm_config_hash: [u8; 32],
    pub syscall_registry_hash: [u8; 32],
    pub program_id: [u8; 32],
    pub programdata_id: [u8; 32],
    pub program_elf: Vec<u8>,
    pub program_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct ResultWitnessV1 {
    pub executed_success: bool,
    pub transaction_error: u32,
    pub instruction_error: u32,
    pub instruction_index: u32,
    pub custom_error: u32,
    pub executed_units: u64,
    pub loaded_accounts_data_size: u64,
    pub transaction_fee: u64,
    pub prioritization_fee: u64,
    pub return_data: Vec<u8>,
    pub log_commitment: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct ReplayWitnessV1 {
    pub magic: [u8; 8],
    pub version: u16,
    pub proof_kind: u8,
    pub proof_version: u8,
    pub trace_schema_version: u16,
    pub session_context: Vec<u8>,
    pub er_slot: u64,
    pub step_index: u64,
    pub transaction_bytes: Vec<u8>,
    pub message_bytes: Vec<u8>,
    pub signature: [u8; 64],
    pub signer: [u8; 32],
    pub recent_blockhash: [u8; 32],
    pub instruction_data: Vec<u8>,
    pub pre_accounts: Vec<AccountWitnessV1>,
    pub post_accounts: Vec<AccountWitnessV1>,
    pub rollback_accounts: Vec<AccountWitnessV1>,
    pub readonly_accounts: Vec<AccountWitnessV1>,
    pub runtime: RuntimeWitnessV1,
    pub event_tags: Vec<u8>,
    pub program_words: Vec<ProgramWordV1>,
    pub vm_rows: Vec<VmRowV1>,
    pub program_input_before: Vec<u8>,
    pub program_input_after: Vec<u8>,
    pub stack_after: Vec<u8>,
    pub heap_after: Vec<u8>,
    pub compute_units_before: u64,
    pub compute_units_after: u64,
    pub trace_hash: [u8; 32],
    pub result: ResultWitnessV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayError {
    Encoding,
    Magic,
    Version,
    Domain,
    Wire,
    Signature,
    Blockhash,
    Accounts,
    Fee,
    Program,
    Trace,
    Opcode,
    Register,
    Memory,
    Compute,
    Outcome,
    Commitment,
}

impl From<CommitmentError> for ReplayError {
    fn from(_: CommitmentError) -> Self {
        Self::Commitment
    }
}

pub fn encode_witness(witness: &ReplayWitnessV1) -> Result<Vec<u8>, ReplayError> {
    borsh::to_vec(witness).map_err(|_| ReplayError::Encoding)
}

pub fn decode_witness(bytes: &[u8]) -> Result<ReplayWitnessV1, ReplayError> {
    borsh::from_slice(bytes).map_err(|_| ReplayError::Encoding)
}

pub fn replay(witness: &ReplayWitnessV1) -> Result<ErStepPublicInputsV1, ReplayError> {
    validate_header(witness)?;
    validate_transaction(witness)?;
    validate_accounts_and_result(witness)?;
    validate_trace(witness)?;
    let public = derive_public_inputs(witness)?;
    FullTransactionPublicInputsV1::try_from(public).map_err(|_| ReplayError::Domain)?;
    Ok(public)
}

fn validate_header(witness: &ReplayWitnessV1) -> Result<(), ReplayError> {
    if witness.magic != WITNESS_MAGIC_V1 {
        return Err(ReplayError::Magic);
    }
    if witness.version != WITNESS_VERSION_V1
        || witness.proof_kind != ER_STEP_PROOF_KIND_FULL_TRANSACTION
        || witness.proof_version != ER_STEP_PROOF_VERSION_V1
        || witness.trace_schema_version != TRACE_SCHEMA_VERSION_V1
    {
        return Err(ReplayError::Version);
    }
    if witness.runtime.sbpf_version != SBPF_VERSION_V0 {
        return Err(ReplayError::Version);
    }
    Ok(())
}

fn validate_transaction(witness: &ReplayWitnessV1) -> Result<(), ReplayError> {
    let parsed = parse_legacy_transaction(&witness.transaction_bytes)?;
    if parsed.message != witness.message_bytes
        || parsed.signature != witness.signature
        || parsed.signer != witness.signer
        || parsed.recent_blockhash != witness.recent_blockhash
        || parsed.instruction_data != witness.instruction_data
        || parsed.account_keys.len() != 3
        || parsed.account_keys[0] != witness.signer
        || parsed.account_keys[1]
            != witness
                .pre_accounts
                .get(1)
                .ok_or(ReplayError::Accounts)?
                .key
        || parsed.account_keys[2] != witness.runtime.program_id
    {
        return Err(ReplayError::Wire);
    }
    let key = VerifyingKey::from_bytes(&witness.signer).map_err(|_| ReplayError::Signature)?;
    let signature = Signature::from_bytes(&witness.signature);
    key.verify(&witness.message_bytes, &signature)
        .map_err(|_| ReplayError::Signature)?;
    if !witness
        .runtime
        .recent_blockhashes
        .iter()
        .any(|hash| hash == &witness.recent_blockhash)
    {
        return Err(ReplayError::Blockhash);
    }
    Ok(())
}

fn validate_accounts_and_result(witness: &ReplayWitnessV1) -> Result<(), ReplayError> {
    if witness.pre_accounts.len() != 2
        || witness.post_accounts.len() != 2
        || !witness.rollback_accounts.is_empty()
        || witness.instruction_data != [1]
    {
        return Err(ReplayError::Accounts);
    }
    let pre_fee = &witness.pre_accounts[0];
    let pre_target = &witness.pre_accounts[1];
    let post_fee = &witness.post_accounts[0];
    let post_target = &witness.post_accounts[1];
    if !pre_fee.signer
        || !pre_fee.writable
        || pre_target.signer
        || !pre_target.writable
        || pre_target.owner != witness.runtime.program_id
        || pre_target.data.is_empty()
        || pre_fee.key != post_fee.key
        || pre_target.key != post_target.key
        || pre_fee.owner != post_fee.owner
        || pre_target.owner != post_target.owner
        || pre_fee.data != post_fee.data
        || pre_target.lamports != post_target.lamports
    {
        return Err(ReplayError::Accounts);
    }
    if pre_fee
        .lamports
        .checked_sub(witness.runtime.lamports_per_signature)
        != Some(post_fee.lamports)
        || witness.result.transaction_fee != witness.runtime.lamports_per_signature
        || witness.result.prioritization_fee != 0
    {
        return Err(ReplayError::Fee);
    }
    let mut expected_data = pre_target.data.clone();
    expected_data[0] = 100;
    if post_target.data != expected_data {
        return Err(ReplayError::Accounts);
    }
    if !witness.result.executed_success
        || witness.result.transaction_error != 0
        || witness.result.instruction_error != 0
        || witness.result.custom_error != 0
    {
        return Err(ReplayError::Outcome);
    }
    Ok(())
}

fn validate_trace(witness: &ReplayWitnessV1) -> Result<(), ReplayError> {
    if witness.vm_rows.is_empty()
        || witness.program_words.is_empty()
        || witness.event_tags.last() != Some(&6)
        || witness.compute_units_before <= witness.compute_units_after
        || witness.result.executed_units == 0
        || witness.trace_hash != trace_hash(witness)
    {
        return Err(ReplayError::Trace);
    }
    let elf_hash: [u8; 32] = Sha256::digest(&witness.runtime.program_elf).into();
    if elf_hash == [0; 32] || witness.runtime.program_hash == [0; 32] {
        return Err(ReplayError::Program);
    }
    let words = witness
        .program_words
        .iter()
        .map(|word| (word.pc, word))
        .collect::<BTreeMap<_, _>>();
    if words.len() != witness.program_words.len() {
        return Err(ReplayError::Program);
    }
    let syscall_count = witness
        .vm_rows
        .iter()
        .filter(|row| row.syscall_key != 0)
        .count();
    if syscall_count != 1
        || witness
            .vm_rows
            .iter()
            .any(|row| row.syscall_key != 0 && row.syscall_key != SOL_MEMCPY_KEY)
    {
        return Err(ReplayError::Trace);
    }
    for (index, row) in witness.vm_rows.iter().enumerate() {
        let word = words.get(&row.registers[11]).ok_or(ReplayError::Program)?;
        if word.instruction != row.instruction {
            return Err(ReplayError::Program);
        }
        if let Some(next) = index
            .checked_add(1)
            .and_then(|next_index| witness.vm_rows.get(next_index))
        {
            validate_row(row, next, word.lddw_high)?;
        }
    }
    Ok(())
}

fn trace_hash(witness: &ReplayWitnessV1) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(&witness.event_tags);
    hasher.update(&witness.runtime.program_elf);
    hasher.update(witness.runtime.program_hash);
    for word in &witness.program_words {
        hasher.update(word.pc.to_le_bytes());
        hasher.update(word.instruction);
        hasher.update(word.lddw_high.to_le_bytes());
    }
    for row in &witness.vm_rows {
        for register in row.registers {
            hasher.update(register.to_le_bytes());
        }
        hasher.update(row.instruction);
        hasher.update(row.syscall_key.to_le_bytes());
    }
    for memory in [
        &witness.program_input_before,
        &witness.program_input_after,
        &witness.stack_after,
        &witness.heap_after,
    ] {
        hasher.update((memory.len() as u64).to_le_bytes());
        hasher.update(memory);
    }
    hasher.update(witness.compute_units_before.to_le_bytes());
    hasher.update(witness.compute_units_after.to_le_bytes());
    hasher.finalize().into()
}

pub fn set_trace_hash(witness: &mut ReplayWitnessV1) {
    witness.trace_hash = trace_hash(witness);
}

fn validate_row(row: &VmRowV1, next: &VmRowV1, lddw_high: u32) -> Result<(), ReplayError> {
    let instruction = decode_instruction(row.instruction);
    if instruction.dst >= 11 || instruction.src >= 11 {
        return Err(ReplayError::Opcode);
    }
    let class = instruction.opcode & 0x07;
    let operation = instruction.opcode & 0xf0;
    let source_register = instruction.opcode & 0x08 != 0;
    let pc = row.registers[11];
    let fallthrough = pc.checked_add(1).ok_or(ReplayError::Register)?;
    match class {
        0 if instruction.opcode == 0x18 => {
            let value = u64::from(instruction.imm as u32) | (u64::from(lddw_high) << 32);
            let next_pc = pc.checked_add(2).ok_or(ReplayError::Register)?;
            validate_register_delta(row, next, instruction.dst, value, next_pc)?;
        }
        4 | 7 => {
            let left = row.registers[instruction.dst];
            let right = if source_register {
                row.registers[instruction.src]
            } else {
                instruction.imm as i64 as u64
            };
            let value = alu(operation, class == 4, left, right)?;
            validate_register_delta(row, next, instruction.dst, value, fallthrough)?;
        }
        5 | 6 => match operation {
            0x80 | 0x90 => {
                if next.registers[11] == pc {
                    return Err(ReplayError::Register);
                }
            }
            _ => {
                let left = row.registers[instruction.dst];
                let right = if source_register {
                    row.registers[instruction.src]
                } else {
                    instruction.imm as i64 as u64
                };
                let taken = jump(operation, class == 6, left, right)?;
                let target = if taken {
                    (fallthrough as i64)
                        .checked_add(i64::from(instruction.offset))
                        .and_then(|value| u64::try_from(value).ok())
                        .ok_or(ReplayError::Register)?
                } else {
                    fallthrough
                };
                if next.registers[11] != target || row.registers[..11] != next.registers[..11] {
                    return Err(ReplayError::Register);
                }
            }
        },
        1..=3 => {
            if next.registers[11] != fallthrough {
                return Err(ReplayError::Register);
            }
        }
        _ => return Err(ReplayError::Opcode),
    }
    Ok(())
}

fn validate_register_delta(
    row: &VmRowV1,
    next: &VmRowV1,
    destination: usize,
    value: u64,
    next_pc: u64,
) -> Result<(), ReplayError> {
    if next.registers[11] != next_pc || next.registers[destination] != value {
        return Err(ReplayError::Register);
    }
    for index in 0..11 {
        if index != destination && row.registers[index] != next.registers[index] {
            return Err(ReplayError::Register);
        }
    }
    Ok(())
}

fn alu(operation: u8, is_32: bool, left: u64, right: u64) -> Result<u64, ReplayError> {
    let value = match operation {
        0x00 => left.wrapping_add(right),
        0x10 => left.wrapping_sub(right),
        0x20 => left.wrapping_mul(right),
        0x30 => left.checked_div(right).ok_or(ReplayError::Opcode)?,
        0x40 => left | right,
        0x50 => left & right,
        0x60 => left.wrapping_shl((right & if is_32 { 31 } else { 63 }) as u32),
        0x70 => left.wrapping_shr((right & if is_32 { 31 } else { 63 }) as u32),
        0x80 => left.wrapping_neg(),
        0x90 => left.checked_rem(right).ok_or(ReplayError::Opcode)?,
        0xa0 => left ^ right,
        0xb0 => right,
        0xc0 => ((left as i64) >> (right & if is_32 { 31 } else { 63 })) as u64,
        _ => return Err(ReplayError::Opcode),
    };
    Ok(if is_32 {
        u64::from(value as u32)
    } else {
        value
    })
}

fn jump(operation: u8, is_32: bool, left: u64, right: u64) -> Result<bool, ReplayError> {
    let (left, right) = if is_32 {
        (u64::from(left as u32), u64::from(right as u32))
    } else {
        (left, right)
    };
    Ok(match operation {
        0x00 => true,
        0x10 => left == right,
        0x20 => left > right,
        0x30 => left >= right,
        0x40 => left & right != 0,
        0x50 => left != right,
        0x60 => (left as i64) > (right as i64),
        0x70 => (left as i64) >= (right as i64),
        0xa0 => left < right,
        0xb0 => left <= right,
        0xc0 => (left as i64) < (right as i64),
        0xd0 => (left as i64) <= (right as i64),
        _ => return Err(ReplayError::Opcode),
    })
}

struct DecodedInstruction {
    opcode: u8,
    dst: usize,
    src: usize,
    offset: i16,
    imm: i32,
}

fn decode_instruction(bytes: [u8; 8]) -> DecodedInstruction {
    DecodedInstruction {
        opcode: bytes[0],
        dst: usize::from(bytes[1] & 0x0f),
        src: usize::from(bytes[1] >> 4),
        offset: i16::from_le_bytes([bytes[2], bytes[3]]),
        imm: i32::from_le_bytes(bytes[4..8].try_into().expect("fixed slice")),
    }
}

pub fn derive_public_inputs(
    witness: &ReplayWitnessV1,
) -> Result<ErStepPublicInputsV1, ReplayError> {
    let transaction_commitment = transaction_commitment(witness)?;
    let runtime_commitment = runtime_commitment(witness)?;
    let result_commitment = result_commitment(witness)?;
    let pre_state_root = account_list_commitment(ACCOUNT_LIST_TAG, &witness.pre_accounts)?;
    let post_state_root = account_list_commitment(ACCOUNT_LIST_TAG, &witness.post_accounts)?;
    let readonly_l1_root = account_list_commitment(READONLY_TAG, &witness.readonly_accounts)?;
    let settlement_effect_root = account_list_commitment(SETTLEMENT_TAG, &witness.post_accounts)?;
    let trace_schema = fold(
        TRACE_SCHEMA_TAG,
        &[
            Fr::from(u64::from(witness.trace_schema_version)),
            vm_table_commitment(witness)?,
        ],
    )?;
    let tx_effect_root = fold(
        TX_EFFECT_TAG,
        &[
            transaction_commitment,
            runtime_commitment,
            result_commitment,
            trace_schema,
            settlement_effect_root,
        ],
    )?;
    Ok(ErStepPublicInputsV1 {
        domain: FrBytes::er_step_domain_v1(
            ER_STEP_PROOF_KIND_FULL_TRANSACTION,
            ER_STEP_PROOF_VERSION_V1,
        ),
        session_context: fr_to_bytes(bytes(SESSION_CONTEXT_TAG, &witness.session_context)?),
        slot_step: FrBytes::from_u64_pair(witness.er_slot, witness.step_index),
        pre_state_root: fr_to_bytes(pre_state_root),
        post_state_root: fr_to_bytes(post_state_root),
        tx_effect_root: fr_to_bytes(tx_effect_root),
        readonly_l1_root: fr_to_bytes(readonly_l1_root),
        settlement_effect_root: fr_to_bytes(settlement_effect_root),
    })
}

pub fn public_inputs_bytes(public: ErStepPublicInputsV1) -> [u8; 256] {
    let mut bytes = [0; 256];
    for (index, value) in public.to_array().iter().enumerate() {
        let start = index.checked_mul(32).expect("eight public inputs fit");
        let end = start.checked_add(32).expect("eight public inputs fit");
        bytes[start..end].copy_from_slice(value);
    }
    bytes
}

fn transaction_commitment(witness: &ReplayWitnessV1) -> Result<Fr, ReplayError> {
    fold(
        TRANSACTION_TAG,
        &[
            bytes(1, &witness.transaction_bytes)?,
            bytes(2, &witness.signature)?,
            bytes(3, &Sha256::digest(&witness.message_bytes))?,
            bytes(4, &witness.signer)?,
            bytes(5, &witness.recent_blockhash)?,
            bytes(6, &witness.instruction_data)?,
        ],
    )
    .map_err(Into::into)
}

fn runtime_commitment(witness: &ReplayWitnessV1) -> Result<Fr, ReplayError> {
    fold(
        RUNTIME_TAG,
        &[
            Fr::from(u64::from(witness.proof_version)),
            Fr::from(u64::from(witness.trace_schema_version)),
            bytes(1, &witness.runtime.agave_revision)?,
            bytes(2, &witness.runtime.northstar_revision)?,
            bytes(3, &witness.runtime.feature_set_hash)?,
            Fr::from(witness.runtime.lamports_per_signature),
            Fr::from(witness.runtime.slot),
            Fr::from(u64::from(witness.runtime.sbpf_version)),
            bytes(4, &witness.runtime.vm_config_hash)?,
            bytes(5, &witness.runtime.syscall_registry_hash)?,
            bytes(6, &witness.runtime.program_id)?,
            bytes(7, &witness.runtime.programdata_id)?,
            bytes(8, &Sha256::digest(&witness.runtime.program_elf))?,
            bytes(9, &witness.runtime.program_hash)?,
        ],
    )
    .map_err(Into::into)
}

fn result_commitment(witness: &ReplayWitnessV1) -> Result<Fr, ReplayError> {
    fold(
        RESULT_TAG,
        &[
            Fr::from(u64::from(witness.result.executed_success)),
            Fr::from(u64::from(witness.result.transaction_error)),
            Fr::from(u64::from(witness.result.instruction_error)),
            Fr::from(u64::from(witness.result.instruction_index)),
            Fr::from(u64::from(witness.result.custom_error)),
            Fr::from(witness.result.executed_units),
            Fr::from(witness.result.loaded_accounts_data_size),
            Fr::from(witness.result.transaction_fee),
            Fr::from(witness.result.prioritization_fee),
            bytes(1, &witness.result.return_data)?,
            bytes(2, &witness.result.log_commitment)?,
        ],
    )
    .map_err(Into::into)
}

fn account_list_commitment(tag: u64, accounts: &[AccountWitnessV1]) -> Result<Fr, ReplayError> {
    let commitments = accounts
        .iter()
        .map(account_commitment)
        .collect::<Result<Vec<_>, _>>()?;
    list(tag, &commitments).map_err(Into::into)
}

fn account_commitment(account: &AccountWitnessV1) -> Result<Fr, ReplayError> {
    fold(
        ACCOUNT_TAG,
        &[
            bytes(1, &account.key)?,
            Fr::from(u64::from(account.signer)),
            Fr::from(u64::from(account.writable)),
            Fr::from(u64::from(account.invoked)),
            Fr::from(account.lamports),
            bytes(2, &account.owner)?,
            Fr::from(u64::from(account.executable)),
            Fr::from(account.rent_epoch),
            bytes(3, &account.data)?,
        ],
    )
    .map_err(Into::into)
}

fn vm_table_commitment(witness: &ReplayWitnessV1) -> Result<Fr, ReplayError> {
    let mut hasher = Sha256::new();
    hasher.update((witness.vm_rows.len() as u64).to_le_bytes());
    for row in &witness.vm_rows {
        for register in row.registers {
            hasher.update(register.to_le_bytes());
        }
        hasher.update(row.instruction);
        hasher.update(row.syscall_key.to_le_bytes());
    }
    bytes(VM_TABLE_TAG, &hasher.finalize()).map_err(Into::into)
}

struct ParsedLegacyTransaction<'a> {
    signature: [u8; 64],
    message: Vec<u8>,
    signer: [u8; 32],
    recent_blockhash: [u8; 32],
    account_keys: Vec<[u8; 32]>,
    instruction_data: Vec<u8>,
    _wire: &'a [u8],
}

fn parse_legacy_transaction(bytes: &[u8]) -> Result<ParsedLegacyTransaction<'_>, ReplayError> {
    let mut cursor = 0;
    if read_short_vec(bytes, &mut cursor)? != 1 {
        return Err(ReplayError::Wire);
    }
    let signature = take_array(bytes, &mut cursor)?;
    let message_start = cursor;
    let header: [u8; 3] = take_array(bytes, &mut cursor)?;
    if header != [1, 0, 1] {
        return Err(ReplayError::Wire);
    }
    let key_count = read_short_vec(bytes, &mut cursor)?;
    if key_count != 3 {
        return Err(ReplayError::Wire);
    }
    let mut account_keys = Vec::with_capacity(key_count);
    for _ in 0..key_count {
        account_keys.push(take_array(bytes, &mut cursor)?);
    }
    let recent_blockhash = take_array(bytes, &mut cursor)?;
    if read_short_vec(bytes, &mut cursor)? != 1 {
        return Err(ReplayError::Wire);
    }
    let program_index = take_byte(bytes, &mut cursor)?;
    let account_count = read_short_vec(bytes, &mut cursor)?;
    let instruction_accounts = take(bytes, &mut cursor, account_count)?;
    let data_len = read_short_vec(bytes, &mut cursor)?;
    let instruction_data = take(bytes, &mut cursor, data_len)?.to_vec();
    if program_index != 2 || instruction_accounts != [1] || cursor != bytes.len() {
        return Err(ReplayError::Wire);
    }
    Ok(ParsedLegacyTransaction {
        signature,
        message: bytes[message_start..].to_vec(),
        signer: account_keys[0],
        recent_blockhash,
        account_keys,
        instruction_data,
        _wire: bytes,
    })
}

fn read_short_vec(bytes: &[u8], cursor: &mut usize) -> Result<usize, ReplayError> {
    let mut value = 0usize;
    for shift in [0, 7, 14] {
        let byte = take_byte(bytes, cursor)?;
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(ReplayError::Wire)
}

fn take_byte(bytes: &[u8], cursor: &mut usize) -> Result<u8, ReplayError> {
    let byte = *bytes.get(*cursor).ok_or(ReplayError::Wire)?;
    *cursor = cursor.checked_add(1).ok_or(ReplayError::Wire)?;
    Ok(byte)
}

fn take<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8], ReplayError> {
    let end = cursor.checked_add(len).ok_or(ReplayError::Wire)?;
    let value = bytes.get(*cursor..end).ok_or(ReplayError::Wire)?;
    *cursor = end;
    Ok(value)
}

fn take_array<const N: usize>(bytes: &[u8], cursor: &mut usize) -> Result<[u8; N], ReplayError> {
    take(bytes, cursor, N)?
        .try_into()
        .map_err(|_| ReplayError::Wire)
}
