use {
    super::txn::BankTxnProcessingResult,
    northstar_zk_types::{
        ER_STEP_PROOF_KIND_FULL_TRANSACTION, ER_STEP_PROOF_VERSION_V1, ErStepPublicInputsV1,
        FrBytes,
        trace::{
            AccountPhaseV1, AccountStateTraceV1, InstructionBoundaryV1, MemoryDeltaV1,
            MemoryRegionV1, MemoryTraceV1, ProcessorStageV1, StageOutcomeV1, TraceEventV1,
            TraceHeaderV1, TransactionOutcomeV1, TransactionTraceV1, VmTraceRowV1,
        },
    },
    solana_account::{AccountSharedData, ReadableAccount},
    solana_program_runtime::invoke_context::VmExecutionTrace,
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hash,
    solana_svm::{
        account_loader::{FeesOnlyTransaction, NoOpTransaction},
        transaction_execution_result::ExecutedTransaction,
        transaction_processing_result::ProcessedTransaction,
    },
    solana_svm_transaction::svm_message::SVMMessage,
    solana_transaction_error::TransactionError,
};

pub fn fixture_trace_header_v1(transaction_bytes: &[u8], runtime_bytes: &[u8]) -> TraceHeaderV1 {
    TraceHeaderV1 {
        proof_inputs: ErStepPublicInputsV1 {
            domain: FrBytes::er_step_domain_v1(
                ER_STEP_PROOF_KIND_FULL_TRANSACTION,
                ER_STEP_PROOF_VERSION_V1,
            ),
            session_context: FrBytes::ZERO,
            slot_step: FrBytes::ZERO,
            pre_state_root: FrBytes::ZERO,
            post_state_root: FrBytes::ZERO,
            tx_effect_root: FrBytes::ZERO,
            readonly_l1_root: FrBytes::ZERO,
            settlement_effect_root: FrBytes::ZERO,
        },
        transaction_commitment: hash(transaction_bytes).to_bytes(),
        runtime_commitment: hash(runtime_bytes).to_bytes(),
        trace_schema_commitment: hash(b"northstar-transaction-trace-v1").to_bytes(),
    }
}

pub fn build_transaction_trace_v1(
    header: TraceHeaderV1,
    input_accounts: &[(Pubkey, AccountSharedData)],
    execution: &BankTxnProcessingResult,
) -> TransactionTraceV1 {
    let mut events = Vec::new();

    match execution {
        BankTxnProcessingResult::FailedVerification(error) => {
            if matches!(error, TransactionError::SignatureFailure) {
                stage(
                    &mut events,
                    ProcessorStageV1::DecodeAndSanitize,
                    StageOutcomeV1::Success,
                    None,
                );
                stage(
                    &mut events,
                    ProcessorStageV1::SignatureVerification,
                    StageOutcomeV1::Failure,
                    Some(error),
                );
            } else {
                stage(
                    &mut events,
                    ProcessorStageV1::DecodeAndSanitize,
                    StageOutcomeV1::Failure,
                    Some(error),
                );
            }
            outcome_event(
                &mut events,
                TransactionOutcomeV1::Unprocessable,
                Some(error),
                0,
                0,
                0,
                0,
                &[],
                &[],
            );
        }
        BankTxnProcessingResult::Processed {
            result,
            runtime_transaction,
        } => {
            for processor_stage in [
                ProcessorStageV1::DecodeAndSanitize,
                ProcessorStageV1::SignatureVerification,
            ] {
                stage(&mut events, processor_stage, StageOutcomeV1::Success, None);
            }

            for (input_index, (address, account)) in input_accounts.iter().enumerate() {
                let transaction_index = runtime_transaction
                    .account_keys()
                    .iter()
                    .position(|key| key == address)
                    .and_then(|index| u32::try_from(index).ok())
                    .unwrap_or(u32::MAX);
                let phase = usize::try_from(transaction_index)
                    .ok()
                    .filter(|index| runtime_transaction.is_writable(*index))
                    .map(|_| AccountPhaseV1::Pre)
                    .unwrap_or(AccountPhaseV1::Readonly);
                let stable_index = if transaction_index == u32::MAX {
                    u32::try_from(input_index).unwrap_or(u32::MAX)
                } else {
                    transaction_index
                };
                events.push(TraceEventV1::AccountState {
                    phase,
                    transaction_index: stable_index,
                    account: account_trace(*address, account),
                });
            }

            if result.is_ok() {
                stage(
                    &mut events,
                    ProcessorStageV1::BankChecks,
                    StageOutcomeV1::Success,
                    None,
                );
            }
            match result {
                Err(error) => {
                    stage(
                        &mut events,
                        ProcessorStageV1::BankChecks,
                        StageOutcomeV1::Failure,
                        Some(error),
                    );
                    outcome_event(
                        &mut events,
                        TransactionOutcomeV1::Unprocessable,
                        Some(error),
                        0,
                        0,
                        0,
                        0,
                        &[],
                        &[],
                    );
                }
                Ok(ProcessedTransaction::NoOp(transaction)) => {
                    trace_noop(&mut events, transaction);
                }
                Ok(ProcessedTransaction::FeesOnly(transaction)) => {
                    trace_fees_only(&mut events, transaction);
                }
                Ok(ProcessedTransaction::Executed(transaction)) => {
                    trace_executed(&mut events, transaction);
                }
            }
        }
    }

    TransactionTraceV1 { header, events }
}

