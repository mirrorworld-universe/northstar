//! Experimental, bounded Agave interpreter replay. Not a generic SVM verifier.
//!
//! Signature, checkpoint and fee authentication belong to the candidate envelope.
//! Accounts supplied here are the execution prestate, after fee handling.

use {
    solana_account::{Account, AccountSharedData, ReadableAccount},
    solana_compute_budget::compute_budget::ComputeBudget,
    solana_message::SanitizedMessage,
    solana_program_runtime::{
        invoke_context::{EnvironmentConfig, InvokeContext},
        loaded_programs::{
            ProgramCacheForTxBatch, ProgramRuntimeEnvironment, ProgramRuntimeEnvironments,
        },
        program_cache_entry::ProgramCacheEntry,
        solana_sbpf::program::BuiltinFunctionDefinition,
        sysvar_cache::SysvarCache,
    },
    solana_pubkey::Pubkey,
    solana_sdk_ids::{bpf_loader_upgradeable, compute_budget, sysvar},
    solana_sha256_hasher::hash,
    solana_svm_callback::InvokeContextCallback,
    solana_svm_feature_set::SVMFeatureSet,
    solana_svm_log_collector::LogCollector,
    solana_svm_timings::ExecuteTimings,
    solana_svm_transaction::svm_message::SVMStaticMessage,
    solana_transaction_context::{
        DropOnBailOut,
        transaction::{ComputeMeter, ExecutionRecord, TransactionContext},
    },
    solana_transaction_error::TransactionError,
    std::sync::Arc,
};

pub const COORDINATOR: Pubkey =
    Pubkey::from_str_const("FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N");
pub const VERIFIER: Pubkey = Pubkey::from_str_const("FUBz1nvSSoHnMqFoK2CA3jF5ckpYPdF3ZT4SoLzDNzh1");
pub const CONSUMER: Pubkey = Pubkey::from_str_const("HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt");
pub const CANDIDATE_PROOF_KIND: u8 = 3;
pub const CANDIDATE_VERSION: u8 = 1;

const PROGRAMS: [(Pubkey, &str); 3] = [
    (
        COORDINATOR,
        "441d8f463ecfcc398d2151f1bb4a1e221cff8b5fff1d9f185db90de6f78b8cde",
    ),
    (
        VERIFIER,
        "225fc076dc79661768bf6a48bbac0d26596d00ad15a8bddcf99dc4a76ad4aa88",
    ),
    (
        CONSUMER,
        "b7662ad1e4ebca17f1a8f2e222f18fcb1dcacf1e8daaad7d709f67f92798c4a1",
    ),
];

pub struct ExecutionInput {
    pub message: SanitizedMessage,
    pub accounts: Vec<(Pubkey, Account)>,
    pub slot: u64,
    pub compute_unit_limit: u64,
}

#[derive(Debug)]
pub struct ExecutionOutput {
    pub status: Result<(), TransactionError>,
    pub executed_units: u64,
    pub loaded_accounts_data_size: u64,
    pub return_data_program: Pubkey,
    pub return_data: Vec<u8>,
    /// Transaction-atomic effects; instruction failure restores the execution prestate.
    pub resulting_accounts: Vec<(Pubkey, Account)>,
    pub logs: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReplayError {
    UnsupportedShape,
    MissingAccount(Pubkey),
    DuplicateAccount(Pubkey),
    InvalidProgram(Pubkey),
    InvalidSysvars,
    RuntimeSetup,
}

// Reuse the exact pinned SVM post-transaction checks without importing the batch processor.
#[path = "../../../svm/src/rent_calculator.rs"]
#[allow(dead_code)]
mod rent_calculator;
#[path = "../../../svm/src/transaction_account_state_info.rs"]
#[allow(dead_code)]
mod transaction_account_state_info;

mod candidate;
pub use candidate::{AgaveReplayEngine, development_profile, pinned_programs, runtime_identities};
#[cfg(feature = "cli")]
pub mod sidecar;

#[cfg(all(test, feature = "cli"))]
mod capture_tests;
#[cfg(test)]
mod tests;

struct BoundedCallback;
impl InvokeContextCallback for BoundedCallback {}

/// The fixed development feature profile; the envelope must authenticate exactly this profile.
pub fn feature_set() -> SVMFeatureSet {
    SVMFeatureSet::default()
}

fn reserved_keys() -> std::collections::HashSet<Pubkey> {
    let mut features = agave_feature_set::FeatureSet::default();
    features.activate(&agave_feature_set::curve25519_syscall_enabled::id(), 0);
    let mut reserved = agave_reserved_account_keys::ReservedAccountKeys::default();
    reserved.update_active_set(&features);
    reserved.active
}

fn account<'a>(input: &'a ExecutionInput, key: &Pubkey) -> Result<&'a Account, ReplayError> {
    input
        .accounts
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, a)| a)
        .ok_or(ReplayError::MissingAccount(*key))
}

