use {
    super::*,
    northstar_transaction_proof::vrf_candidate::{
        BoundedVec, CandidateAccountInputV1, CandidateAccountStateV1, CandidateError,
        CandidateExecutionInputV1, CandidateExecutionOutputV1, CandidateExecutionStatusV1,
        CandidateFeatureV1, CandidateProfileV1, CandidateProgramV1, CandidateReplayEngine,
        CandidateReturnDataV1, CandidateUnsupported, admit_signed_transaction,
        candidate_log_commitment, candidate_programs_sha256,
    },
    solana_instruction_error::InstructionError,
    solana_message::{Message, MessageHeader, compiled_instruction::CompiledInstruction},
};

#[derive(Default)]
pub struct AgaveReplayEngine;

/// Hashes only deterministic interpreter configuration, not JIT fields or pointers.
/// These identities describe compiled behavior; profile/session binding is separate.
pub fn runtime_identities() -> ([u8; 32], [u8; 32], [u8; 32]) {
    let budget = ComputeBudget::new_with_defaults(false);
    let environment = solana_syscalls::create_program_runtime_environment(
        &feature_set(),
        &budget.to_budget(),
        false,
        false,
    )
    .expect("fixed runtime environment");
    let config = environment.get_config();
    let mut bytes = b"northstar.vrf-sbpf.vm-config.v1".to_vec();
    for value in [
        config.max_call_depth,
        config.stack_frame_size,
        config.instruction_meter_checkpoint_distance,
    ] {
        bytes.extend_from_slice(&(value as u64).to_le_bytes());
    }
    bytes.extend_from_slice(&[
        config.enable_address_translation as u8,
        config.enable_stack_frame_gaps as u8,
        config.enable_instruction_meter as u8,
        config.enable_register_tracing as u8,
        config.enable_symbol_and_section_labels as u8,
        config.reject_broken_elfs as u8,
        config.optimize_rodata as u8,
        config.aligned_memory_mapping as u8,
        *config.enabled_sbpf_versions.start() as u8,
        *config.enabled_sbpf_versions.end() as u8,
    ]);
    let vm = hash(&bytes).to_bytes();
    let mut bytes = b"northstar.vrf-sbpf.syscalls.v1".to_vec();
    let mut functions: Vec<_> = environment
        .get_function_registry()
        .iter()
        .map(|(key, (name, _))| (key, name.to_vec()))
        .collect();
    functions.sort();
    for (key, name) in functions {
        bytes.extend_from_slice(&key.to_le_bytes());
        bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&name);
    }
    let syscalls = hash(&bytes).to_bytes();
    // Debug is fixed by the pinned ComputeBudget implementation; all integer
    // fields use the same 64-bit layout on the native host and SP1 target.
    let budget =
        hash(format!("northstar.vrf-sbpf.compute-budget.v1:{budget:?}").as_bytes()).to_bytes();
    (vm, syscalls, budget)
}

pub fn pinned_programs() -> [CandidateProgramV1; 3] {
    let mut programs = PROGRAMS.map(|(key, digest)| {
        let mut elf_sha256 = [0; 32];
        for (pair, byte) in digest
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .zip(&mut elf_sha256)
        {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).expect("constant ASCII"), 16)
                .expect("constant hash");
        }
        CandidateProgramV1 {
            program_id: key.to_bytes(),
            programdata_id: Pubkey::find_program_address(
                &[key.as_ref()],
                &bpf_loader_upgradeable::id(),
            )
            .0
            .to_bytes(),
            loader_id: bpf_loader_upgradeable::id().to_bytes(),
            sbpf_version: 2,
            elf_sha256,
        }
    });
    programs.sort_by_key(|p| p.program_id);
    programs
}

/// The fixed development policy, derived from compiled behavior.
/// Revision fields name the pinned source base; the distinct guest key pins all
/// interpreter portability/binding changes on top of that base.
pub fn development_profile() -> CandidateProfileV1 {
    fn revision(value: &str) -> [u8; 20] {
        let mut result = [0; 20];
        for (pair, byte) in value.as_bytes().as_chunks::<2>().0.iter().zip(&mut result) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).expect("constant ASCII"), 16)
                .expect("constant revision");
        }
        result
    }
    let mut features = agave_feature_set::FeatureSet::default();
    features.activate(&agave_feature_set::curve25519_syscall_enabled::id(), 0);
    let mut inactive: Vec<_> = features.inactive().iter().map(|id| id.to_bytes()).collect();
    inactive.sort();
    let (vm, syscalls, budget) = runtime_identities();
    CandidateProfileV1 {
        version: 1,
        library_revision: revision("d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd"),
        runtime_revision: revision("34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52"),
        agave_revision: revision("34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52"),
        program_identities_sha256: candidate_programs_sha256(&pinned_programs())
            .expect("fixed programs"),
        active_features: BoundedVec::new(vec![CandidateFeatureV1 {
            id: agave_feature_set::curve25519_syscall_enabled::id().to_bytes(),
            activation_slot: 0,
        }])
        .expect("fixed active features"),
        inactive_features: BoundedVec::new(inactive).expect("fixed inactive features"),
        vm_config_sha256: vm,
        syscall_registry_sha256: syscalls,
        compute_budget_sha256: budget,
        lamports_per_signature: 0,
        checkpoint_steps: 1,
        max_compute_units: 1_400_000,
        heap_bytes: 32_768,
        max_processing_age: 900,
    }
}