fn trace_noop(events: &mut Vec<TraceEventV1>, transaction: &NoOpTransaction) {
    stage(
        events,
        ProcessorStageV1::FeeAndNonceValidation,
        StageOutcomeV1::Failure,
        Some(&transaction.validation_error),
    );
    stage(
        events,
        ProcessorStageV1::CommitOrRollback,
        StageOutcomeV1::Success,
        None,
    );
    outcome_event(
        events,
        TransactionOutcomeV1::NoOp,
        Some(&transaction.validation_error),
        transaction.compute_unit_limit,
        u64::from(transaction.loaded_accounts_bytes_limit),
        0,
        0,
        &[],
        &[],
    );
}

fn trace_fees_only(events: &mut Vec<TraceEventV1>, transaction: &FeesOnlyTransaction) {
    stage(
        events,
        ProcessorStageV1::FeeAndNonceValidation,
        StageOutcomeV1::Success,
        None,
    );
    stage(
        events,
        ProcessorStageV1::AccountLoading,
        StageOutcomeV1::Failure,
        Some(&transaction.load_error),
    );
    for (index, (address, account)) in transaction.rollback_accounts.iter().enumerate() {
        events.push(TraceEventV1::AccountState {
            phase: AccountPhaseV1::Rollback,
            transaction_index: u32::try_from(index).unwrap_or(u32::MAX),
            account: account_trace(*address, account),
        });
    }
    stage(
        events,
        ProcessorStageV1::CommitOrRollback,
        StageOutcomeV1::Success,
        None,
    );
    outcome_event(
        events,
        TransactionOutcomeV1::FeesOnly,
        Some(&transaction.load_error),
        0,
        u64::from(transaction.loaded_accounts_data_size),
        transaction.fee_details.transaction_fee(),
        transaction.fee_details.prioritization_fee(),
        &[],
        &[],
    );
}