fn validate_shape(input: &ExecutionInput) -> Result<(), ReplayError> {
    let SanitizedMessage::Legacy(_) = &input.message else {
        return Err(ReplayError::UnsupportedShape);
    };
    if input.accounts.len() > 64
        || input.message.account_keys().len() > 32
        || input
            .accounts
            .iter()
            .map(|(_, a)| a.data.len())
            .sum::<usize>()
            > 8 * 1024 * 1024
        || !(1..=1_400_000).contains(&input.compute_unit_limit)
    {
        return Err(ReplayError::UnsupportedShape);
    }
    for (index, (key, _)) in input.accounts.iter().enumerate() {
        if input.accounts[..index].iter().any(|(k, _)| k == key) {
            return Err(ReplayError::DuplicateAccount(*key));
        }
    }
    let reserved = reserved_keys();
    for (index, key) in input.message.account_keys().iter().enumerate() {
        account(input, key)?;
        if input.message.is_writable(index) && reserved.contains(key) {
            return Err(ReplayError::UnsupportedShape);
        }
    }
    // The instructions sysvar is generated by the transaction loader, not supplied as an oracle.
    if input
        .message
        .account_keys()
        .iter()
        .any(|k| k == &sysvar::instructions::id())
    {
        return Err(ReplayError::UnsupportedShape);
    }
    let instructions: Vec<_> = input.message.program_instructions_iter().collect();
    if instructions.len() != 2
        || instructions[0].0 != &compute_budget::id()
        || !instructions[0].1.accounts.is_empty()
        || instructions[0].1.data.len() != 5
        || instructions[0].1.data[0] != 2
        || u32::from_le_bytes(instructions[0].1.data[1..5].try_into().unwrap()) as u64
            != input.compute_unit_limit
    {
        return Err(ReplayError::UnsupportedShape);
    }
    let (program_id, instruction) = instructions[1];
    let discriminator = if program_id == &CONSUMER {
        hash(b"global:make_request")
    } else if program_id == &COORDINATOR {
        hash(b"global:fulfill")
    } else {
        return Err(ReplayError::UnsupportedShape);
    };
    if instruction.data.get(..8) != Some(&discriminator.as_ref()[..8]) {
        return Err(ReplayError::UnsupportedShape);
    }
    for (key, a) in &input.accounts {
        if a.executable
            && ![
                COORDINATOR,
                VERIFIER,
                CONSUMER,
                solana_sdk_ids::system_program::id(),
                compute_budget::id(),
                bpf_loader_upgradeable::id(),
            ]
            .contains(key)
        {
            return Err(ReplayError::UnsupportedShape);
        }
    }
    for key in [
        solana_sdk_ids::system_program::id(),
        compute_budget::id(),
        bpf_loader_upgradeable::id(),
    ] {
        let builtin = match account(input, &key) {
            Ok(value) => value,
            Err(_) if key == solana_sdk_ids::system_program::id() => continue,
            Err(error) => return Err(error),
        };
        if !builtin.executable
            || builtin.owner != solana_sdk_ids::native_loader::id()
            || builtin.lamports == 0
        {
            return Err(ReplayError::InvalidProgram(key));
        }
    }
    let payer = account(input, &input.message.account_keys()[0])?;
    if payer.lamports == 0
        || payer.owner != solana_sdk_ids::system_program::id()
        || !payer.data.is_empty()
        || payer.executable
    {
        return Err(ReplayError::UnsupportedShape);
    }
    Ok(())
}

