pub use solana_runtime::bank::replay_execution::{
    BankTxnProcessingResult, execute_er_txn_with_trace, execute_txn, execute_txn_with_trace,
};

#[cfg(test)]
mod tests {
    #[cfg(feature = "conformance")]
    use {
        super::execute_txn_with_trace,
        crate::trace::{build_transaction_trace_v1, fixture_trace_header_v1},
        northstar_zk_types::trace::{
            AccountPhaseV1, InstructionBoundaryV1, ProcessorStageV1, StageOutcomeV1, TraceEventV1,
            TransactionOutcomeV1, TransactionTraceV1,
        },
        solana_program_option::COption,
        solana_program_pack::Pack,
        solana_system_interface::instruction::transfer,
        solana_transaction_error::TransactionError,
        spl_token_2022_interface::{
            instruction::transfer_checked,
            state::{Account as TokenAccount, AccountState, Mint},
        },
    };
    use {
        super::{BankTxnProcessingResult, execute_txn},
        agave_feature_set::{FeatureSet, disable_sbpf_v0_execution, set_exempt_rent_epoch_max},
        solana_account::{AccountSharedData, ReadableAccount},
        solana_accounts_db::blockhash_queue::BlockhashQueue,
        solana_address_lookup_table_interface::state::{AddressLookupTable, LookupTableMeta},
        solana_clock::Clock,
        solana_epoch_schedule::EpochSchedule,
        solana_fee_calculator::FeeRateGovernor,
        solana_hash::Hash,
        solana_loader_v3_interface::state::UpgradeableLoaderState,
        solana_message::{
            MessageHeader, VersionedMessage,
            compiled_instruction::CompiledInstruction,
            legacy,
            v0::{self, MessageAddressTableLookup},
        },
        solana_pubkey::Pubkey,
        solana_sdk_ids::{bpf_loader_upgradeable, native_loader, sysvar},
        solana_sha256_hasher::hash,
        solana_signature::Signature,
        solana_slot_hashes::SlotHashes,
        solana_svm::transaction_processing_result::{
            ProcessedTransaction, TransactionProcessingResultExtensions,
        },
        solana_transaction::versioned::VersionedTransaction,
        std::{borrow::Cow, fs, path::PathBuf, sync::Arc},
    };

    /// All features enabled except `disable_sbpf_v0_execution`, so the v0
    /// `complex-transfer` program loads. `set_exempt_rent_epoch_max` is forced on
    /// to match the accounts' `u64::MAX` rent epoch.
    fn feature_set() -> FeatureSet {
        let mut feature_set = FeatureSet::all_enabled();
        feature_set.activate(&set_exempt_rent_epoch_max::id(), 0);
        feature_set.deactivate(&disable_sbpf_v0_execution::id());
        feature_set
    }

    fn fee_rate_governor() -> FeeRateGovernor {
        // Mirrors the proto path: only `lamports_per_signature` is set; the
        // targets/burn are zeroed (unlike `FeeRateGovernor::default()`).
        FeeRateGovernor {
            lamports_per_signature: 5000,
            target_lamports_per_signature: 0,
            target_signatures_per_slot: 0,
            min_lamports_per_signature: 0,
            max_lamports_per_signature: 0,
            burn_percent: 0,
        }
    }

