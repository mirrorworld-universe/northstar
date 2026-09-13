//! Offline, supported-relation witness extraction. Callers supply authenticated L1 context.
use {
    crate::checkpoint::{CheckpointArtifactV1, CheckpointTransactionEffectV1},
    bincode::Options,
    northstar_transaction_proof::{
        checkpoint::CheckpointBindingV1, fixture::assemble_replay_witness_v1, public_inputs_bytes,
        replay, set_trace_hash, ReplayWitnessV1,
    },
    solana_account::ReadableAccount,
    solana_loader_v3_interface::state::UpgradeableLoaderState,
    solana_rpc::er_history::ErHistoryStore,
    solana_runtime::{
        bank::er_replay::{ErReplaySnapshot, MAX_ER_REPLAY_SNAPSHOT_BYTES},
        conformance::{
            proof_fixture::{ExecutedFullTransactionFixtureV1, FullTransactionFixtureV1},
            trace::{build_transaction_trace_v1, fixture_trace_header_v1},
            txn::BankTxnProcessingResult,
        },
    },
    solana_sha256_hasher::hash,
    solana_svm::transaction_processing_result::ProcessedTransaction,
    solana_transaction::versioned::VersionedTransaction,
};

pub struct ReplayContextV1 {
    pub session_context: Vec<u8>,
    pub agave_revision: [u8; 20],
    pub northstar_revision: [u8; 20],
    pub vm_config_hash: [u8; 32],
    pub syscall_registry_hash: [u8; 32],
}

fn convert<T: borsh::BorshSerialize, U: borsh::BorshDeserialize>(
    value: &T,
) -> Result<U, &'static str> {
    borsh::from_slice(&borsh::to_vec(value).map_err(|_| "checkpoint encoding")?)
        .map_err(|_| "checkpoint shape")
}

