use {
    super::{
        trace::{build_transaction_trace_v1, fixture_trace_header_v1},
        txn::{BankTxnProcessingResult, execute_txn_with_trace},
    },
    agave_feature_set::{FeatureSet, disable_sbpf_v0_execution, set_exempt_rent_epoch_max},
    solana_account::{AccountSharedData, ReadableAccount, WritableAccount},
    solana_accounts_db::blockhash_queue::BlockhashQueue,
    solana_clock::Clock,
    solana_epoch_schedule::EpochSchedule,
    solana_fee_calculator::FeeRateGovernor,
    solana_hash::Hash,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::keypair_from_seed,
    solana_loader_v3_interface::state::UpgradeableLoaderState,
    solana_pubkey::Pubkey,
    solana_sdk_ids::{bpf_loader_upgradeable, native_loader, sysvar},
    solana_sha256_hasher::hash,
    solana_signer::Signer,
    solana_slot_hashes::SlotHashes,
    solana_svm::transaction_processing_result::{
        ProcessedTransaction, TransactionProcessingResultExtensions,
    },
    solana_transaction::{Transaction, versioned::VersionedTransaction},
    std::{fs, sync::Arc},
};

pub const TRANSACTION_FEE_V1: u64 = 5_000;
pub const FEE_PAYER_LAMPORTS_V1: u64 = 10_000_000;
pub const TARGET_LAMPORTS_V1: u64 = 1_000_000;
pub const TARGET_DATA_LEN_V1: usize = 8;
pub const TARGET_WRITTEN_BYTE_V1: u8 = 100;
pub const RECENT_BLOCKHASH_V1: Hash = Hash::new_from_array([241; 32]);

pub struct FullTransactionFixtureV1 {
    pub transaction: VersionedTransaction,
    pub transaction_bytes: Vec<u8>,
    pub ordered_account_keys: Vec<Pubkey>,
    pub accounts: Vec<(Pubkey, AccountSharedData)>,
    pub program_id: Pubkey,
    pub programdata_id: Pubkey,
    pub program_elf: Vec<u8>,
    pub fee_payer: Pubkey,
    pub target: Pubkey,
    pub expected_fee_payer: AccountSharedData,
    pub expected_target: AccountSharedData,
    pub blockhash_queue: BlockhashQueue,
    pub feature_set: FeatureSet,
    pub fee_rate_governor: FeeRateGovernor,
}

pub struct ExecutedFullTransactionFixtureV1 {
    pub fixture: FullTransactionFixtureV1,
    pub execution: BankTxnProcessingResult,
    pub trace: northstar_zk_types::trace::TransactionTraceV1,
}

pub fn full_transaction_fixture_v1() -> FullTransactionFixtureV1 {
    full_transaction_fixture(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../svm/tests/example-programs/write-to-account/write_to_account_program.so"
        ),
        b"northstar-full-transaction-proof-v1",
        &[1],
        true,
    )
}

pub fn full_transaction_rollback_fixture_v1() -> FullTransactionFixtureV1 {
    full_transaction_fixture(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../svm/tests/example-programs/write-then-fail/write_then_fail_program.so"
        ),
        b"northstar-full-transaction-rollback-v1",
        &[],
        false,
    )
}

fn full_transaction_fixture(
    program_path: &str,
    program_seed: &[u8],
    instruction_data: &[u8],
    expect_write: bool,
) -> FullTransactionFixtureV1 {
    let signer = keypair_from_seed(&[42; 32]).expect("fixed seed is valid");
    let fee_payer = signer.pubkey();
    let target = Pubkey::new_from_array([43; 32]);
    let program_elf = fs::read(program_path).expect("tracked proof fixture exists");
    let program_id = Pubkey::new_from_array(hash(program_seed).to_bytes());
    let (programdata_id, program, programdata) = deploy_program(program_id, program_elf.clone());

    let transaction = VersionedTransaction::from(Transaction::new_signed_with_payer(
        &[Instruction::new_with_bytes(
            program_id,
            instruction_data,
            vec![AccountMeta::new(target, false)],
        )],
        Some(&fee_payer),
        &[&signer],
        RECENT_BLOCKHASH_V1,
    ));
    let transaction_bytes = bincode::serialize(&transaction).expect("fixed transaction encodes");
    let ordered_account_keys = transaction.message.static_account_keys().to_vec();

    let mut blockhash_queue = BlockhashQueue::default();
    blockhash_queue.register_hash(&Hash::new_from_array([240; 32]), TRANSACTION_FEE_V1);
    blockhash_queue.register_hash(&RECENT_BLOCKHASH_V1, TRANSACTION_FEE_V1);
    let feature_set = proof_feature_set();
    let fee_rate_governor = proof_fee_rate_governor();

    let target_account = account(
        TARGET_LAMPORTS_V1,
        vec![0; TARGET_DATA_LEN_V1],
        program_id,
        false,
    );
    let accounts = vec![
        (
            fee_payer,
            account(FEE_PAYER_LAMPORTS_V1, vec![], Pubkey::default(), false),
        ),
        (target, target_account.clone()),
        (program_id, program),
        (programdata_id, programdata),
        clock_sysvar_account(),
        epoch_schedule_sysvar_account(),
        rent_sysvar_account(),
        slot_hashes_sysvar_account(),
    ];
    let expected_fee_payer = account(
        FEE_PAYER_LAMPORTS_V1 - TRANSACTION_FEE_V1,
        vec![],
        Pubkey::default(),
        false,
    );
    let mut expected_target = target_account;
    if expect_write {
        expected_target.data_as_mut_slice()[0] = TARGET_WRITTEN_BYTE_V1;
    }

    FullTransactionFixtureV1 {
        transaction,
        transaction_bytes,
        ordered_account_keys,
        accounts,
        program_id,
        programdata_id,
        program_elf,
        fee_payer,
        target,
        expected_fee_payer,
        expected_target,
        blockhash_queue,
        feature_set,
        fee_rate_governor,
    }
}