    /// A blockhash queue with two registered hashes; returns the queue plus the
    /// most-recent blockhash to use as the message's `recent_blockhash`.
    fn blockhash_queue() -> (BlockhashQueue, Hash) {
        let mut queue = BlockhashQueue::default();
        queue.register_hash(&Hash::new_from_array([240; 32]), 5000);
        let recent = Hash::new_from_array([241; 32]);
        queue.register_hash(&recent, 5000);
        (queue, recent)
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

    fn empty_account(lamports: u64) -> AccountSharedData {
        account(lamports, vec![], Pubkey::default(), false)
    }

    fn sysvar_account<T: serde::Serialize>(id: Pubkey, state: &T) -> (Pubkey, AccountSharedData) {
        (
            id,
            account(
                1,
                bincode::serialize(state).unwrap(),
                native_loader::id(),
                false,
            ),
        )
    }

    fn clock_sysvar_account() -> (Pubkey, AccountSharedData) {
        let clock = Clock {
            slot: 20,
            epoch_start_timestamp: 1720556855,
            epoch: 0,
            leader_schedule_epoch: 1,
            unix_timestamp: 1720556855,
        };
        sysvar_account(sysvar::clock::id(), &clock)
    }

    fn epoch_schedule_sysvar_account() -> (Pubkey, AccountSharedData) {
        let epoch_schedule = EpochSchedule {
            slots_per_epoch: 432000,
            leader_schedule_slot_offset: 432000,
            warmup: true,
            first_normal_epoch: 14,
            first_normal_slot: 524256,
        };
        sysvar_account(sysvar::epoch_schedule::id(), &epoch_schedule)
    }

    fn rent_sysvar_account() -> (Pubkey, AccountSharedData) {
        sysvar_account(sysvar::rent::id(), &solana_rent::Rent::default())
    }

    fn slot_hashes_sysvar_account() -> (Pubkey, AccountSharedData) {
        (
            sysvar::slot_hashes::id(),
            account(
                1,
                wincode::serialize(&SlotHashes::default()).unwrap(),
                native_loader::id(),
                false,
            ),
        )
    }

    fn system_program_account() -> (Pubkey, AccountSharedData) {
        (
            solana_sdk_ids::system_program::id(),
            account(1, vec![], native_loader::id(), true),
        )
    }

    fn load_program(name: &str) -> Vec<u8> {
        let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        dir.push("../..");
        dir.push("svm");
        dir.push("tests");
        dir.push("example-programs");
        dir.push(name);
        dir.push(format!("{}_program.so", name.replace('-', "_")));
        fs::read(&dir).expect("program file not found")
    }

    #[cfg(feature = "conformance")]
    fn load_token_2022_program() -> Vec<u8> {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../..");
        path.push("program-binaries");
        path.push("src");
        path.push("programs");
        path.push("spl_token_2022-10.0.0.so");
        fs::read(path).expect("Token-2022 program file not found")
    }

    /// Build the program + programdata accounts for an upgradeable BPF program.
    fn deploy_program(name: &str) -> [(Pubkey, AccountSharedData); 2] {
        let mut program_seed = b"northstar-conformance-program:".to_vec();
        program_seed.extend_from_slice(name.as_bytes());
        let program_id = Pubkey::new_from_array(hash(&program_seed).to_bytes());
        deploy_program_bytes(program_id, load_program(name))
    }

    fn deploy_program_bytes(
        program_id: Pubkey,
        mut buffer: Vec<u8>,
    ) -> [(Pubkey, AccountSharedData); 2] {
        let mut program_data_seed = b"northstar-conformance-programdata:".to_vec();
        program_data_seed.extend_from_slice(program_id.as_ref());
        let program_data_id = Pubkey::new_from_array(hash(&program_data_seed).to_bytes());
        let program = account(
            25,
            bincode::serialize(&UpgradeableLoaderState::Program {
                programdata_address: program_data_id,
            })
            .unwrap(),
            bpf_loader_upgradeable::id(),
            true,
        );
        let state = UpgradeableLoaderState::ProgramData {
            slot: 0,
            upgrade_authority_address: None,
        };
        let mut header = bincode::serialize(&state).unwrap();
        let mut complement = vec![
            0;
            UpgradeableLoaderState::size_of_programdata_metadata()
                .saturating_sub(header.len())
        ];
        header.append(&mut complement);
        header.append(&mut buffer);
        let program_data = account(25, header, bpf_loader_upgradeable::id(), false);
        [(program_id, program), (program_data_id, program_data)]
    }

    /// Lamports of the writable account `pubkey` after execution, if the
    /// transaction executed successfully.
    fn writable_account_lamports(
        execution: &BankTxnProcessingResult,
        pubkey: &Pubkey,
    ) -> Option<u64> {
        match execution {
            BankTxnProcessingResult::Processed {
                result: Ok(ProcessedTransaction::Executed(executed_tx)),
                runtime_transaction,
            } => executed_tx
                .loaded_transaction
                .accounts
                .iter()
                .enumerate()
                .filter(|(index, _)| runtime_transaction.message().is_writable(*index))
                .find(|(_, (key, _))| key == pubkey)
                .map(|(_, (_, account))| account.lamports()),
            _ => None,
        }
    }

    fn return_data(execution: &BankTxnProcessingResult) -> Vec<u8> {
        match execution {
            BankTxnProcessingResult::Processed {
                result: Ok(ProcessedTransaction::Executed(executed_tx)),
                ..
            } => executed_tx
                .execution_details
                .return_data
                .as_ref()
                .map(|info| info.data.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn assert_executed_ok(execution: &BankTxnProcessingResult) {
        match execution {
            BankTxnProcessingResult::Processed { result, .. } => {
                assert!(result.was_processed_with_successful_result())
            }
            BankTxnProcessingResult::FailedVerification(err) => {
                panic!("transaction failed verification: {err:?}")
            }
        }
    }

    #[cfg(feature = "conformance")]
    fn traced_execution(
        accounts: &[(Pubkey, AccountSharedData)],
        blockhash_queue: BlockhashQueue,
        transaction: VersionedTransaction,
        verify_signatures: bool,
    ) -> (BankTxnProcessingResult, TransactionTraceV1) {
        let transaction_bytes = bincode::serialize(&transaction).unwrap();
        let execution = execute_txn_with_trace(
            accounts,
            feature_set(),
            blockhash_queue,
            fee_rate_governor(),
            0,
            transaction,
            verify_signatures,
        );
        let header = fixture_trace_header_v1(&transaction_bytes, b"all-features-v1");
        let trace = build_transaction_trace_v1(header, accounts, &execution);
        (execution, trace)
    }

    #[test]
    fn test_txn_execute_clock() {
        let [(program_id, program), (program_data_id, program_data)] =
            deploy_program("clock-sysvar");
        let fee_payer = Pubkey::new_unique();
        let (blockhash_queue, recent_blockhash) = blockhash_queue();

        let message = VersionedMessage::Legacy(legacy::Message {
            header: MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 0,
            },
            account_keys: vec![fee_payer, program_id],
            recent_blockhash,
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
                accounts: vec![],
                data: vec![],
            }],
        });
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message,
        };

        let accounts = vec![
            (fee_payer, empty_account(80000000)),
            (program_id, program),
            (program_data_id, program_data),
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
        ];

        let execution = execute_txn(
            &accounts,
            feature_set(),
            blockhash_queue,
            fee_rate_governor(),
            0,
            transaction,
        );

        assert_executed_ok(&execution);
        assert_eq!(return_data(&execution).len(), 8);
    }