fn trace_executed(events: &mut Vec<TraceEventV1>, transaction: &ExecutedTransaction) {
    for processor_stage in [
        ProcessorStageV1::FeeAndNonceValidation,
        ProcessorStageV1::AccountLoading,
        ProcessorStageV1::ProgramLoading,
    ] {
        stage(events, processor_stage, StageOutcomeV1::Success, None);
    }

    let status = transaction.execution_details.status.as_ref();
    stage(
        events,
        ProcessorStageV1::Execution,
        if status.is_ok() {
            StageOutcomeV1::Success
        } else {
            StageOutcomeV1::Failure
        },
        status.err(),
    );

    cpi_boundary_events(events, transaction);
    for trace in &transaction.execution_details.vm_traces {
        vm_trace_events(events, trace, status.is_ok());
    }

    stage(
        events,
        ProcessorStageV1::PostExecutionChecks,
        if status.is_ok() {
            StageOutcomeV1::Success
        } else {
            StageOutcomeV1::Failure
        },
        status.err(),
    );

    if status.is_ok() {
        for (index, ((address, account), touched)) in transaction
            .loaded_transaction
            .accounts
            .iter()
            .zip(&transaction.loaded_transaction.touched_flags)
            .enumerate()
        {
            if *touched {
                events.push(TraceEventV1::AccountState {
                    phase: AccountPhaseV1::Post,
                    transaction_index: u32::try_from(index).unwrap_or(u32::MAX),
                    account: account_trace(*address, account),
                });
            }
        }
    } else {
        for (index, (address, account)) in transaction
            .loaded_transaction
            .rollback_accounts
            .iter()
            .enumerate()
        {
            events.push(TraceEventV1::AccountState {
                phase: AccountPhaseV1::Rollback,
                transaction_index: u32::try_from(index).unwrap_or(u32::MAX),
                account: account_trace(*address, account),
            });
        }
    }

    stage(
        events,
        ProcessorStageV1::CommitOrRollback,
        StageOutcomeV1::Success,
        None,
    );
    let logs = transaction
        .execution_details
        .log_messages
        .as_deref()
        .unwrap_or_default();
    let return_data = transaction
        .execution_details
        .return_data
        .as_ref()
        .map(|data| data.data.as_slice())
        .unwrap_or_default();
    outcome_event(
        events,
        if status.is_ok() {
            TransactionOutcomeV1::ExecutedSuccess
        } else {
            TransactionOutcomeV1::ExecutedFailure
        },
        status.err(),
        transaction.execution_details.executed_units,
        u64::from(transaction.loaded_transaction.loaded_accounts_data_size),
        transaction.loaded_transaction.fee_details.transaction_fee(),
        transaction
            .loaded_transaction
            .fee_details
            .prioritization_fee(),
        return_data,
        logs,
    );
}

fn cpi_boundary_events(events: &mut Vec<TraceEventV1>, transaction: &ExecutedTransaction) {
    let Some(inner_groups) = &transaction.execution_details.inner_instructions else {
        return;
    };
    let mut next_id = 1u64 << 63;
    let mut parent_at_height = Vec::<Option<u64>>::new();
    for (outer_index, inner_group) in inner_groups.iter().enumerate() {
        parent_at_height.clear();
        parent_at_height.resize(16, None);
        parent_at_height[1] = u64::try_from(outer_index).ok();
        for inner in inner_group {
            let stack_height = usize::from(inner.stack_height);
            let parent_invocation_id = stack_height
                .checked_sub(1)
                .and_then(|height| parent_at_height.get(height))
                .copied()
                .flatten();
            let program_id = transaction
                .loaded_transaction
                .accounts
                .get(usize::from(inner.instruction.program_id_index))
                .map(|(address, _)| address.to_bytes())
                .unwrap_or_default();
            for boundary in [
                InstructionBoundaryV1::Enter,
                if transaction.execution_details.status.is_ok() {
                    InstructionBoundaryV1::ExitSuccess
                } else {
                    InstructionBoundaryV1::ExitFailure
                },
            ] {
                events.push(TraceEventV1::InstructionBoundary {
                    boundary,
                    invocation_id: next_id,
                    parent_invocation_id,
                    instruction_trace_index: u32::MAX,
                    stack_height: u32::from(inner.stack_height),
                    program_id,
                    compute_units_remaining: 0,
                });
            }
            if let Some(slot) = parent_at_height.get_mut(stack_height) {
                *slot = Some(next_id);
            }
            next_id = next_id.saturating_add(1);
        }
    }
}

