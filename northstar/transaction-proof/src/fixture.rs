use {
    crate::{
        replay, AccountWitnessV1, ProgramWordV1, ReplayError, ReplayWitnessV1, ResultWitnessV1,
        RuntimeWitnessV1, VmRowV1, SBPF_VERSION_V0, TRACE_SCHEMA_VERSION_V1, WITNESS_MAGIC_V1,
        WITNESS_VERSION_V1,
    },
    northstar_zk_types::{ER_STEP_PROOF_KIND_FULL_TRANSACTION, ER_STEP_PROOF_VERSION_V1},
    solana_account::ReadableAccount,
    solana_program_runtime::invoke_context::VmExecutionTrace,
    solana_runtime::conformance::{
        proof_fixture::{
            execute_full_transaction_fixture_v1, ExecutedFullTransactionFixtureV1,
            TRANSACTION_FEE_V1,
        },
        txn::BankTxnProcessingResult,
    },
    solana_sha256_hasher::hash,
    solana_svm::transaction_processing_result::ProcessedTransaction,
    solana_svm_transaction::svm_message::SVMMessage,
};

const REVISION: [u8; 20] = [
    0x9c, 0x73, 0xb8, 0x41, 0x45, 0x47, 0x1b, 0x22, 0xd0, 0x0b, 0x01, 0x69, 0x84, 0x29, 0x3e, 0x04,
    0x26, 0x35, 0xad, 0xe9,
];

pub fn build_replay_witness_v1() -> Result<ReplayWitnessV1, ReplayError> {
    let executed = execute_full_transaction_fixture_v1();
    executed.assert_expected_success();
    build_from_execution(executed)
}