pub fn execute_full_transaction_fixture_v1() -> ExecutedFullTransactionFixtureV1 {
    execute_fixture(full_transaction_fixture_v1())
}

pub fn execute_full_transaction_rollback_fixture_v1() -> ExecutedFullTransactionFixtureV1 {
    execute_fixture(full_transaction_rollback_fixture_v1())
}

fn execute_fixture(fixture: FullTransactionFixtureV1) -> ExecutedFullTransactionFixtureV1 {
    let execution = execute_txn_with_trace(
        &fixture.accounts,
        fixture.feature_set.clone(),
        fixture.blockhash_queue.clone(),
        fixture.fee_rate_governor.clone(),
        0,
        fixture.transaction.clone(),
        true,
    );
    let header = fixture_trace_header_v1(&fixture.transaction_bytes, b"proof-fixture-runtime-v1");
    let trace = build_transaction_trace_v1(header, &fixture.accounts, &execution);
    ExecutedFullTransactionFixtureV1 {
        fixture,
        execution,
        trace,
    }
}

impl ExecutedFullTransactionFixtureV1 {
    pub fn assert_expected_success(&self) {
        assert!(match &self.execution {
            BankTxnProcessingResult::Processed { result, .. } => {
                result.was_processed_with_successful_result()
            }
            BankTxnProcessingResult::FailedVerification(_) => false,
        });
        let BankTxnProcessingResult::Processed {
            result: Ok(ProcessedTransaction::Executed(transaction)),
            ..
        } = &self.execution
        else {
            panic!("fixture did not execute");
        };
        let loaded = &transaction.loaded_transaction.accounts;
        let fee_payer = loaded
            .iter()
            .find(|(key, _)| *key == self.fixture.fee_payer)
            .expect("fee payer loaded");
        let target = loaded
            .iter()
            .find(|(key, _)| *key == self.fixture.target)
            .expect("target loaded");
        assert_eq!(fee_payer.1, self.fixture.expected_fee_payer);
        assert_eq!(target.1, self.fixture.expected_target);
    }

    pub fn assert_expected_rollback(&self) {
        let summary = self.trace.summary().expect("single outcome");
        assert_eq!(
            summary.outcome,
            northstar_zk_types::trace::TransactionOutcomeV1::ExecutedFailure
        );
        assert_eq!(summary.transaction_fee, TRANSACTION_FEE_V1);
        assert!(summary.post_accounts.is_empty());
        assert!(summary.rollback_accounts.iter().any(|effect| {
            effect.account.address == self.fixture.fee_payer.to_bytes()
                && effect.account.lamports == self.fixture.expected_fee_payer.lamports()
        }));
        assert!(!summary.rollback_accounts.iter().any(|effect| {
            effect.account.address == self.fixture.target.to_bytes()
                && effect.account.data.first() == Some(&TARGET_WRITTEN_BYTE_V1)
        }));
    }
}

fn proof_feature_set() -> FeatureSet {
    let mut feature_set = FeatureSet::all_enabled();
    feature_set.activate(&set_exempt_rent_epoch_max::id(), 0);
    feature_set.deactivate(&disable_sbpf_v0_execution::id());
    feature_set
}

fn proof_fee_rate_governor() -> FeeRateGovernor {
    FeeRateGovernor {
        lamports_per_signature: TRANSACTION_FEE_V1,
        target_lamports_per_signature: 0,
        target_signatures_per_slot: 0,
        min_lamports_per_signature: 0,
        max_lamports_per_signature: 0,
        burn_percent: 0,
    }
}

fn deploy_program(
    program_id: Pubkey,
    program_elf: Vec<u8>,
) -> (Pubkey, AccountSharedData, AccountSharedData) {
    let programdata_id = Pubkey::new_from_array(hash(program_id.as_ref()).to_bytes());
    let program = account(
        25,
        bincode::serialize(&UpgradeableLoaderState::Program {
            programdata_address: programdata_id,
        })
        .expect("loader state encodes"),
        bpf_loader_upgradeable::id(),
        true,
    );
    let mut data = bincode::serialize(&UpgradeableLoaderState::ProgramData {
        slot: 0,
        upgrade_authority_address: None,
    })
    .expect("loader state encodes");
    data.resize(UpgradeableLoaderState::size_of_programdata_metadata(), 0);
    data.extend_from_slice(&program_elf);
    let programdata = account(25, data, bpf_loader_upgradeable::id(), false);
    (programdata_id, program, programdata)
}

