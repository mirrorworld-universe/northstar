//! [`observe_success`] observes one packet without committing. [`ordered`] replays
//! complete batches on an owned, disposable bank. Neither authenticates the bootstrap.

pub mod ordered;
pub mod slot_chain;

use {
    agave_transaction_view::transaction_view::UnsanitizedTransactionView,
    solana_account::AccountSharedData,
    solana_pubkey::Pubkey,
    solana_runtime::bank::Bank,
    solana_sha256_hasher::hashv,
    solana_svm::{
        transaction_error_metrics::TransactionErrorMetrics,
        transaction_processing_result::ProcessedTransaction,
        transaction_processor::{ExecutionRecordingConfig, TransactionProcessingConfig},
    },
    solana_svm_timings::ExecuteTimings,
    solana_svm_transaction::svm_message::{SVMMessage, SVMStaticMessage},
    solana_transaction::TransactionVerificationMode,
    solana_transaction_error::TransactionError,
    std::fmt,
};

const MAX_WIRE: usize = 1232;
const MAX_CALLS: usize = 256;
const MAX_TRACE_BYTES: usize = 4 * 1024 * 1024;
const MAX_BATCH: usize = 64;
type Digest = [u8; 32];

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Context,
    Slot,
    Wire,
    Age,
    Bank(TransactionError),
    NotExecuted,
    Trace,
    Writes,
    Batch,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context => {
                f.write_str("local replay bank does not match the enrolled slot lineage")
            }
            Self::Slot => f.write_str("not the expected working slot"),
            Self::Wire => f.write_str("invalid signed packet"),
            Self::Age => f.write_str("blockhash outside ER processing window"),
            Self::Bank(error) => write!(f, "transaction did not succeed: {error}"),
            Self::NotExecuted => f.write_str("no executed transaction"),
            Self::Trace => f.write_str("missing or invalid instruction recording"),
            Self::Writes => f.write_str("inconsistent writable account recording"),
            Self::Batch => f.write_str("invalid receipt batch"),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub depth: u8,
    pub program: Digest,
    pub accounts: Vec<Digest>,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    wire_hash: Digest,
    trace_hash: Digest,
}
impl Receipt {
    pub fn wire_hash(&self) -> Digest {
        self.wire_hash
    }
    pub fn trace_hash(&self) -> Digest {
        self.trace_hash
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Write {
    pub address: Pubkey,
    pub before: Option<AccountSharedData>,
    pub after: AccountSharedData,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Observation {
    pub receipt: Receipt,
    /// A recorded CPI entered the callee. A caller may catch a callee error;
    /// the observer must also verify the expected consumer state change.
    pub calls: Vec<Invocation>,
    pub writes: Vec<Write>,
}

struct Trace {
    calls: Vec<Invocation>,
    remaining: usize,
}
impl Trace {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            remaining: MAX_TRACE_BYTES,
        }
    }
    fn push(
        &mut self,
        depth: u8,
        program_index: u8,
        indices: &[u8],
        data: &[u8],
        resolve: impl Fn(u8) -> Option<Digest>,
    ) -> Result<(), Error> {
        let previous = self.calls.last().map_or(0, |c| c.depth);
        if self.calls.len() == MAX_CALLS
            || depth == 0
            || depth > previous.saturating_add(1)
            || indices.len() > 256
            || data.len() > 10_240
        {
            return Err(Error::Trace);
        }
        let left = self
            .remaining
            .checked_sub(39)
            .and_then(|n| n.checked_sub(indices.len().saturating_mul(32)))
            .and_then(|n| n.checked_sub(data.len()))
            .ok_or(Error::Trace)?;
        self.calls.push(Invocation {
            depth,
            program: resolve(program_index).ok_or(Error::Trace)?,
            accounts: indices
                .iter()
                .map(|index| resolve(*index))
                .collect::<Option<_>>()
                .ok_or(Error::Trace)?,
            data: data.to_vec(),
        });
        self.remaining = left;
        Ok(())
    }
    fn receipt(&self, wire: &[u8]) -> Receipt {
        let mut hashes = Vec::with_capacity(self.calls.len().saturating_mul(32));
        for call in &self.calls {
            hashes.extend(
                hashv(&[
                    b"northstar/er-receipt/call/v2\0",
                    &[call.depth],
                    &call.program,
                    &(call.accounts.len() as u16).to_le_bytes(),
                    call.accounts.as_flattened(),
                    &(call.data.len() as u32).to_le_bytes(),
                    &call.data,
                ])
                .to_bytes(),
            );
        }
        Receipt {
            wire_hash: hashv(&[
                b"northstar/er-receipt/wire/v2\0",
                &(wire.len() as u16).to_le_bytes(),
                wire,
            ])
            .to_bytes(),
            trace_hash: hashv(&[
                b"northstar/er-receipt/trace/v2\0",
                &(self.calls.len() as u16).to_le_bytes(),
                &hashes,
            ])
            .to_bytes(),
        }
    }
}

/// All entries must come from successful executions in the proposed order. An
/// unsupported or failed execution invalidates this profile's batch; never omit it.
pub fn successful_batch_root(entries: &[Receipt]) -> Result<Digest, Error> {
    if entries.len() > MAX_BATCH {
        return Err(Error::Batch);
    }
    let mut encoded = Vec::with_capacity(entries.len().saturating_mul(65));
    for (i, e) in entries.iter().enumerate() {
        if e.wire_hash == [0; 32]
            || e.trace_hash == [0; 32]
            || entries[..i]
                .iter()
                .any(|prev| prev.wire_hash == e.wire_hash)
        {
            return Err(Error::Batch);
        }
        encoded.extend(e.wire_hash);
        encoded.push(1);
        encoded.extend(e.trace_hash);
    }
    Ok(hashv(&[
        b"northstar/er-receipt/batch/v2\0",
        &(entries.len() as u16).to_le_bytes(),
        &encoded,
    ])
    .to_bytes())
}

/// The caller supplies a dedicated local bank, including its own program images,
/// features, sysvars, blockhash queue and pre-state. No part of the packet supplies
/// any of those inputs. No account overrides, log parsing, or RPC simulation.
pub fn observe_success(bank: &Bank, slot: u64, wire: &[u8]) -> Result<Observation, Error> {
    execute_success(bank, slot, wire, ExecutionMode::Observe)
}

enum ExecutionMode {
    Observe,
    AdvanceScratch,
}

fn execute_success(
    bank: &Bank,
    slot: u64,
    wire: &[u8],
    mode: ExecutionMode,
) -> Result<Observation, Error> {
    if slot != bank.slot() || bank.freeze_started() {
        return Err(Error::Slot);
    }
    if !(1..=MAX_WIRE).contains(&wire.len()) {
        return Err(Error::Wire);
    }
    let view = UnsanitizedTransactionView::try_new_unsanitized(wire).map_err(|_| Error::Wire)?;
    let tx = bank
        .verify_transaction(view, TransactionVerificationMode::FullVerification)
        .map_err(Error::Bank)?;
    let age = bank.max_processing_age();
    if !bank
        .get_hash_age(tx.recent_blockhash())
        .is_some_and(|v| v <= age as u64)
    {
        return Err(Error::Age);
    }
    let batch = bank.prepare_locked_batch_from_single_tx(&tx);
    let guard = bank
        .try_enter_transaction_execution()
        .ok_or(Error::Bank(TransactionError::CommitCancelled))?;
    let mut timings = ExecuteTimings::default();
    let output = guard.load_and_execute_transactions(
        &batch,
        age,
        &mut timings,
        &mut TransactionErrorMetrics::default(),
        TransactionProcessingConfig {
            recording_config: ExecutionRecordingConfig {
                enable_cpi_recording: true,
                ..ExecutionRecordingConfig::default()
            },
            ..TransactionProcessingConfig::default()
        },
    );
    let [result] = output.processing_results.as_slice() else {
        return Err(Error::NotExecuted);
    };
    let ProcessedTransaction::Executed(executed) =
        result.as_ref().map_err(|e| Error::Bank(e.clone()))?
    else {
        return Err(Error::NotExecuted);
    };
    executed
        .execution_details
        .status
        .as_ref()
        .map_err(|e| Error::Bank(e.clone()))?;
    let groups = executed
        .execution_details
        .inner_instructions
        .as_ref()
        .ok_or(Error::Trace)?;
    if groups.len() != tx.num_instructions() {
        return Err(Error::Trace);
    }
    let keys = tx.account_keys();
    let resolve = |index: u8| keys.get(usize::from(index)).map(|k| k.to_bytes());
    let mut trace = Trace::new();
    for (outer, group) in tx.instructions_iter().zip(groups) {
        trace.push(
            1,
            outer.program_id_index,
            outer.accounts,
            outer.data,
            resolve,
        )?;
        for inner in group {
            if inner.stack_height < 2 {
                return Err(Error::Trace);
            }
            let ix = &inner.instruction;
            trace.push(
                inner.stack_height,
                ix.program_id_index,
                &ix.accounts,
                &ix.data,
                resolve,
            )?;
        }
    }
    let loaded = &executed.loaded_transaction;
    if loaded.accounts.len() != loaded.touched_flags.len() {
        return Err(Error::Writes);
    }
    let mut writes = Vec::new();
    for (index, ((address, after), touched)) in loaded
        .accounts
        .iter()
        .zip(&loaded.touched_flags)
        .enumerate()
    {
        if *touched {
            if keys.get(index) != Some(address) || !tx.is_writable(index) {
                return Err(Error::Writes);
            }
            writes.push(Write {
                address: *address,
                before: bank.get_account(address),
                after: after.clone(),
            });
        }
    }
    writes.sort_unstable_by_key(|w| w.address);
    if writes.windows(2).any(|w| w[0].address == w[1].address) {
        return Err(Error::Writes);
    }
    if bank.freeze_started() {
        return Err(Error::Slot);
    }
    let observation = Observation {
        receipt: trace.receipt(wire),
        calls: trace.calls,
        writes,
    };
    if let ExecutionMode::AdvanceScratch = mode {
        let results = bank.commit_transactions(
            batch.sanitized_transactions(),
            output.processing_results,
            &output.processed_counts,
            &mut timings,
        );
        let [result] = results.as_slice() else {
            return Err(Error::NotExecuted);
        };
        result
            .as_ref()
            .map_err(|e| Error::Bank(e.clone()))?
            .status
            .as_ref()
            .map_err(|e| Error::Bank(e.clone()))?;
    }
    Ok(observation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_interoperability_vector_and_batch_order() {
        let mut trace = Trace::new();
        trace.push(1, 7, &[9], b"abc", |n| Some([n; 32])).unwrap();
        let entry = trace.receipt(b"fixture:wire");
        assert_eq!(
            successful_batch_root(&[entry]).unwrap(),
            [
                0x6a, 0xf7, 0x81, 0x97, 0x57, 0xd2, 0x48, 0xa0, 0x2b, 0xf2, 0xf0, 0xae, 0x30, 0xab,
                0xe3, 0x53, 0x06, 0xcb, 0x18, 0x03, 0x3c, 0xb9, 0xa8, 0x1c, 0xd2, 0x54, 0xaa, 0x0f,
                0x48, 0xed, 0x52, 0x9f
            ]
        );
        let other = trace.receipt(b"different");
        assert_ne!(
            successful_batch_root(&[entry, other]).unwrap(),
            successful_batch_root(&[other, entry]).unwrap()
        );
        assert_eq!(successful_batch_root(&[entry, entry]), Err(Error::Batch));
        assert_eq!(
            successful_batch_root(&[entry; MAX_BATCH + 1]),
            Err(Error::Batch)
        );
    }

    #[test]
    fn invalid_trace_gives_no_record() {
        let mut trace = Trace::new();
        let resolve = |n: u8| (n != 0).then_some([n; 32]);
        for (depth, program, keys) in [
            (0, 1, &[1][..]),
            (2, 1, &[1][..]),
            (1, 0, &[1][..]),
            (1, 1, &[0][..]),
        ] {
            assert_eq!(
                trace.push(depth, program, keys, &[], resolve),
                Err(Error::Trace)
            );
        }
        assert!(trace.calls.is_empty());
        assert_eq!(trace.remaining, MAX_TRACE_BYTES);
    }
}
