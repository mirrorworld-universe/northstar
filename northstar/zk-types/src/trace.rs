use {crate::ErStepPublicInputsV1, alloc::vec::Vec};

pub const TRANSACTION_TRACE_MAGIC_V1: [u8; 8] = *b"NSTRACE1";
pub const TRANSACTION_TRACE_SCHEMA_V1: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceEncodingError {
    LengthOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProcessorStageV1 {
    DecodeAndSanitize = 1,
    SignatureVerification = 2,
    BankChecks = 3,
    FeeAndNonceValidation = 4,
    AccountLoading = 5,
    ProgramLoading = 6,
    Execution = 7,
    PostExecutionChecks = 8,
    CommitOrRollback = 9,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StageOutcomeV1 {
    Enter = 1,
    Success = 2,
    Failure = 3,
    Unsupported = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AccountPhaseV1 {
    Pre = 1,
    Post = 2,
    Rollback = 3,
    Readonly = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InstructionBoundaryV1 {
    Enter = 1,
    ExitSuccess = 2,
    ExitFailure = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MemoryRegionV1 {
    ProgramInput = 1,
    Stack = 2,
    Heap = 3,
    Account = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransactionOutcomeV1 {
    Unprocessable = 1,
    NoOp = 2,
    FeesOnly = 3,
    ExecutedSuccess = 4,
    ExecutedFailure = 5,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceHeaderV1 {
    pub proof_inputs: ErStepPublicInputsV1,
    pub transaction_commitment: [u8; 32],
    pub runtime_commitment: [u8; 32],
    pub trace_schema_commitment: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStateTraceV1 {
    pub address: [u8; 32],
    pub lamports: u64,
    pub owner: [u8; 32],
    pub executable: bool,
    pub rent_epoch: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmTraceRowV1 {
    /// Pre-instruction r0-r10 and program counter in slot 11.
    pub registers: [u64; 12],
    /// Raw eight-byte SBPF instruction at `registers[11]`.
    pub instruction: [u8; 8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDeltaV1 {
    pub offset: u64,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryTraceV1 {
    pub region: MemoryRegionV1,
    pub virtual_address: u64,
    pub before_len: u64,
    pub after_len: u64,
    pub before_hash: [u8; 32],
    pub after_hash: [u8; 32],
    pub deltas: Vec<MemoryDeltaV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceEventV1 {
    ProcessorStage {
        stage: ProcessorStageV1,
        outcome: StageOutcomeV1,
        transaction_error: u32,
        instruction_error: u32,
        instruction_index: u32,
        custom_error: u32,
    },
    AccountState {
        phase: AccountPhaseV1,
        transaction_index: u32,
        account: AccountStateTraceV1,
    },
    InstructionBoundary {
        boundary: InstructionBoundaryV1,
        invocation_id: u64,
        parent_invocation_id: Option<u64>,
        instruction_trace_index: u32,
        stack_height: u32,
        program_id: [u8; 32],
        compute_units_remaining: u64,
    },
    Syscall {
        invocation_id: u64,
        vm_row: u64,
        function_key: u32,
        arguments: [u64; 5],
        result: u64,
    },
    VmInvocation {
        invocation_id: u64,
        program_id: [u8; 32],
        program_hash: [u8; 32],
        sbpf_version: u8,
        compute_units_before: u64,
        compute_units_after: u64,
        rows: Vec<VmTraceRowV1>,
        memory: Vec<MemoryTraceV1>,
    },
    TransactionOutcome {
        outcome: TransactionOutcomeV1,
        transaction_error: u32,
        instruction_error: u32,
        instruction_index: u32,
        custom_error: u32,
        executed_units: u64,
        loaded_accounts_data_size: u64,
        transaction_fee: u64,
        prioritization_fee: u64,
        return_data: Vec<u8>,
        log_commitment: [u8; 32],
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionTraceV1 {
    pub header: TraceHeaderV1,
    pub events: Vec<TraceEventV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceSummaryError {
    MissingOutcome,
    DuplicateOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceAccountEffectV1 {
    pub transaction_index: u32,
    pub account: AccountStateTraceV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceSummaryV1 {
    pub outcome: TransactionOutcomeV1,
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
    pub post_accounts: Vec<TraceAccountEffectV1>,
    pub rollback_accounts: Vec<TraceAccountEffectV1>,
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn fixed(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    fn len(&mut self, value: usize) -> Result<(), TraceEncodingError> {
        self.u64(u64::try_from(value).map_err(|_| TraceEncodingError::LengthOverflow)?);
        Ok(())
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), TraceEncodingError> {
        self.len(value.len())?;
        self.fixed(value);
        Ok(())
    }
}

impl TransactionTraceV1 {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, TraceEncodingError> {
        let mut encoder = Encoder::new();
        encoder.fixed(&TRANSACTION_TRACE_MAGIC_V1);
        encoder.u16(TRANSACTION_TRACE_SCHEMA_V1);
        for input in self.header.proof_inputs.to_array() {
            encoder.fixed(&input);
        }
        encoder.fixed(&self.header.transaction_commitment);
        encoder.fixed(&self.header.runtime_commitment);
        encoder.fixed(&self.header.trace_schema_commitment);
        encoder.len(self.events.len())?;
        for event in &self.events {
            event.encode(&mut encoder)?;
        }
        Ok(encoder.bytes)
    }

    pub fn summary(&self) -> Result<TraceSummaryV1, TraceSummaryError> {
        let mut post_accounts = Vec::new();
        let mut rollback_accounts = Vec::new();
        let mut summary = None;
        for event in &self.events {
            match event {
                TraceEventV1::AccountState {
                    phase,
                    transaction_index,
                    account,
                } => {
                    let effect = TraceAccountEffectV1 {
                        transaction_index: *transaction_index,
                        account: account.clone(),
                    };
                    match phase {
                        AccountPhaseV1::Post => post_accounts.push(effect),
                        AccountPhaseV1::Rollback => rollback_accounts.push(effect),
                        AccountPhaseV1::Pre | AccountPhaseV1::Readonly => {}
                    }
                }
                TraceEventV1::TransactionOutcome {
                    outcome,
                    transaction_error,
                    instruction_error,
                    instruction_index,
                    custom_error,
                    executed_units,
                    loaded_accounts_data_size,
                    transaction_fee,
                    prioritization_fee,
                    return_data,
                    log_commitment,
                } => {
                    if summary.is_some() {
                        return Err(TraceSummaryError::DuplicateOutcome);
                    }
                    summary = Some(TraceSummaryV1 {
                        outcome: *outcome,
                        transaction_error: *transaction_error,
                        instruction_error: *instruction_error,
                        instruction_index: *instruction_index,
                        custom_error: *custom_error,
                        executed_units: *executed_units,
                        loaded_accounts_data_size: *loaded_accounts_data_size,
                        transaction_fee: *transaction_fee,
                        prioritization_fee: *prioritization_fee,
                        return_data: return_data.clone(),
                        log_commitment: *log_commitment,
                        post_accounts: Vec::new(),
                        rollback_accounts: Vec::new(),
                    });
                }
                _ => {}
            }
        }
        let mut summary = summary.ok_or(TraceSummaryError::MissingOutcome)?;
        summary.post_accounts = post_accounts;
        summary.rollback_accounts = rollback_accounts;
        Ok(summary)
    }
}

impl TraceEventV1 {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), TraceEncodingError> {
        match self {
            Self::ProcessorStage {
                stage,
                outcome,
                transaction_error,
                instruction_error,
                instruction_index,
                custom_error,
            } => {
                encoder.u8(1);
                encoder.u8(*stage as u8);
                encoder.u8(*outcome as u8);
                encode_error(
                    encoder,
                    *transaction_error,
                    *instruction_error,
                    *instruction_index,
                    *custom_error,
                );
            }
            Self::AccountState {
                phase,
                transaction_index,
                account,
            } => {
                encoder.u8(2);
                encoder.u8(*phase as u8);
                encoder.u32(*transaction_index);
                account.encode(encoder)?;
            }
            Self::InstructionBoundary {
                boundary,
                invocation_id,
                parent_invocation_id,
                instruction_trace_index,
                stack_height,
                program_id,
                compute_units_remaining,
            } => {
                encoder.u8(3);
                encoder.u8(*boundary as u8);
                encoder.u64(*invocation_id);
                encoder.bool(parent_invocation_id.is_some());
                encoder.u64(parent_invocation_id.unwrap_or_default());
                encoder.u32(*instruction_trace_index);
                encoder.u32(*stack_height);
                encoder.fixed(program_id);
                encoder.u64(*compute_units_remaining);
            }
            Self::Syscall {
                invocation_id,
                vm_row,
                function_key,
                arguments,
                result,
            } => {
                encoder.u8(4);
                encoder.u64(*invocation_id);
                encoder.u64(*vm_row);
                encoder.u32(*function_key);
                for argument in arguments {
                    encoder.u64(*argument);
                }
                encoder.u64(*result);
            }
            Self::VmInvocation {
                invocation_id,
                program_id,
                program_hash,
                sbpf_version,
                compute_units_before,
                compute_units_after,
                rows,
                memory,
            } => {
                encoder.u8(5);
                encoder.u64(*invocation_id);
                encoder.fixed(program_id);
                encoder.fixed(program_hash);
                encoder.u8(*sbpf_version);
                encoder.u64(*compute_units_before);
                encoder.u64(*compute_units_after);
                encoder.len(rows.len())?;
                for row in rows {
                    for register in row.registers {
                        encoder.u64(register);
                    }
                    encoder.fixed(&row.instruction);
                }
                encoder.len(memory.len())?;
                for region in memory {
                    region.encode(encoder)?;
                }
            }
            Self::TransactionOutcome {
                outcome,
                transaction_error,
                instruction_error,
                instruction_index,
                custom_error,
                executed_units,
                loaded_accounts_data_size,
                transaction_fee,
                prioritization_fee,
                return_data,
                log_commitment,
            } => {
                encoder.u8(6);
                encoder.u8(*outcome as u8);
                encode_error(
                    encoder,
                    *transaction_error,
                    *instruction_error,
                    *instruction_index,
                    *custom_error,
                );
                encoder.u64(*executed_units);
                encoder.u64(*loaded_accounts_data_size);
                encoder.u64(*transaction_fee);
                encoder.u64(*prioritization_fee);
                encoder.bytes(return_data)?;
                encoder.fixed(log_commitment);
            }
        }
        Ok(())
    }
}

fn encode_error(
    encoder: &mut Encoder,
    transaction_error: u32,
    instruction_error: u32,
    instruction_index: u32,
    custom_error: u32,
) {
    encoder.u32(transaction_error);
    encoder.u32(instruction_error);
    encoder.u32(instruction_index);
    encoder.u32(custom_error);
}

impl AccountStateTraceV1 {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), TraceEncodingError> {
        encoder.fixed(&self.address);
        encoder.u64(self.lamports);
        encoder.fixed(&self.owner);
        encoder.bool(self.executable);
        encoder.u64(self.rent_epoch);
        encoder.bytes(&self.data)
    }
}

impl MemoryTraceV1 {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), TraceEncodingError> {
        encoder.u8(self.region as u8);
        encoder.u64(self.virtual_address);
        encoder.u64(self.before_len);
        encoder.u64(self.after_len);
        encoder.fixed(&self.before_hash);
        encoder.fixed(&self.after_hash);
        encoder.len(self.deltas.len())?;
        for delta in &self.deltas {
            encoder.u64(delta.offset);
            encoder.bytes(&delta.before)?;
            encoder.bytes(&delta.after)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::FrBytes, alloc::vec};

    fn inputs() -> ErStepPublicInputsV1 {
        ErStepPublicInputsV1 {
            domain: FrBytes::er_step_domain_v1(2, 1),
            session_context: FrBytes::from_u64(2),
            slot_step: FrBytes::from_u64_pair(3, 4),
            pre_state_root: FrBytes::from_u64(5),
            post_state_root: FrBytes::from_u64(6),
            tx_effect_root: FrBytes::from_u64(7),
            readonly_l1_root: FrBytes::from_u64(8),
            settlement_effect_root: FrBytes::from_u64(9),
        }
    }

    #[test]
    fn canonical_encoding_is_stable_and_length_delimited() {
        let trace = TransactionTraceV1 {
            header: TraceHeaderV1 {
                proof_inputs: inputs(),
                transaction_commitment: [10; 32],
                runtime_commitment: [11; 32],
                trace_schema_commitment: [12; 32],
            },
            events: vec![
                TraceEventV1::ProcessorStage {
                    stage: ProcessorStageV1::Execution,
                    outcome: StageOutcomeV1::Success,
                    transaction_error: 0,
                    instruction_error: 0,
                    instruction_index: 0,
                    custom_error: 0,
                },
                TraceEventV1::TransactionOutcome {
                    outcome: TransactionOutcomeV1::ExecutedSuccess,
                    transaction_error: 0,
                    instruction_error: 0,
                    instruction_index: 0,
                    custom_error: 0,
                    executed_units: 42,
                    loaded_accounts_data_size: 64,
                    transaction_fee: 5_000,
                    prioritization_fee: 0,
                    return_data: vec![1, 2, 3],
                    log_commitment: [13; 32],
                },
            ],
        };

        let first = trace.canonical_bytes().unwrap();
        let second = trace.canonical_bytes().unwrap();
        assert_eq!(first, second);
        assert_eq!(&first[..8], &TRANSACTION_TRACE_MAGIC_V1);
        assert_eq!(u64::from_le_bytes(first[362..370].try_into().unwrap()), 2);
        assert!(first.ends_with(&[13; 32]));
        let summary = trace.summary().unwrap();
        assert_eq!(summary.outcome, TransactionOutcomeV1::ExecutedSuccess);
        assert_eq!(summary.executed_units, 42);
        assert_eq!(summary.transaction_fee, 5_000);
        assert_eq!(summary.return_data, vec![1, 2, 3]);
    }

    #[test]
    fn every_event_variant_has_a_stable_distinct_tag() {
        let events = vec![
            TraceEventV1::ProcessorStage {
                stage: ProcessorStageV1::Execution,
                outcome: StageOutcomeV1::Success,
                transaction_error: 0,
                instruction_error: 0,
                instruction_index: 0,
                custom_error: 0,
            },
            TraceEventV1::AccountState {
                phase: AccountPhaseV1::Pre,
                transaction_index: 0,
                account: AccountStateTraceV1 {
                    address: [1; 32],
                    lamports: 2,
                    owner: [3; 32],
                    executable: false,
                    rent_epoch: 4,
                    data: vec![5],
                },
            },
            TraceEventV1::InstructionBoundary {
                boundary: InstructionBoundaryV1::Enter,
                invocation_id: 1,
                parent_invocation_id: None,
                instruction_trace_index: 0,
                stack_height: 1,
                program_id: [6; 32],
                compute_units_remaining: 7,
            },
            TraceEventV1::Syscall {
                invocation_id: 1,
                vm_row: 2,
                function_key: 3,
                arguments: [4; 5],
                result: 5,
            },
            TraceEventV1::VmInvocation {
                invocation_id: 1,
                program_id: [6; 32],
                program_hash: [7; 32],
                sbpf_version: 0,
                compute_units_before: 10,
                compute_units_after: 9,
                rows: vec![VmTraceRowV1 {
                    registers: [8; 12],
                    instruction: [9; 8],
                }],
                memory: vec![MemoryTraceV1 {
                    region: MemoryRegionV1::Heap,
                    virtual_address: 0,
                    before_len: 1,
                    after_len: 1,
                    before_hash: [10; 32],
                    after_hash: [11; 32],
                    deltas: vec![MemoryDeltaV1 {
                        offset: 0,
                        before: vec![0],
                        after: vec![1],
                    }],
                }],
            },
            TraceEventV1::TransactionOutcome {
                outcome: TransactionOutcomeV1::ExecutedSuccess,
                transaction_error: 0,
                instruction_error: 0,
                instruction_index: 0,
                custom_error: 0,
                executed_units: 1,
                loaded_accounts_data_size: 2,
                transaction_fee: 3,
                prioritization_fee: 4,
                return_data: vec![],
                log_commitment: [12; 32],
            },
        ];
        let tags = events
            .into_iter()
            .map(|event| {
                let trace = TransactionTraceV1 {
                    header: TraceHeaderV1 {
                        proof_inputs: inputs(),
                        transaction_commitment: [10; 32],
                        runtime_commitment: [11; 32],
                        trace_schema_commitment: [12; 32],
                    },
                    events: vec![event],
                };
                trace.canonical_bytes().unwrap()[370]
            })
            .collect::<Vec<_>>();
        assert_eq!(tags, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn summary_rejects_missing_and_duplicate_outcomes() {
        let mut trace = TransactionTraceV1 {
            header: TraceHeaderV1 {
                proof_inputs: inputs(),
                transaction_commitment: [0; 32],
                runtime_commitment: [0; 32],
                trace_schema_commitment: [0; 32],
            },
            events: vec![],
        };
        assert_eq!(trace.summary(), Err(TraceSummaryError::MissingOutcome));
        let outcome = TraceEventV1::TransactionOutcome {
            outcome: TransactionOutcomeV1::NoOp,
            transaction_error: 0,
            instruction_error: 0,
            instruction_index: 0,
            custom_error: 0,
            executed_units: 0,
            loaded_accounts_data_size: 0,
            transaction_fee: 0,
            prioritization_fee: 0,
            return_data: vec![],
            log_commitment: [0; 32],
        };
        trace.events = vec![outcome.clone(), outcome];
        assert_eq!(trace.summary(), Err(TraceSummaryError::DuplicateOutcome));
    }
}