fn account(lamports: u64, data: Vec<u8>, owner: Pubkey, executable: bool) -> AccountSharedData {
    AccountSharedData::create_from_existing_shared_data(
        lamports,
        Arc::new(data),
        owner,
        executable,
        u64::MAX,
    )
}

fn sysvar_account<T: serde::Serialize>(id: Pubkey, state: &T) -> (Pubkey, AccountSharedData) {
    (
        id,
        account(
            1,
            bincode::serialize(state).expect("sysvar encodes"),
            native_loader::id(),
            false,
        ),
    )
}

fn clock_sysvar_account() -> (Pubkey, AccountSharedData) {
    sysvar_account(
        sysvar::clock::id(),
        &Clock {
            slot: 20,
            epoch_start_timestamp: 1_720_556_855,
            epoch: 0,
            leader_schedule_epoch: 1,
            unix_timestamp: 1_720_556_855,
        },
    )
}

fn epoch_schedule_sysvar_account() -> (Pubkey, AccountSharedData) {
    sysvar_account(
        sysvar::epoch_schedule::id(),
        &EpochSchedule {
            slots_per_epoch: 432_000,
            leader_schedule_slot_offset: 432_000,
            warmup: true,
            first_normal_epoch: 14,
            first_normal_slot: 524_256,
        },
    )
}

fn rent_sysvar_account() -> (Pubkey, AccountSharedData) {
    sysvar_account(sysvar::rent::id(), &solana_rent::Rent::default())
}

fn slot_hashes_sysvar_account() -> (Pubkey, AccountSharedData) {
    (
        sysvar::slot_hashes::id(),
        account(
            1,
            wincode::serialize(&SlotHashes::default()).expect("slot hashes encode"),
            native_loader::id(),
            false,
        ),
    )
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        northstar_zk_types::trace::{TraceEventV1, TransactionOutcomeV1},
    };

    #[test]
    fn fixture_is_real_deterministic_signed_bank_execution() {
        let first = execute_full_transaction_fixture_v1();
        let second = execute_full_transaction_fixture_v1();
        first.assert_expected_success();
        second.assert_expected_success();
        assert_eq!(first.fixture.transaction.signatures.len(), 1);
        assert_ne!(
            first.fixture.transaction.signatures[0],
            solana_signature::Signature::default()
        );
        assert_eq!(first.fixture.ordered_account_keys.len(), 3);
        assert_eq!(
            first.trace.canonical_bytes().unwrap(),
            second.trace.canonical_bytes().unwrap()
        );
        let mut opcodes = std::collections::BTreeSet::new();
        let mut rows = 0;
        for event in &first.trace.events {
            if let TraceEventV1::VmInvocation { rows: vm_rows, .. } = event {
                rows += vm_rows.len();
                opcodes.extend(vm_rows.iter().map(|row| row.instruction[0]));
            }
            if let TraceEventV1::Syscall {
                vm_row,
                function_key,
                ..
            } = event
            {
                println!("syscall row={vm_row} key={function_key:#x}");
            }
        }
        let syscalls = first
            .trace
            .events
            .iter()
            .filter(|event| matches!(event, TraceEventV1::Syscall { .. }))
            .count();
        let trace_bytes = first.trace.canonical_bytes().unwrap();
        let summary = first.trace.summary().unwrap();
        println!(
            "trace_bytes={} trace_sha256={} events={} rows={} syscalls={} units={} fee={} \
             opcodes={opcodes:x?}",
            trace_bytes.len(),
            hash(&trace_bytes),
            first.trace.events.len(),
            rows,
            syscalls,
            summary.executed_units,
            summary.transaction_fee,
        );
        assert!(first.trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::TransactionOutcome {
                outcome: TransactionOutcomeV1::ExecutedSuccess,
                transaction_fee: TRANSACTION_FEE_V1,
                ..
            }
        )));
    }

    #[test]
    fn paired_write_then_fail_fixture_rolls_back_mutation() {
        let first = execute_full_transaction_rollback_fixture_v1();
        let second = execute_full_transaction_rollback_fixture_v1();
        first.assert_expected_rollback();
        second.assert_expected_rollback();
        assert_eq!(
            first.trace.canonical_bytes().unwrap(),
            second.trace.canonical_bytes().unwrap()
        );
        let trace_bytes = first.trace.canonical_bytes().unwrap();
        let summary = first.trace.summary().unwrap();
        let rows = first
            .trace
            .events
            .iter()
            .map(|event| match event {
                TraceEventV1::VmInvocation { rows, .. } => rows.len(),
                _ => 0,
            })
            .sum::<usize>();
        println!(
            "rollback_trace_bytes={} rollback_trace_sha256={} rows={} units={} fee={}",
            trace_bytes.len(),
            hash(&trace_bytes),
            rows,
            summary.executed_units,
            summary.transaction_fee,
        );
    }
}