impl CandidateReplayEngine for AgaveReplayEngine {
    fn execute(
        &mut self,
        input: &CandidateExecutionInputV1,
    ) -> Result<CandidateExecutionOutputV1, CandidateError> {
        let unsupported = || CandidateError::Unsupported(CandidateUnsupported::Engine);
        if input.profile != development_profile() {
            return Err(CandidateError::Unsupported(CandidateUnsupported::Runtime));
        }
        let curve =
            Pubkey::from_str_const("7rcw5UtqgDTBBv2EcynNfYckgdAaH1MAsCjKgXMkN7Ri").to_bytes();
        let (vm, syscalls, budget) = runtime_identities();
        if input.profile.active_features.len() != 1
            || input.profile.active_features[0].id != curve
            || input.profile.active_features[0].activation_slot > input.runtime.slot
            || input.profile.vm_config_sha256 != vm
            || input.profile.syscall_registry_sha256 != syscalls
            || input.profile.compute_budget_sha256 != budget
            || input.profile.lamports_per_signature != 0
            || input.profile.heap_bytes != 32_768
            || input.runtime.heap_bytes != 32_768
            || input.profile.max_processing_age != 900
            || input.programs.as_ref() != pinned_programs().as_slice()
        {
            return Err(unsupported());
        }
        let transaction = admit_signed_transaction(&input.transaction_bytes)?;
        let message = Message {
            header: MessageHeader {
                num_required_signatures: transaction.header[0],
                num_readonly_signed_accounts: transaction.header[1],
                num_readonly_unsigned_accounts: transaction.header[2],
            },
            account_keys: transaction
                .account_keys
                .iter()
                .copied()
                .map(Pubkey::new_from_array)
                .collect(),
            recent_blockhash: solana_hash::Hash::new_from_array(transaction.recent_blockhash),
            instructions: transaction
                .instructions
                .iter()
                .map(|instruction| CompiledInstruction {
                    program_id_index: instruction.program_id_index,
                    accounts: instruction.accounts.clone(),
                    data: instruction.data.clone(),
                })
                .collect(),
        };
        let reserved = reserved_keys();
        let message = SanitizedMessage::try_from_legacy_message(message, &reserved)
            .map_err(|_| CandidateError::Wire)?;
        // Reserved-key demotion must not change signed privileges for this profile.
        if message
            .account_keys()
            .iter()
            .enumerate()
            .any(|(index, _)| message.is_writable(index) != transaction.is_writable(index))
        {
            return Err(unsupported());
        }
        let accounts = input
            .accounts
            .iter()
            .map(|entry| {
                let state = entry
                    .state
                    .as_ref()
                    .map(|state| Account {
                        lamports: state.lamports,
                        owner: Pubkey::new_from_array(state.owner),
                        executable: state.executable,
                        rent_epoch: state.rent_epoch,
                        data: state.data.to_vec(),
                    })
                    .unwrap_or_default();
                (Pubkey::new_from_array(entry.key), state)
            })
            .collect();
        let execution = super::execute(&ExecutionInput {
            message,
            accounts,
            slot: input.runtime.slot,
            compute_unit_limit: input.runtime.compute_unit_limit,
        })
        .map_err(|_| unsupported())?;
        let status = match execution.status {
            Ok(()) => CandidateExecutionStatusV1::Success,
            Err(TransactionError::InstructionError(1, InstructionError::Custom(custom_error))) => {
                CandidateExecutionStatusV1::InstructionFailure {
                    instruction_index: 1,
                    error_kind: 1,
                    custom_error,
                }
            }
            // Version 1 has no encoding for arbitrary transaction/runtime failures.
            Err(_) => return Err(unsupported()),
        };
        let accounts = execution
            .resulting_accounts
            .into_iter()
            .map(|(key, state)| {
                Ok(CandidateAccountInputV1 {
                    key: key.to_bytes(),
                    state: if state.lamports == 0 {
                        None
                    } else {
                        Some(CandidateAccountStateV1 {
                            lamports: state.lamports,
                            owner: state.owner.to_bytes(),
                            executable: state.executable,
                            rent_epoch: state.rent_epoch,
                            data: BoundedVec::new(state.data)?,
                        })
                    },
                })
            })
            .collect::<Result<Vec<_>, CandidateError>>()?;
        Ok(CandidateExecutionOutputV1 {
            status,
            accounts: BoundedVec::new(accounts)?,
            executed_units: execution.executed_units,
            loaded_accounts_data_size: execution.loaded_accounts_data_size,
            transaction_fee: 0,
            prioritization_fee: 0,
            return_data: Some(CandidateReturnDataV1 {
                program_id: execution.return_data_program.to_bytes(),
                data: BoundedVec::new(execution.return_data)?,
            }),
            log_commitment: candidate_log_commitment(&execution.logs)?,
        })
    }
}
