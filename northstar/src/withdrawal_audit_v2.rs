//! Read-only auditor for one successful CPI withdrawal step. Never called by admission.
//! Pins come from the auditor, not from the capsule or a bank supplied by its producer.
use {
    crate::{
        checkpoint::MAX_TRANSACTION_BYTES_V1,
        checkpoint_v2::CheckpointArtifactV2,
        continuity_v2::{
            hash_parts, CommittedAccountV2, Hash32, RecordedExecutionResultV2,
            RecordedInnerInstructionV2, SettlementEffectV2, TransitionPayloadV2,
        },
        ephemeral_tx_client::EphemeralTransactionClient,
    },
    bincode::Options,
    northstar_token_bridge::audit_match::{compare, Comparison, Transcript},
    solana_account::ReadableAccount,
    solana_loader_v3_interface::state::UpgradeableLoaderState,
    solana_message::AddressLoader,
    solana_pubkey::Pubkey,
    solana_runtime::bank::Bank,
    solana_sdk_ids::{bpf_loader, bpf_loader_upgradeable},
    solana_signature::Signature,
    solana_svm::{
        transaction_error_metrics::TransactionErrorMetrics,
        transaction_processor::{ExecutionRecordingConfig, TransactionProcessingConfig},
    },
    solana_svm_timings::ExecuteTimings,
    solana_transaction::versioned::VersionedTransaction,
    std::collections::BTreeMap,
    thiserror::Error,
};

/// The exact transaction cut matters: a frozen end-of-slot bank is not its pre-execution bank.
#[derive(Clone, Copy, Debug)]
pub struct AuditPins {
    pub bank_slot: u64,
    pub bank_hash: Hash32,
    pub checkpoint_binding: Hash32,
    pub step: u32,
    pub pre_state_root: Hash32,
    pub bridge: Pubkey,
    /// SHA-256 of loader-visible program bytes (including any ProgramData padding).
    pub bridge_image: Hash32,
}