fn pinned_elf<'a>(
    input: &'a ExecutionInput,
    key: &Pubkey,
    expected_hash: &str,
) -> Result<(&'a [u8], u64), ReplayError> {
    let invalid = || ReplayError::InvalidProgram(*key);
    let program = account(input, key)?;
    if !program.executable
        || program.owner != bpf_loader_upgradeable::id()
        || program.data.len() != 36
        || program.data[..4] != 2u32.to_le_bytes()
    {
        return Err(invalid());
    }
    let data_key = Pubkey::new_from_array(program.data[4..36].try_into().unwrap());
    if data_key != Pubkey::find_program_address(&[key.as_ref()], &bpf_loader_upgradeable::id()).0 {
        return Err(invalid());
    }
    let data = account(input, &data_key)?;
    if data.executable
        || data.owner != bpf_loader_upgradeable::id()
        || data.data.len() <= 45
        || data.data[..4] != 3u32.to_le_bytes()
        || data.data[12] > 1
    {
        return Err(invalid());
    }
    let deployment_slot = u64::from_le_bytes(data.data[4..12].try_into().unwrap());
    if deployment_slot >= input.slot {
        return Err(invalid());
    }
    let elf = &data.data[45..];
    if !matches!(
        solana_program_runtime::solana_sbpf::elf::get_sbpf_version(elf),
        Ok(solana_program_runtime::solana_sbpf::program::SBPFVersion::V2)
    ) {
        return Err(invalid());
    }
    let digest = hash(elf).to_bytes();
    const HEX: &[u8] = b"0123456789abcdef";
    if !digest
        .iter()
        .zip(expected_hash.as_bytes().as_chunks::<2>().0.iter())
        .all(|(byte, pair)| {
            pair[0] == HEX[(byte >> 4) as usize] && pair[1] == HEX[(byte & 15) as usize]
        })
    {
        return Err(invalid());
    }
    Ok((elf, deployment_slot))
}