pub fn extract_replay_witness_v1(
    history: &ErHistoryStore,
    artifact: &CheckpointArtifactV1,
    step_index: usize,
    context: ReplayContextV1,
    expected_public_inputs: &[u8; 256],
) -> Result<ReplayWitnessV1, &'static str> {
    artifact.verify().map_err(|_| "checkpoint authentication")?;
    let page = artifact.da.pages.get(step_index).ok_or("step missing")?;
    let transaction: VersionedTransaction =
        bincode::deserialize(&page.transaction).map_err(|_| "transaction encoding")?;
    let signature = transaction.signatures.first().ok_or("signature missing")?;
    let capture = history
        .get_replay_capture(
            signature,
            solana_rpc_client_types::config::CommitmentConfig::finalized(),
        )
        .ok_or("finalized replay capture missing")?;
    let bytes = capture
        .reexecution_snapshot
        .as_ref()
        .ok_or("runtime snapshot missing")?;
    if bytes.len() > MAX_ER_REPLAY_SNAPSHOT_BYTES {
        return Err("runtime snapshot too large");
    }
    let snapshot: ErReplaySnapshot = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_ER_REPLAY_SNAPSHOT_BYTES as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|_| "runtime snapshot encoding")?;
    let keys = transaction.message.static_account_keys().to_vec();
    if keys.len() != 3 || capture.accounts.len() != 3 {
        return Err("unsupported account shape");
    }
    for (index, captured) in capture.accounts.iter().enumerate() {
        if captured.transaction_index as usize != index
            || captured.key != keys[index]
            || snapshot
                .accounts
                .iter()
                .find(|(key, _)| *key == captured.key)
                .map(|(_, account)| account)
                != Some(&captured.pre_account)
        {
            return Err("captured pre-state mismatch");
        }
    }
    let effect: CheckpointTransactionEffectV1 =
        borsh::from_slice(&page.transaction_effect).map_err(|_| "transaction effect encoding")?;
    let execution = snapshot.reexecute(transaction.clone());
    let BankTxnProcessingResult::Processed {
        result: Ok(ProcessedTransaction::Executed(executed)),
        ..
    } = &execution
    else {
        return Err("re-execution failed");
    };
    if executed.execution_details.status.is_err()
        || executed.execution_details.executed_units != effect.executed_units
        || u64::from(executed.loaded_transaction.loaded_accounts_data_size)
            != capture.loaded_accounts_data_size
        || executed.loaded_transaction.fee_details.transaction_fee() != capture.transaction_fee
        || executed.loaded_transaction.fee_details.prioritization_fee()
            != capture.prioritization_fee
        || executed.loaded_transaction.accounts.len() != capture.accounts.len()
    {
        return Err("committed execution mismatch");
    }
    for captured in &capture.accounts {
        if executed
            .loaded_transaction
            .accounts
            .iter()
            .find(|(key, _)| *key == captured.key)
            .map(|(_, account)| account)
            != Some(&captured.post_account)
        {
            return Err("committed post-state mismatch");
        }
    }
    let program = snapshot
        .accounts
        .iter()
        .find(|(key, _)| *key == keys[2])
        .ok_or("program missing")?;
    let UpgradeableLoaderState::Program {
        programdata_address,
    } = bincode::deserialize(program.1.data()).map_err(|_| "program state")?
    else {
        return Err("program state");
    };
    let programdata = snapshot
        .accounts
        .iter()
        .find(|(key, _)| *key == programdata_address)
        .ok_or("programdata missing")?;
    let elf = programdata
        .1
        .data()
        .get(UpgradeableLoaderState::size_of_programdata_metadata()..)
        .ok_or("program ELF missing")?
        .to_vec();
    let header = fixture_trace_header_v1(&page.transaction, bytes);
    let trace = build_transaction_trace_v1(header, &snapshot.accounts, &execution);
    let fixture = FullTransactionFixtureV1 {
        transaction: transaction.clone(),
        transaction_bytes: page.transaction.clone(),
        ordered_account_keys: keys.clone(),
        accounts: snapshot.accounts.clone(),
        program_id: keys[2],
        programdata_id: programdata_address,
        program_elf: elf,
        fee_payer: keys[0],
        target: keys[1],
        expected_fee_payer: capture.accounts[0].post_account.clone(),
        expected_target: capture.accounts[1].post_account.clone(),
        blockhash_queue: snapshot.blockhash_queue.clone(),
        feature_set: snapshot.feature_set(),
        fee_rate_governor: snapshot.fee_rate_governor.clone(),
    };
    let mut witness = assemble_replay_witness_v1(ExecutedFullTransactionFixtureV1 {
        fixture,
        execution,
        trace,
    })
    .map_err(|_| "unsupported replay trace")?;
    witness.session_context = context.session_context;
    witness.er_slot = artifact.checkpoint.er_slot;
    witness.step_index = page.step_index.into();
    witness.runtime.agave_revision = context.agave_revision;
    witness.runtime.northstar_revision = context.northstar_revision;
    witness.runtime.vm_config_hash = context.vm_config_hash;
    witness.runtime.syscall_registry_hash = context.syscall_registry_hash;
    witness.runtime.feature_set_hash = hash(
        &bincode::serialize(&(&snapshot.active_features, &snapshot.inactive_features))
            .map_err(|_| "feature encoding")?,
    )
    .to_bytes();
    witness.runtime.recent_blockhashes = vec![transaction.message.recent_blockhash().to_bytes()];
    witness.runtime.lamports_per_signature = snapshot.lamports_per_signature;
    // Replay v2 uses this field to bind readonly L1 observations, not ER bank time.
    witness.runtime.slot = page
        .readonly_l1_values
        .first()
        .ok_or("readonly observation missing")?
        .value
        .observed_l1_slot;
    witness.checkpoint = CheckpointBindingV1 {
        pre_state_accounts: convert(&page.pre_state_accounts)?,
        post_state_accounts: convert(&page.post_state_accounts)?,
        transaction_effect: page.transaction_effect.clone(),
        transaction_effect_path: convert(&page.transaction_effect_path)?,
        checkpoint_transaction_effect_root: artifact.checkpoint.transaction_effect_root,
        readonly_l1_values: convert(&page.readonly_l1_values)?,
        readonly_l1_root: artifact.checkpoint.readonly_l1_root,
        settlement_effects: convert(&page.settlement_effects)?,
        settlement_effect_root: artifact.checkpoint.effect_commitment,
    };
    set_trace_hash(&mut witness);
    let public = replay(&witness).map_err(|_| "replay relation rejected")?;
    if public_inputs_bytes(public) != *expected_public_inputs {
        return Err("checkpoint public inputs mismatch");
    }
    Ok(witness)
}