fn build_from_execution(
    executed: ExecutedFullTransactionFixtureV1,
) -> Result<ReplayWitnessV1, ReplayError> {
    let BankTxnProcessingResult::Processed {
        result: Ok(ProcessedTransaction::Executed(transaction)),
        runtime_transaction,
    } = &executed.execution
    else {
        return Err(ReplayError::Outcome);
    };
    let details = &transaction.execution_details;
    let vm_trace = details.vm_traces.first().ok_or(ReplayError::Trace)?;
    if details.vm_traces.len() != 1 {
        return Err(ReplayError::Trace);
    }
    let summary = executed.trace.summary().map_err(|_| ReplayError::Outcome)?;

    let pre_accounts = executed
        .fixture
        .ordered_account_keys
        .iter()
        .enumerate()
        .filter(|(index, _)| runtime_transaction.is_writable(*index))
        .map(|(index, key)| {
            let account = executed
                .fixture
                .accounts
                .iter()
                .find(|(candidate, _)| candidate == key)
                .ok_or(ReplayError::Accounts)?;
            Ok(account_witness(
                *key,
                &account.1,
                runtime_transaction.is_signer(index),
                true,
                runtime_transaction.is_invoked(index),
            ))
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;
    let post_accounts = pre_accounts
        .iter()
        .map(|pre| {
            let (_, account) = transaction
                .loaded_transaction
                .accounts
                .iter()
                .find(|(key, _)| key.to_bytes() == pre.key)
                .ok_or(ReplayError::Accounts)?;
            Ok(account_witness(
                solana_pubkey::Pubkey::new_from_array(pre.key),
                account,
                pre.signer,
                pre.writable,
                pre.invoked,
            ))
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;
    let readonly_accounts = executed
        .fixture
        .ordered_account_keys
        .iter()
        .enumerate()
        .filter(|(index, _)| !runtime_transaction.is_writable(*index))
        .map(|(index, key)| {
            let (_, account) = transaction
                .loaded_transaction
                .accounts
                .iter()
                .find(|(candidate, _)| candidate == key)
                .ok_or(ReplayError::Accounts)?;
            Ok(account_witness(
                *key,
                account,
                runtime_transaction.is_signer(index),
                false,
                runtime_transaction.is_invoked(index),
            ))
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;

    let signature = executed.fixture.transaction.signatures[0]
        .as_ref()
        .try_into()
        .unwrap();
    let message_bytes = executed.fixture.transaction.message.serialize();
    let signer = executed.fixture.ordered_account_keys[0].to_bytes();
    let instruction_data = executed.fixture.transaction.message.instructions()[0]
        .data
        .clone();
    let event_tags = executed
        .trace
        .events
        .iter()
        .map(|event| match event {
            northstar_zk_types::trace::TraceEventV1::ProcessorStage { .. } => 1,
            northstar_zk_types::trace::TraceEventV1::AccountState { .. } => 2,
            northstar_zk_types::trace::TraceEventV1::InstructionBoundary { .. } => 3,
            northstar_zk_types::trace::TraceEventV1::Syscall { .. } => 4,
            northstar_zk_types::trace::TraceEventV1::VmInvocation { .. } => 5,
            northstar_zk_types::trace::TraceEventV1::TransactionOutcome { .. } => 6,
        })
        .collect();
    let program_words = program_words(vm_trace);
    let vm_rows = vm_trace
        .rows
        .iter()
        .map(|row| VmRowV1 {
            registers: row.registers,
            instruction: row.instruction,
            syscall_key: row.syscall_key.unwrap_or_default(),
        })
        .collect();

    let witness = ReplayWitnessV1 {
        magic: WITNESS_MAGIC_V1,
        version: WITNESS_VERSION_V1,
        proof_kind: ER_STEP_PROOF_KIND_FULL_TRANSACTION,
        proof_version: ER_STEP_PROOF_VERSION_V1,
        trace_schema_version: TRACE_SCHEMA_VERSION_V1,
        session_context: b"northstar-proof-spike-session-v1".to_vec(),
        er_slot: 20,
        step_index: 1,
        transaction_bytes: executed.fixture.transaction_bytes,
        message_bytes,
        signature,
        signer,
        recent_blockhash: executed
            .fixture
            .transaction
            .message
            .recent_blockhash()
            .to_bytes(),
        instruction_data,
        pre_accounts,
        post_accounts,
        rollback_accounts: Vec::new(),
        readonly_accounts,
        runtime: RuntimeWitnessV1 {
            agave_revision: REVISION,
            northstar_revision: REVISION,
            feature_set_hash: hash(b"agave-all-enabled-except-disable-sbpf-v0").to_bytes(),
            recent_blockhashes: vec![[240; 32], [241; 32]],
            lamports_per_signature: TRANSACTION_FEE_V1,
            slot: 20,
            sbpf_version: SBPF_VERSION_V0,
            vm_config_hash: hash(b"agave-vm-config-v0-default").to_bytes(),
            syscall_registry_hash: hash(b"agave-syscalls-v0-sol-memcpy-only-fixture").to_bytes(),
            program_id: executed.fixture.program_id.to_bytes(),
            programdata_id: executed.fixture.programdata_id.to_bytes(),
            program_elf: executed.fixture.program_elf,
            program_hash: vm_trace.program_hash,
        },
        event_tags,
        program_words,
        vm_rows,
        program_input_before: vm_trace.program_input_before.clone(),
        program_input_after: vm_trace.program_input_after.clone(),
        stack_after: vm_trace.stack_after.clone(),
        heap_after: vm_trace.heap_after.clone(),
        compute_units_before: vm_trace.compute_units_before,
        compute_units_after: vm_trace.compute_units_after,
        trace_hash: [0; 32],
        result: ResultWitnessV1 {
            executed_success: details.status.is_ok(),
            transaction_error: summary.transaction_error,
            instruction_error: summary.instruction_error,
            instruction_index: summary.instruction_index,
            custom_error: summary.custom_error,
            executed_units: summary.executed_units,
            loaded_accounts_data_size: summary.loaded_accounts_data_size,
            transaction_fee: summary.transaction_fee,
            prioritization_fee: summary.prioritization_fee,
            return_data: summary.return_data,
            log_commitment: summary.log_commitment,
        },
    };
    let mut witness = witness;
    crate::set_trace_hash(&mut witness);
    replay(&witness)?;
    Ok(witness)
}

fn account_witness(
    key: solana_pubkey::Pubkey,
    account: &solana_account::AccountSharedData,
    signer: bool,
    writable: bool,
    invoked: bool,
) -> AccountWitnessV1 {
    AccountWitnessV1 {
        key: key.to_bytes(),
        signer,
        writable,
        invoked,
        lamports: account.lamports(),
        owner: account.owner().to_bytes(),
        executable: account.executable(),
        rent_epoch: account.rent_epoch(),
        data: account.data().to_vec(),
    }
}

fn program_words(trace: &VmExecutionTrace) -> Vec<ProgramWordV1> {
    let mut words = std::collections::BTreeMap::new();
    for row in &trace.rows {
        let pc = row.registers[11];
        words.entry(pc).or_insert_with(|| ProgramWordV1 {
            pc,
            instruction: row.instruction,
            lddw_high: 0,
        });
    }
    let lddw_pcs = words
        .values()
        .filter(|word| word.instruction[0] == 0x18)
        .map(|word| word.pc)
        .collect::<Vec<_>>();
    for pc in lddw_pcs {
        let next_pc = pc.checked_add(2).expect("fixture program counter fits");
        let high = trace
            .rows
            .iter()
            .find(|row| row.registers[11] == next_pc)
            .map(|next| {
                let instruction = words.get(&pc).unwrap().instruction;
                let destination = usize::from(instruction[1] & 0x0f);
                (next.registers[destination] >> 32) as u32
            })
            .unwrap_or_default();
        words.get_mut(&pc).unwrap().lddw_high = high;
    }
    words.into_values().collect()
}

#[cfg(test)]
mod tests {
    use {super::*, crate::encode_witness};

    #[test]
    fn real_fixture_builds_and_replays_deterministically() {
        let first = build_replay_witness_v1().unwrap();
        let second = build_replay_witness_v1().unwrap();
        assert_eq!(
            encode_witness(&first).unwrap(),
            encode_witness(&second).unwrap()
        );
        assert_eq!(replay(&first).unwrap(), replay(&second).unwrap());
    }

    fn assert_rejected(
        name: &str,
        mut witness: ReplayWitnessV1,
        mutate: impl FnOnce(&mut ReplayWitnessV1),
    ) {
        let expected = replay(&witness).unwrap();
        mutate(&mut witness);
        let actual = replay(&witness);
        assert!(
            actual.is_err() || actual.unwrap() != expected,
            "mutation accepted: {name}"
        );
    }

    #[test]
    fn mutations_fail_closed() {
        let witness = build_replay_witness_v1().unwrap();
        assert_rejected("wire", witness.clone(), |value| {
            value.transaction_bytes[10] ^= 1
        });
        assert_rejected("signature", witness.clone(), |value| {
            value.signature[0] ^= 1
        });
        assert_rejected("ELF", witness.clone(), |value| {
            value.runtime.program_elf[0] ^= 1
        });
        assert_rejected("program hash", witness.clone(), |value| {
            value.runtime.program_hash[0] ^= 1
        });
        assert_rejected("instruction", witness.clone(), |value| {
            value.vm_rows[0].instruction[0] ^= 1
        });
        assert_rejected("register", witness.clone(), |value| {
            value.vm_rows[0].registers[0] ^= 1
        });
        assert_rejected("PC", witness.clone(), |value| {
            value.vm_rows[0].registers[11] ^= 1
        });
        assert_rejected("memory", witness.clone(), |value| {
            value.program_input_after[0] ^= 1
        });
        assert_rejected("fee account", witness.clone(), |value| {
            value.pre_accounts[0].lamports += 1
        });
        assert_rejected("pre data", witness.clone(), |value| {
            value.pre_accounts[1].data[0] ^= 1
        });
        assert_rejected("post data", witness.clone(), |value| {
            value.post_accounts[1].data[0] ^= 1
        });
        assert_rejected("fee", witness.clone(), |value| {
            value.result.transaction_fee += 1
        });
        assert_rejected("compute", witness.clone(), |value| {
            value.compute_units_after += 1
        });
        assert_rejected("units", witness.clone(), |value| {
            value.result.executed_units += 1
        });
        assert_rejected("blockhash", witness.clone(), |value| {
            value.runtime.recent_blockhashes.clear()
        });
        assert_rejected("outcome", witness.clone(), |value| {
            value.result.executed_success = false
        });
        assert_rejected("missing outcome", witness.clone(), |value| {
            value.event_tags.pop();
        });
        assert_rejected("duplicate outcome", witness.clone(), |value| {
            value.event_tags.push(6)
        });
        assert_rejected("unsupported opcode", witness.clone(), |value| {
            value.vm_rows[0].instruction[0] = 0xff
        });
        assert_rejected("syscall", witness.clone(), |value| {
            value.vm_rows[0].syscall_key = 1
        });
        assert_rejected("extra row", witness.clone(), |value| {
            value.vm_rows.push(value.vm_rows[0].clone())
        });
        let mut encoded = encode_witness(&witness).unwrap();
        encoded.push(0);
        assert!(crate::decode_witness(&encoded).is_err());
    }
}