/// Reexecute pinned request or fulfillment bytecode, including all nested CPI and curve syscalls.
/// This deliberately does not verify signatures or checkpoints; use only through an authenticated envelope.
pub fn execute(input: &ExecutionInput) -> Result<ExecutionOutput, ReplayError> {
    validate_shape(input)?;
    let features = feature_set();
    let mut budget = ComputeBudget::new_with_defaults(false);
    budget.compute_unit_limit = input.compute_unit_limit;
    let environment = solana_syscalls::create_program_runtime_environment(
        &features,
        &budget.to_budget(),
        false,
        false,
    )
    .map_err(|_| ReplayError::RuntimeSetup)?;
    let environments = ProgramRuntimeEnvironments::new(
        ProgramRuntimeEnvironment::clone(&environment),
        environment.clone(),
    );
    let mut program_cache = ProgramCacheForTxBatch::new(input.slot);
    program_cache.replenish(
        solana_sdk_ids::system_program::id(),
        Arc::new(ProgramCacheEntry::new_builtin(
            solana_system_program::system_processor::Entrypoint::register,
        )),
    );
    program_cache.replenish(
        bpf_loader_upgradeable::id(),
        Arc::new(ProgramCacheEntry::new_builtin(
            solana_bpf_loader_program::Entrypoint::register,
        )),
    );
    program_cache.replenish(
        compute_budget::id(),
        Arc::new(ProgramCacheEntry::new_builtin(
            solana_compute_budget_program::Entrypoint::register,
        )),
    );
    for (key, expected_hash) in PROGRAMS {
        let (elf, deployment_slot) = pinned_elf(input, &key, expected_hash)?;
        let entry = ProgramCacheEntry::load_interpreted(
            &bpf_loader_upgradeable::id(),
            environment.clone(),
            deployment_slot,
            elf,
        )
        .map_err(|_| ReplayError::InvalidProgram(key))?;
        program_cache.replenish(key, Arc::new(entry));
    }
    let mut sysvars = SysvarCache::default();
    sysvars.fill_missing_entries(|key, set| {
        if let Ok(a) = account(input, key)
            && a.lamports > 0
            && a.owner == sysvar::id()
        {
            set(&a.data);
        }
    });
    let rent = sysvars
        .get_rent()
        .map_err(|_| ReplayError::InvalidSysvars)?;
    if *rent != solana_rent::Rent::default() {
        return Err(ReplayError::InvalidSysvars);
    }
    if sysvars
        .get_clock()
        .map_err(|_| ReplayError::InvalidSysvars)?
        .slot
        != input.slot
        || sysvars.get_slot_hashes().is_err()
    {
        return Err(ReplayError::InvalidSysvars);
    }
    let mut loaded_accounts_data_size = 0u64;
    let mut counted_programdata = Vec::new();
    let transaction_accounts = input
        .message
        .account_keys()
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let mut data = account(input, key)?.clone();
            if data.lamports == 0 {
                data = Account {
                    rent_epoch: u64::MAX,
                    ..Account::default()
                };
            } else {
                loaded_accounts_data_size = loaded_accounts_data_size
                    .saturating_add(64)
                    .saturating_add(data.data.len() as u64);
                if data.owner == bpf_loader_upgradeable::id()
                    && data.data.len() == 36
                    && data.data[..4] == 2u32.to_le_bytes()
                {
                    let data_key = Pubkey::new_from_array(data.data[4..36].try_into().unwrap());
                    if !input.message.account_keys().iter().any(|k| k == &data_key)
                        && !counted_programdata.contains(&data_key)
                    {
                        let programdata = account(input, &data_key)?;
                        loaded_accounts_data_size = loaded_accounts_data_size
                            .saturating_add(64)
                            .saturating_add(programdata.data.len() as u64);
                        counted_programdata.push(data_key);
                    }
                }
                if input.message.is_writable(index)
                    && rent.is_exempt(data.lamports, data.data.len())
                {
                    data.rent_epoch = u64::MAX;
                }
            }
            Ok((*key, AccountSharedData::from(data)))
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;
    let execution_budget = budget.to_budget();
    let mut transaction_context = TransactionContext::new_with_feature_flags(
        transaction_accounts,
        (*rent).clone(),
        execution_budget.max_instruction_stack_depth,
        execution_budget.max_instruction_trace_length,
        input.message.num_instructions(),
        ComputeMeter::new(input.compute_unit_limit),
        DropOnBailOut::Disabled,
    );
    let pre_rent = transaction_account_state_info::TransactionAccountStateInfo::new_pre_exec(
        &transaction_context,
        &input.message,
        &rent,
        false,
    );
    let pre_lamports: u128 = input
        .message
        .account_keys()
        .iter()
        .map(|key| u128::from(account(input, key).expect("validated account").lamports))
        .sum();
    let config = EnvironmentConfig::new(
        *input.message.recent_blockhash(),
        0,
        false,
        &BoundedCallback,
        &features,
        &environments,
        &sysvars,
    );
    let log_collector = LogCollector::new_ref();
    let mut units = 0;
    let mut status = InvokeContext::new(
        &mut transaction_context,
        &mut program_cache,
        config,
        Some(log_collector.clone()),
        execution_budget,
        budget.to_cost(),
    )
    .process_message(&input.message, &mut ExecuteTimings::default(), &mut units)
    .map_err(|(index, error)| TransactionError::InstructionError(index, error));
    if status.is_ok() {
        let post_rent = transaction_account_state_info::TransactionAccountStateInfo::new_post_exec(
            &transaction_context,
            &input.message,
            &pre_rent,
            &rent,
            false,
        );
        status = transaction_account_state_info::verify_changes(
            &pre_rent,
            &post_rent,
            &transaction_context,
        );
    }
    let (return_data_program, return_data) = transaction_context.get_return_data();
    let (return_data_program, return_data) = (*return_data_program, return_data.to_vec());
    let record = ExecutionRecord::from(transaction_context);
    let post_lamports: u128 = record
        .accounts
        .iter()
        .map(|(_, a)| u128::from(a.lamports()))
        .sum();
    if status.is_ok() && pre_lamports != post_lamports {
        status = Err(TransactionError::UnbalancedTransaction);
    }
    let mut resulting_accounts = input.accounts.clone();
    if status.is_ok() {
        for (index, (key, data)) in record.accounts.into_iter().enumerate() {
            // Match SVM commit filtering, including the loader-normalized fee payer.
            if input.message.is_writable(index) && (index == 0 || record.touched_flags[index]) {
                let target = resulting_accounts
                    .iter_mut()
                    .find(|(k, _)| k == &key)
                    .ok_or(ReplayError::MissingAccount(key))?;
                target.1 = data.into();
            }
        }
    }
    let logs = log_collector.borrow().get_recorded_content().to_vec();
    Ok(ExecutionOutput {
        status,
        executed_units: units,
        loaded_accounts_data_size,
        return_data_program,
        return_data,
        resulting_accounts,
        logs,
    })
}