fn vm_trace_events(events: &mut Vec<TraceEventV1>, trace: &VmExecutionTrace, success: bool) {
    let invocation_id = u64::try_from(trace.instruction_trace_index).unwrap_or(u64::MAX);
    events.push(TraceEventV1::InstructionBoundary {
        boundary: InstructionBoundaryV1::Enter,
        invocation_id,
        parent_invocation_id: trace
            .parent_instruction_trace_index
            .and_then(|id| u64::try_from(id).ok()),
        instruction_trace_index: u32::try_from(trace.instruction_trace_index).unwrap_or(u32::MAX),
        stack_height: u32::try_from(trace.stack_height).unwrap_or(u32::MAX),
        program_id: trace.program_id.to_bytes(),
        compute_units_remaining: trace.compute_units_before,
    });

    let rows = trace
        .rows
        .iter()
        .map(|row| VmTraceRowV1 {
            registers: row.registers,
            instruction: row.instruction,
        })
        .collect::<Vec<_>>();
    for (index, row) in trace.rows.iter().enumerate() {
        if let Some(function_key) = row.syscall_key {
            events.push(TraceEventV1::Syscall {
                invocation_id,
                vm_row: u64::try_from(index).unwrap_or(u64::MAX),
                function_key,
                arguments: row.registers[1..6].try_into().unwrap(),
                result: trace
                    .rows
                    .get(index.saturating_add(1))
                    .map(|next| next.registers[0])
                    .unwrap_or_default(),
            });
        }
    }

    let zero_stack = vec![0; trace.stack_after.len()];
    let zero_heap = vec![0; trace.heap_after.len()];
    events.push(TraceEventV1::VmInvocation {
        invocation_id,
        program_id: trace.program_id.to_bytes(),
        program_hash: trace.program_hash,
        sbpf_version: trace.sbpf_version,
        compute_units_before: trace.compute_units_before,
        compute_units_after: trace.compute_units_after,
        rows,
        memory: vec![
            memory_trace(
                MemoryRegionV1::ProgramInput,
                0,
                &trace.program_input_before,
                &trace.program_input_after,
            ),
            memory_trace(MemoryRegionV1::Stack, 0, &zero_stack, &trace.stack_after),
            memory_trace(MemoryRegionV1::Heap, 0, &zero_heap, &trace.heap_after),
        ],
    });

    events.push(TraceEventV1::InstructionBoundary {
        boundary: if success {
            InstructionBoundaryV1::ExitSuccess
        } else {
            InstructionBoundaryV1::ExitFailure
        },
        invocation_id,
        parent_invocation_id: trace
            .parent_instruction_trace_index
            .and_then(|id| u64::try_from(id).ok()),
        instruction_trace_index: u32::try_from(trace.instruction_trace_index).unwrap_or(u32::MAX),
        stack_height: u32::try_from(trace.stack_height).unwrap_or(u32::MAX),
        program_id: trace.program_id.to_bytes(),
        compute_units_remaining: trace.compute_units_after,
    });
}

fn memory_trace(
    region: MemoryRegionV1,
    virtual_address: u64,
    before: &[u8],
    after: &[u8],
) -> MemoryTraceV1 {
    MemoryTraceV1 {
        region,
        virtual_address,
        before_len: u64::try_from(before.len()).unwrap_or(u64::MAX),
        after_len: u64::try_from(after.len()).unwrap_or(u64::MAX),
        before_hash: hash(before).to_bytes(),
        after_hash: hash(after).to_bytes(),
        deltas: memory_deltas(before, after),
    }
}

fn memory_deltas(before: &[u8], after: &[u8]) -> Vec<MemoryDeltaV1> {
    let common_len = before.len().min(after.len());
    let mut deltas = Vec::new();
    let mut index = 0;
    while index < common_len {
        if before[index] == after[index] {
            index = index.saturating_add(1);
            continue;
        }
        let start = index;
        while index < common_len && before[index] != after[index] {
            index = index.saturating_add(1);
        }
        deltas.push(MemoryDeltaV1 {
            offset: u64::try_from(start).unwrap_or(u64::MAX),
            before: before[start..index].to_vec(),
            after: after[start..index].to_vec(),
        });
    }
    if before.len() != after.len() {
        deltas.push(MemoryDeltaV1 {
            offset: u64::try_from(common_len).unwrap_or(u64::MAX),
            before: before[common_len..].to_vec(),
            after: after[common_len..].to_vec(),
        });
    }
    deltas
}