    #[test]
    fn test_simple_transfer() {
        let [(program_id, program), (program_data_id, program_data)] =
            deploy_program("simple-transfer");
        let fee_payer = Pubkey::new_from_array([21; 32]);
        let sender = Pubkey::new_from_array([22; 32]);
        let recipient = Pubkey::new_from_array([23; 32]);
        let (blockhash_queue, recent_blockhash) = blockhash_queue();

        let message = VersionedMessage::V0(v0::Message {
            header: MessageHeader {
                num_required_signatures: 2,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 1,
            },
            account_keys: vec![fee_payer, sender, recipient, program_id, Pubkey::default()],
            recent_blockhash,
            instructions: vec![CompiledInstruction {
                program_id_index: 3,
                accounts: vec![1, 2, 4],
                data: vec![0, 0, 0, 0, 0, 0, 0, 10],
            }],
            address_table_lookups: vec![],
        });
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default(), Signature::default()],
            message,
        };

        let accounts = vec![
            (fee_payer, empty_account(10000000)),
            (recipient, empty_account(900000)),
            (sender, empty_account(900000)),
            (program_id, program),
            (program_data_id, program_data),
            system_program_account(),
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
            slot_hashes_sysvar_account(),
        ];

        #[cfg(feature = "conformance")]
        let execution = {
            let transaction_bytes = bincode::serialize(&transaction).unwrap();
            let first = execute_txn_with_trace(
                &accounts,
                feature_set(),
                blockhash_queue.clone(),
                fee_rate_governor(),
                0,
                transaction.clone(),
                false,
            );
            let second = execute_txn_with_trace(
                &accounts,
                feature_set(),
                blockhash_queue.clone(),
                fee_rate_governor(),
                0,
                transaction.clone(),
                false,
            );
            let header = fixture_trace_header_v1(&transaction_bytes, b"all-features-v1");
            let first_trace = build_transaction_trace_v1(header.clone(), &accounts, &first);
            let second_trace = build_transaction_trace_v1(header, &accounts, &second);
            let first_bytes = first_trace.canonical_bytes().unwrap();
            let second_bytes = second_trace.canonical_bytes().unwrap();
            assert_eq!(first_bytes, second_bytes);
            assert_eq!(hash(&first_bytes), hash(&second_bytes));
            assert_eq!(first_bytes.len(), 180_302);
            assert_eq!(
                hash(&first_bytes).to_bytes(),
                [
                    206, 36, 189, 166, 16, 199, 162, 43, 196, 85, 71, 28, 114, 220, 183, 69, 133,
                    151, 96, 153, 149, 8, 110, 164, 207, 133, 216, 138, 42, 13, 83, 44,
                ]
            );
            assert!(first_trace.events.iter().any(|event| matches!(
                event,
                TraceEventV1::VmInvocation { rows, memory, .. }
                    if !rows.is_empty() && memory.len() == 3
            )));
            assert!(
                first_trace
                    .events
                    .iter()
                    .any(|event| matches!(event, TraceEventV1::Syscall { .. }))
            );
            assert!(first_trace.events.iter().any(|event| matches!(
                event,
                TraceEventV1::InstructionBoundary {
                    boundary: InstructionBoundaryV1::Enter,
                    parent_invocation_id: Some(_),
                    ..
                }
            )));
            assert!(first_trace.events.iter().any(|event| matches!(
                event,
                TraceEventV1::AccountState {
                    phase: AccountPhaseV1::Post,
                    ..
                }
            )));
            assert!(first_trace.events.iter().any(|event| matches!(
                event,
                TraceEventV1::TransactionOutcome {
                    outcome: TransactionOutcomeV1::ExecutedSuccess,
                    transaction_fee: 10_000,
                    ..
                }
            )));
            let summary = first_trace.summary().unwrap();
            let BankTxnProcessingResult::Processed {
                result: Ok(ProcessedTransaction::Executed(first_transaction)),
                ..
            } = &first
            else {
                panic!("traced execution failed")
            };
            assert_eq!(
                summary.executed_units,
                first_transaction.execution_details.executed_units
            );
            assert_eq!(
                summary.loaded_accounts_data_size,
                u64::from(
                    first_transaction
                        .loaded_transaction
                        .loaded_accounts_data_size
                )
            );
            assert_eq!(
                summary.transaction_fee,
                first_transaction
                    .loaded_transaction
                    .fee_details
                    .transaction_fee()
            );
            assert_eq!(
                summary.prioritization_fee,
                first_transaction
                    .loaded_transaction
                    .fee_details
                    .prioritization_fee()
            );
            for effect in &summary.post_accounts {
                let (expected_address, expected_account) = &first_transaction
                    .loaded_transaction
                    .accounts[effect.transaction_index as usize];
                assert_eq!(effect.account.address, expected_address.to_bytes());
                assert_eq!(effect.account.lamports, expected_account.lamports());
                assert_eq!(effect.account.owner, expected_account.owner().to_bytes());
                assert_eq!(effect.account.data, expected_account.data());
            }
            let untraced = execute_txn(
                &accounts,
                feature_set(),
                blockhash_queue,
                fee_rate_governor(),
                0,
                transaction,
            );
            let BankTxnProcessingResult::Processed {
                result: Ok(ProcessedTransaction::Executed(untraced_transaction)),
                ..
            } = &untraced
            else {
                panic!("untraced execution failed")
            };
            assert!(untraced_transaction.execution_details.vm_traces.is_empty());
            assert_eq!(
                writable_account_lamports(&untraced, &sender),
                writable_account_lamports(&first, &sender)
            );
            first
        };
        #[cfg(not(feature = "conformance"))]
        let execution = execute_txn(
            &accounts,
            feature_set(),
            blockhash_queue,
            fee_rate_governor(),
            0,
            transaction,
        );

        assert_executed_ok(&execution);
        assert_eq!(writable_account_lamports(&execution, &sender), Some(899990));
        assert_eq!(
            writable_account_lamports(&execution, &recipient),
            Some(900010)
        );
    }

    #[cfg(feature = "conformance")]
    #[test]
    fn trace_rejects_bad_signature_before_execution() {
        let payer = Pubkey::new_unique();
        let (queue, recent_blockhash) = blockhash_queue();
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(legacy::Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 0,
                },
                account_keys: vec![payer],
                recent_blockhash,
                instructions: vec![],
            }),
        };
        let accounts = vec![
            (payer, empty_account(1_000_000)),
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
        ];
        let (execution, trace) = traced_execution(&accounts, queue, transaction, true);
        assert!(matches!(
            execution,
            BankTxnProcessingResult::FailedVerification(TransactionError::SignatureFailure)
        ));
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::ProcessorStage {
                stage: ProcessorStageV1::SignatureVerification,
                outcome: StageOutcomeV1::Failure,
                ..
            }
        )));
        assert!(
            !trace
                .events
                .iter()
                .any(|event| matches!(event, TraceEventV1::VmInvocation { .. }))
        );
    }

    #[cfg(feature = "conformance")]
    #[test]
    fn trace_records_stale_blockhash_as_bank_check_failure() {
        let payer = Pubkey::new_unique();
        let (queue, _) = blockhash_queue();
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(legacy::Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 0,
                },
                account_keys: vec![payer],
                recent_blockhash: Hash::new_unique(),
                instructions: vec![],
            }),
        };
        let accounts = vec![
            (payer, empty_account(1_000_000)),
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
        ];
        let (execution, trace) = traced_execution(&accounts, queue, transaction, false);
        assert!(matches!(
            execution,
            BankTxnProcessingResult::Processed {
                result: Err(TransactionError::BlockhashNotFound),
                ..
            }
        ));
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::ProcessorStage {
                stage: ProcessorStageV1::BankChecks,
                outcome: StageOutcomeV1::Failure,
                ..
            }
        )));
    }

    #[cfg(feature = "conformance")]
    #[test]
    fn trace_records_failed_sbf_and_rollback() {
        let [(program_id, program), (program_data_id, program_data)] =
            deploy_program("simple-transfer");
        let fee_payer = Pubkey::new_unique();
        let sender = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let (queue, recent_blockhash) = blockhash_queue();
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default(), Signature::default()],
            message: VersionedMessage::V0(v0::Message {
                header: MessageHeader {
                    num_required_signatures: 2,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 1,
                },
                account_keys: vec![fee_payer, sender, recipient, program_id, Pubkey::default()],
                recent_blockhash,
                instructions: vec![CompiledInstruction {
                    program_id_index: 3,
                    accounts: vec![1, 2, 4],
                    data: 1_000_000u64.to_be_bytes().to_vec(),
                }],
                address_table_lookups: vec![],
            }),
        };
        let accounts = vec![
            (fee_payer, empty_account(10_000_000)),
            (recipient, empty_account(900_000)),
            (sender, empty_account(900_000)),
            (program_id, program),
            (program_data_id, program_data),
            system_program_account(),
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
            slot_hashes_sysvar_account(),
        ];
        let (execution, trace) = traced_execution(&accounts, queue, transaction, false);
        assert!(matches!(
            execution,
            BankTxnProcessingResult::Processed {
                result: Ok(ProcessedTransaction::Executed(ref transaction)),
                ..
            } if transaction.execution_details.status.is_err()
        ));
        assert!(trace.events.iter().any(
            |event| matches!(event, TraceEventV1::VmInvocation { rows, .. } if !rows.is_empty())
        ));
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::AccountState {
                phase: AccountPhaseV1::Rollback,
                ..
            }
        )));
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::TransactionOutcome {
                outcome: TransactionOutcomeV1::ExecutedFailure,
                ..
            }
        )));
    }

    #[cfg(feature = "conformance")]
    #[test]
    fn trace_records_native_system_execution_without_vm_rows() {
        let payer = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let (queue, recent_blockhash) = blockhash_queue();
        let message = legacy::Message::new_with_blockhash(
            &[transfer(&payer, &recipient, 10)],
            Some(&payer),
            &recent_blockhash,
        );
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(message),
        };
        let accounts = vec![
            (payer, empty_account(1_000_000)),
            (recipient, empty_account(1)),
            system_program_account(),
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
        ];
        let (execution, trace) = traced_execution(&accounts, queue, transaction, false);
        assert_executed_ok(&execution);
        assert!(
            !trace
                .events
                .iter()
                .any(|event| matches!(event, TraceEventV1::VmInvocation { .. }))
        );
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::TransactionOutcome {
                outcome: TransactionOutcomeV1::ExecutedSuccess,
                ..
            }
        )));
    }

    #[cfg(feature = "conformance")]
    #[test]
    fn trace_records_noop_fee_payer_failure() {
        let missing_payer = Pubkey::new_unique();
        let (queue, recent_blockhash) = blockhash_queue();
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(legacy::Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 0,
                },
                account_keys: vec![missing_payer],
                recent_blockhash,
                instructions: vec![],
            }),
        };
        let accounts = vec![
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
        ];
        let (execution, trace) = traced_execution(&accounts, queue, transaction, false);
        assert!(matches!(
            execution,
            BankTxnProcessingResult::Processed {
                result: Ok(ProcessedTransaction::NoOp(_)),
                ..
            }
        ));
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::TransactionOutcome {
                outcome: TransactionOutcomeV1::NoOp,
                ..
            }
        )));
    }

    #[cfg(feature = "conformance")]
    #[test]
    fn trace_records_fees_only_program_load_failure() {
        let payer = Pubkey::new_unique();
        let missing_program = Pubkey::new_unique();
        let (queue, recent_blockhash) = blockhash_queue();
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(legacy::Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 1,
                },
                account_keys: vec![payer, missing_program],
                recent_blockhash,
                instructions: vec![CompiledInstruction {
                    program_id_index: 1,
                    accounts: vec![],
                    data: vec![],
                }],
            }),
        };
        let accounts = vec![
            (payer, empty_account(1_000_000)),
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
        ];
        let (execution, trace) = traced_execution(&accounts, queue, transaction, false);
        assert!(matches!(
            execution,
            BankTxnProcessingResult::Processed {
                result: Ok(ProcessedTransaction::FeesOnly(_)),
                ..
            }
        ));
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::TransactionOutcome {
                outcome: TransactionOutcomeV1::FeesOnly,
                transaction_fee: 5_000,
                ..
            }
        )));
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::AccountState {
                phase: AccountPhaseV1::Rollback,
                ..
            }
        )));
    }

    #[cfg(feature = "conformance")]
    #[test]
    fn trace_records_token_2022_transfer() {
        let program_id = spl_token_2022_interface::id();
        let [(program_id, program), (program_data_id, program_data)] =
            deploy_program_bytes(program_id, load_token_2022_program());
        let payer = Pubkey::new_from_array([31; 32]);
        let mint_id = Pubkey::new_from_array([32; 32]);
        let source_id = Pubkey::new_from_array([33; 32]);
        let destination_id = Pubkey::new_from_array([34; 32]);
        let mut mint_data = vec![0; Mint::LEN];
        Mint::pack(
            Mint {
                mint_authority: COption::Some(payer),
                supply: 100,
                decimals: 0,
                is_initialized: true,
                freeze_authority: COption::None,
            },
            &mut mint_data,
        )
        .unwrap();
        let mut source_data = vec![0; TokenAccount::LEN];
        TokenAccount::pack(
            TokenAccount {
                mint: mint_id,
                owner: payer,
                amount: 100,
                delegate: COption::None,
                state: AccountState::Initialized,
                is_native: COption::None,
                delegated_amount: 0,
                close_authority: COption::None,
            },
            &mut source_data,
        )
        .unwrap();
        let mut destination_data = vec![0; TokenAccount::LEN];
        TokenAccount::pack(
            TokenAccount {
                mint: mint_id,
                owner: payer,
                amount: 0,
                delegate: COption::None,
                state: AccountState::Initialized,
                is_native: COption::None,
                delegated_amount: 0,
                close_authority: COption::None,
            },
            &mut destination_data,
        )
        .unwrap();
        let (queue, recent_blockhash) = blockhash_queue();
        let instruction = transfer_checked(
            &program_id,
            &source_id,
            &mint_id,
            &destination_id,
            &payer,
            &[],
            10,
            0,
        )
        .unwrap();
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(legacy::Message::new_with_blockhash(
                &[instruction],
                Some(&payer),
                &recent_blockhash,
            )),
        };
        let accounts = vec![
            (payer, empty_account(10_000_000)),
            (mint_id, account(2_000_000, mint_data, program_id, false)),
            (
                source_id,
                account(2_000_000, source_data, program_id, false),
            ),
            (
                destination_id,
                account(2_000_000, destination_data, program_id, false),
            ),
            (program_id, program),
            (program_data_id, program_data),
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
            slot_hashes_sysvar_account(),
        ];
        let (execution, trace) = traced_execution(&accounts, queue, transaction, false);
        assert_executed_ok(&execution);
        let BankTxnProcessingResult::Processed {
            result: Ok(ProcessedTransaction::Executed(transaction)),
            ..
        } = &execution
        else {
            unreachable!()
        };
        let unpack = |address| {
            let (_, account) = transaction
                .loaded_transaction
                .accounts
                .iter()
                .find(|(key, _)| *key == address)
                .unwrap();
            TokenAccount::unpack(account.data()).unwrap()
        };
        assert_eq!(unpack(source_id).amount, 90);
        assert_eq!(unpack(destination_id).amount, 10);
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEventV1::VmInvocation {
                program_id: traced_program_id,
                rows,
                ..
            } if *traced_program_id == program_id.to_bytes() && !rows.is_empty()
        )));
        assert_eq!(
            trace.summary().unwrap().outcome,
            TransactionOutcomeV1::ExecutedSuccess
        );
    }

    #[test]
    fn test_lookup_table() {
        let [(program_id, program), (program_data_id, program_data)] =
            deploy_program("complex-transfer");
        let fee_payer = Pubkey::new_unique();
        let sender = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let extra_account = Pubkey::new_unique();
        let (blockhash_queue, recent_blockhash) = blockhash_queue();

        // The program adds this account's little-endian amount to the transfer.
        let extra_data = account(2, vec![5, 0, 0, 0, 0, 0, 0, 0], Pubkey::default(), false);

        // `recipient` and `extra_account` are supplied via the address lookup table.
        let alut_key = Pubkey::new_from_array([1; 32]);
        let alut = AddressLookupTable {
            meta: LookupTableMeta::default(),
            addresses: Cow::Owned(vec![recipient, extra_account]),
        };
        let alut_account = account(
            1,
            alut.serialize_for_tests().unwrap(),
            solana_sdk_ids::address_lookup_table::id(),
            false,
        );

        let message = VersionedMessage::V0(v0::Message {
            header: MessageHeader {
                num_required_signatures: 2,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 2,
            },
            account_keys: vec![fee_payer, sender, program_id, Pubkey::default()],
            recent_blockhash,
            // sender (1), recipient (4, ALUT), system (3), extra_account (5, ALUT)
            instructions: vec![CompiledInstruction {
                program_id_index: 2,
                accounts: vec![1, 4, 3, 5],
                data: vec![0, 0, 0, 0, 0, 0, 0, 10],
            }],
            address_table_lookups: vec![MessageAddressTableLookup {
                account_key: alut_key,
                writable_indexes: vec![0],
                readonly_indexes: vec![1],
            }],
        });
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default(), Signature::default()],
            message,
        };

        let accounts = vec![
            (fee_payer, empty_account(10000000)),
            (recipient, empty_account(900000)),
            (sender, empty_account(900000)),
            (program_id, program),
            (program_data_id, program_data),
            (extra_account, extra_data),
            (alut_key, alut_account),
            system_program_account(),
            clock_sysvar_account(),
            epoch_schedule_sysvar_account(),
            rent_sysvar_account(),
            slot_hashes_sysvar_account(),
        ];

        let execution = execute_txn(
            &accounts,
            feature_set(),
            blockhash_queue,
            fee_rate_governor(),
            0,
            transaction,
        );

        assert_executed_ok(&execution);
        assert_eq!(writable_account_lamports(&execution, &sender), Some(899985));
        assert_eq!(
            writable_account_lamports(&execution, &recipient),
            Some(900015)
        );
    }
}