#[derive(Debug, PartialEq, Eq, Error)]
pub enum AuditError {
    #[error("no independent verdict: {0}")]
    Unavailable(&'static str),
    #[error("checkpoint does not match the audited execution: {0}")]
    Mismatch(&'static str),
}
use AuditError::{Mismatch, Unavailable};

#[derive(Debug, PartialEq, Eq)]
pub struct VerifiedStep {
    pub signature: Signature,
    pub checkpoint_binding: Hash32,
    pub step: u32,
    pub bank_hash: Hash32,
    pub withdrawals: usize,
    pub cpi_withdrawals: usize,
}

fn program_image(bank: &Bank, key: &Pubkey) -> Option<Hash32> {
    let a = bank.get_account(key)?;
    if !a.executable() {
        return None;
    }
    if bpf_loader::check_id(a.owner()) {
        return Some(hash_parts(&[a.data()]));
    }
    if !bpf_loader_upgradeable::check_id(a.owner()) {
        return None;
    }
    let UpgradeableLoaderState::Program {
        programdata_address,
    } = bincode::deserialize(a.data()).ok()?
    else {
        return None;
    };
    if programdata_address != solana_loader_v3_interface::get_program_data_address(key) {
        return None;
    }
    let d = bank.get_account(&programdata_address)?;
    if d.executable() || !bpf_loader_upgradeable::check_id(d.owner()) {
        return None;
    }
    let UpgradeableLoaderState::ProgramData { slot, .. } = bincode::deserialize(d.data()).ok()?
    else {
        return None;
    };
    // A deployment in this slot is not necessarily the image selected by the program cache.
    if slot >= bank.slot() {
        return None;
    }
    Some(hash_parts(&[d.data().get(
        UpgradeableLoaderState::size_of_programdata_metadata()..,
    )?]))
}

fn encode<T: borsh::BorshSerialize>(value: &T) -> Result<Vec<u8>, AuditError> {
    borsh::to_vec(value).map_err(|_| Unavailable("cannot encode observations"))
}

/// Re-execute on a frozen ER bank independently replayed to the transaction cut, or restored
/// with full account-hash verification and no slot-load limit. A matching cached bank hash
/// does not authenticate a partial snapshot. Do not use a live working bank.
/// No transaction commit, account overrides, blockhash replacement, or producer replay inputs.
/// Only a nonempty set of withdrawals for the reviewed bridge with at least one CPI can pass.
pub fn audit(
    bank: Option<&Bank>,
    artifact: &CheckpointArtifactV2,
    pins: &AuditPins,
) -> Result<VerifiedStep, AuditError> {
    let bank = bank.ok_or(Unavailable("pre-execution bank missing"))?;
    if !bank.is_frozen()
        || bank.hash().to_bytes() != pins.bank_hash
        || bank.slot() != pins.bank_slot
    {
        return Err(Unavailable("frozen bank slot/hash does not match pin"));
    }
    if bank.rc.accounts.accounts_db.skip_initial_hash_calc {
        return Err(Unavailable("accounts hash verification disabled"));
    }
    if artifact.binding_hash().ok() != Some(pins.checkpoint_binding) {
        return Err(Unavailable("checkpoint binding does not match pin"));
    }
    let page = artifact
        .pages
        .get(pins.step as usize)
        .ok_or(Unavailable("step absent"))?;
    if page.index != pins.step
        || page.transition.er_slot != bank.slot()
        || page.pre_state.root().ok() != Some(pins.pre_state_root)
    {
        return Err(Unavailable("step does not match pinned pre-execution cut"));
    }
    artifact
        .validate()
        .map_err(|_| Mismatch("invalid checkpoint artifact"))?;
    let r = &page.transition;
    let TransitionPayloadV2::Execution {
        transaction,
        execution_result,
        pre,
        post,
        ..
    } = &r.payload
    else {
        return Err(Unavailable("not an execution step"));
    };
    if program_image(bank, &pins.bridge) != Some(pins.bridge_image) {
        return Err(Unavailable("reviewed bridge image not available at cut"));
    }
    for (k, v) in page.pre_state.accounts() {
        match bank.get_account(&Pubkey::from(*k)) {
            Some(a) if CommittedAccountV2::from_account(&a) == *v => (),
            None if v.lamports == 0 => (), // AccountsDb elides canonical tombstones.
            _ => return Err(Unavailable("bank does not open the committed pre-state")),
        }
    }
    let tx = signed_transaction(transaction).map_err(Mismatch)?;
    let signature = *tx.signatures.first().ok_or(Mismatch("missing signature"))?;
    if let Some(lookups) = tx.message.address_table_lookups() {
        bank.load_addresses(lookups)
            .map_err(|_| Unavailable("historical address table not resolvable"))?;
    }
    let batch = bank
        .prepare_entry_batch([tx.clone()])
        .map_err(|_| Unavailable("bank cannot prepare the signed transaction"))?;
    let mut replay = bank.load_and_execute_transactions(
        &batch,
        bank.max_processing_age(),
        &mut ExecuteTimings::default(),
        &mut TransactionErrorMetrics::default(),
        TransactionProcessingConfig {
            recording_config: ExecutionRecordingConfig::new_single_setting(true),
            ..Default::default()
        },
    );
    let processed = replay
        .processing_results
        .pop()
        .and_then(Result::ok)
        .ok_or(Unavailable("bank did not execute transaction"))?;
    let executed = processed
        .executed_transaction()
        .ok_or(Unavailable("bank did not execute instructions"))?;
    if processed.fee_details().total_fee() != 0 {
        return Err(Unavailable("not the zero-fee V2 profile"));
    }
    let details = &executed.execution_details;
    if details.status.is_err() {
        return if r.settlement_effects.is_empty() {
            Err(Unavailable("no successful withdrawal to audit"))
        } else {
            Err(Mismatch("payout transaction did not execute successfully"))
        };
    }
    let groups = details
        .inner_instructions
        .as_ref()
        .ok_or(Unavailable("re-execution did not produce a CPI trace"))?;
    let mut inner = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        for ix in g {
            inner.push(RecordedInnerInstructionV2 {
                outer_index: u32::try_from(i)
                    .map_err(|_| Unavailable("trace index out of range"))?,
                stack_height: u32::from(ix.stack_height),
                program_id_index: ix.instruction.program_id_index,
                accounts: ix.instruction.accounts.clone(),
                data: ix.instruction.data.clone(),
            });
        }
    }
    let actual = RecordedExecutionResultV2 {
        status: bincode::serialize(&details.status)
            .map_err(|_| Unavailable("cannot encode execution status"))?,
        executed_units: processed.executed_units(),
        inner,
        logs: details
            .log_messages
            .clone()
            .ok_or(Unavailable("missing execution logs"))?,
        return_data: details
            .return_data
            .as_ref()
            .map(|v| (v.program_id.to_bytes(), v.data.clone())),
    };
    let loaded = &executed.loaded_transaction;
    let mut writes = BTreeMap::new();
    if loaded.accounts.len() != loaded.touched_flags.len() {
        return Err(Unavailable("incomplete SVM write flags"));
    }
    for ((k, a), touched) in loaded.accounts.iter().zip(&loaded.touched_flags) {
        if *touched
            && !agave_reserved_account_keys::ReservedAccountKeys::all_keys_iter()
                .any(|key| key == k)
        {
            writes.insert(
                k.to_bytes(),
                (
                    CommittedAccountV2::from_account(&bank.get_account(k).unwrap_or_default()),
                    CommittedAccountV2::from_account(a),
                ),
            );
        }
    }
    for observation in &r.readonly_l1_values {
        let value = loaded
            .accounts
            .iter()
            .find(|(k, _)| k == &observation.account)
            .map(|(_, v)| v.clone())
            .or_else(|| bank.get_account(&observation.account))
            .unwrap_or_default();
        if value.owner() != &observation.owner
            || value.lamports() != observation.lamports
            || hash_parts(&[value.data()]) != observation.data_hash
        {
            return Err(Mismatch("readonly account observation"));
        }
    }
    let (before, after): (Vec<_>, Vec<_>) = writes
        .into_iter()
        .map(|(k, (a, b))| ((k, a), (k, b)))
        .unzip();
    let mut state = page.pre_state.clone();
    state
        .apply_execution(&before, &after)
        .map_err(|_| Mismatch("observed writes do not apply to checkpoint pre-state"))?;
    let (effects, cpi_count) = EphemeralTransactionClient::v2_settlement_effects(
        &page.pre_state.identity().portal.into(),
        bank,
        &tx,
        &actual.inner,
        &mut state,
    )
    .map_err(|_| Mismatch("observed calls do not derive settlement effects"))?;
    let verdict = compare(
        Transcript {
            execution: execution_result,
            writes: &encode(&(pre, post))?,
            effects: &encode(&r.settlement_effects)?,
        },
        Transcript {
            execution: &encode(&actual)?,
            writes: &encode(&(&before, &after))?,
            effects: &encode(&effects)?,
        },
        cpi_count,
    );
    match verdict {
        Comparison::Execution => return Err(Mismatch("execution result or invocation trace")),
        Comparison::Writes => return Err(Mismatch("touched account pre/post images")),
        Comparison::Effects => return Err(Mismatch("ordered settlement effects")),
        Comparison::NoCpiWithdrawal => return Err(Unavailable("no observed CPI withdrawal")),
        Comparison::Match => (),
    }
    let reviewed = |row: &Vec<u8>| {
        matches!(borsh::from_slice(row),
        Ok(SettlementEffectV2::TokenWithdrawal { bridge, .. }) if bridge == pins.bridge.to_bytes())
    };
    if effects.is_empty() || !effects.iter().all(reviewed) {
        return Err(Unavailable("mixed or unreviewed settlement programs"));
    }
    if state
        .opening()
        .map_err(|_| Mismatch("observed state cannot be opened"))?
        != r.after
    {
        return Err(Mismatch("post-state opening"));
    }
    Ok(VerifiedStep {
        signature,
        checkpoint_binding: pins.checkpoint_binding,
        step: pins.step,
        bank_hash: pins.bank_hash,
        withdrawals: effects.len(),
        cpi_withdrawals: cpi_count,
    })
}

fn signed_transaction(bytes: &[u8]) -> Result<VersionedTransaction, &'static str> {
    if bytes.is_empty() || bytes.len() > MAX_TRANSACTION_BYTES_V1 {
        return Err("invalid signed transaction length");
    }
    let tx: VersionedTransaction = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_TRANSACTION_BYTES_V1 as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|_| "invalid signed transaction bytes")?;
    tx.sanitize().map_err(|_| "invalid transaction")?;
    tx.verify_and_hash_message()
        .map_err(|_| "invalid transaction signatures")?;
    if tx.signatures.is_empty() || bincode::serialize(&tx).ok().as_deref() != Some(bytes) {
        return Err("noncanonical signed transaction");
    }
    Ok(tx)
}

pub mod reconstruction;

#[cfg(test)]
mod tests;