fn account_trace(address: Pubkey, account: &AccountSharedData) -> AccountStateTraceV1 {
    AccountStateTraceV1 {
        address: address.to_bytes(),
        lamports: account.lamports(),
        owner: account.owner().to_bytes(),
        executable: account.executable(),
        rent_epoch: account.rent_epoch(),
        data: account.data().to_vec(),
    }
}

fn stage(
    events: &mut Vec<TraceEventV1>,
    processor_stage: ProcessorStageV1,
    outcome: StageOutcomeV1,
    error: Option<&TransactionError>,
) {
    let (transaction_error, instruction_error, instruction_index, custom_error) =
        error_fields(error);
    events.push(TraceEventV1::ProcessorStage {
        stage: processor_stage,
        outcome: StageOutcomeV1::Enter,
        transaction_error: 0,
        instruction_error: 0,
        instruction_index: 0,
        custom_error: 0,
    });
    events.push(TraceEventV1::ProcessorStage {
        stage: processor_stage,
        outcome,
        transaction_error,
        instruction_error,
        instruction_index,
        custom_error,
    });
}

#[allow(clippy::too_many_arguments)]
fn outcome_event(
    events: &mut Vec<TraceEventV1>,
    outcome: TransactionOutcomeV1,
    error: Option<&TransactionError>,
    executed_units: u64,
    loaded_accounts_data_size: u64,
    transaction_fee: u64,
    prioritization_fee: u64,
    return_data: &[u8],
    logs: &[String],
) {
    let (transaction_error, instruction_error, instruction_index, custom_error) =
        error_fields(error);
    let mut log_bytes = Vec::new();
    for log in logs {
        log_bytes.extend_from_slice(&(log.len() as u64).to_le_bytes());
        log_bytes.extend_from_slice(log.as_bytes());
    }
    events.push(TraceEventV1::TransactionOutcome {
        outcome,
        transaction_error,
        instruction_error,
        instruction_index,
        custom_error,
        executed_units,
        loaded_accounts_data_size,
        transaction_fee,
        prioritization_fee,
        return_data: return_data.to_vec(),
        log_commitment: hash(&log_bytes).to_bytes(),
    });
}

fn error_fields(error: Option<&TransactionError>) -> (u32, u32, u32, u32) {
    use solana_instruction::error::InstructionError;
    let Some(error) = error else {
        return (0, 0, 0, 0);
    };
    let transaction_error = solana_svm::conformance::err::serialized_error_code(error);
    match error {
        TransactionError::InstructionError(index, instruction_error) => (
            transaction_error,
            solana_svm::conformance::err::serialized_error_code(instruction_error),
            u32::from(*index),
            match instruction_error {
                InstructionError::Custom(custom) => *custom,
                _ => 0,
            },
        ),
        _ => (transaction_error, 0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_deltas_are_minimal_and_ordered() {
        assert_eq!(
            memory_deltas(&[0, 1, 2, 3, 4], &[0, 9, 8, 3, 7]),
            vec![
                MemoryDeltaV1 {
                    offset: 1,
                    before: vec![1, 2],
                    after: vec![9, 8],
                },
                MemoryDeltaV1 {
                    offset: 4,
                    before: vec![4],
                    after: vec![7],
                },
            ]
        );
    }

    #[test]
    fn fixture_header_selects_full_transaction_domain() {
        let header = fixture_trace_header_v1(b"tx", b"runtime");
        assert_eq!(
            header.proof_inputs.domain,
            FrBytes::er_step_domain_v1(
                ER_STEP_PROOF_KIND_FULL_TRANSACTION,
                ER_STEP_PROOF_VERSION_V1
            )
        );
        assert_ne!(header.transaction_commitment, header.runtime_commitment);
    }
}
