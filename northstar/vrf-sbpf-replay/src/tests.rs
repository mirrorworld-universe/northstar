use {
    super::*, solana_clock::Clock, solana_instruction::Instruction, solana_message::Message,
    solana_rent::Rent, solana_slot_hashes::SlotHashes,
};

fn input(program: Pubkey) -> ExecutionInput {
    let prefix =
        Instruction::new_with_bytes(compute_budget::id(), &[2, 0x40, 0x42, 0x0f, 0], vec![]);
    let discriminator = hash(if program == CONSUMER {
        b"global:make_request"
    } else {
        b"global:fulfill"
    });
    let call = Instruction::new_with_bytes(program, &discriminator.as_ref()[..8], vec![]);
    let payer = Pubkey::new_from_array([1; 32]);
    let message = SanitizedMessage::try_from_legacy_message(
        Message::new(&[prefix, call], Some(&payer)),
        &std::collections::HashSet::new(),
    )
    .unwrap();
    let mut accounts: Vec<_> = message
        .account_keys()
        .iter()
        .map(|key| {
            (
                *key,
                Account {
                    lamports: 10_000_000,
                    owner: if key == &compute_budget::id() {
                        solana_sdk_ids::native_loader::id()
                    } else {
                        solana_sdk_ids::system_program::id()
                    },
                    executable: key == &compute_budget::id(),
                    ..Account::default()
                },
            )
        })
        .collect();
    for key in [
        solana_sdk_ids::system_program::id(),
        bpf_loader_upgradeable::id(),
    ] {
        accounts.push((
            key,
            Account {
                lamports: 1,
                executable: true,
                owner: solana_sdk_ids::native_loader::id(),
                ..Account::default()
            },
        ));
    }
    ExecutionInput {
        message,
        accounts,
        slot: 10,
        compute_unit_limit: 1_000_000,
    }
}

#[test]
fn invariant_shape_and_compute_budget_are_bound() {
    let mut value = input(CONSUMER);
    assert_eq!(validate_shape(&value), Ok(()));
    value.compute_unit_limit -= 1;
    assert_eq!(validate_shape(&value), Err(ReplayError::UnsupportedShape));
    value.compute_unit_limit = 1_400_001;
    assert_eq!(validate_shape(&value), Err(ReplayError::UnsupportedShape));
    assert_eq!(
        validate_shape(&input(VERIFIER)),
        Err(ReplayError::UnsupportedShape)
    );
}

#[test]
fn invariant_reserved_write_privileges_are_rejected() {
    let mut value = input(CONSUMER);
    let mut message = value.message.legacy_message().unwrap().clone();
    let reserved = sysvar::rent::id();
    let payer = message.account_keys[0];
    message.account_keys[0] = reserved;
    value
        .accounts
        .iter_mut()
        .find(|(key, _)| key == &payer)
        .unwrap()
        .0 = reserved;
    value.message =
        SanitizedMessage::try_from_legacy_message(message, &std::collections::HashSet::new())
            .unwrap();
    assert_eq!(validate_shape(&value), Err(ReplayError::UnsupportedShape));
}

#[test]
fn invariant_compiled_configuration_and_program_identity() {
    let (vm, syscalls, budget) = runtime_identities();
    assert_ne!(vm, [0; 32]);
    assert_ne!(syscalls, [0; 32]);
    assert_ne!(budget, [0; 32]);
    let programs = pinned_programs();
    assert!(
        programs
            .windows(2)
            .all(|pair| pair[0].program_id < pair[1].program_id)
    );
    for program in programs {
        assert_eq!(program.loader_id, bpf_loader_upgradeable::id().to_bytes());
        assert_eq!(program.sbpf_version, 2);
        assert_ne!(program.elf_sha256, [0; 32]);
    }
}

#[test]
fn invariant_compiled_profile_cannot_be_widened() {
    use northstar_transaction_proof::vrf_candidate::{
        BoundedVec, CandidateError, CandidateExecutionInputV1, CandidateReplayEngine,
        CandidateRuntimeV1, CandidateUnsupported,
    };
    let check = |profile| {
        let input = CandidateExecutionInputV1 {
            session_context: BoundedVec::default(),
            er_slot: 0,
            step_index: 0,
            transaction_bytes: BoundedVec::default(),
            profile,
            runtime: CandidateRuntimeV1 {
                slot: 0,
                observation_slot: 0,
                recent_blockhashes: BoundedVec::default(),
                compute_unit_limit: 1_400_000,
                heap_bytes: 32_768,
            },
            accounts: BoundedVec::default(),
            programs: BoundedVec::default(),
        };
        assert_eq!(
            AgaveReplayEngine.execute(&input).err(),
            Some(CandidateError::Unsupported(CandidateUnsupported::Runtime))
        );
    };
    let mut profile = development_profile();
    profile.checkpoint_steps = 2;
    check(profile);
    let mut profile = development_profile();
    profile.max_processing_age = 1800;
    check(profile);
    let mut profile = development_profile();
    profile.vm_config_sha256 = [0; 32];
    check(profile);
}

#[test]
fn invariant_builtin_owner_and_executable_are_bound() {
    let mut value = input(CONSUMER);
    let key = solana_sdk_ids::system_program::id();
    value
        .accounts
        .iter_mut()
        .find(|(k, _)| k == &key)
        .unwrap()
        .1
        .owner = CONSUMER;
    assert_eq!(
        validate_shape(&value),
        Err(ReplayError::InvalidProgram(key))
    );
    value
        .accounts
        .iter_mut()
        .find(|(k, _)| k == &key)
        .unwrap()
        .1
        .owner = solana_sdk_ids::native_loader::id();
    value
        .accounts
        .iter_mut()
        .find(|(k, _)| k == &key)
        .unwrap()
        .1
        .executable = false;
    assert_eq!(
        validate_shape(&value),
        Err(ReplayError::InvalidProgram(key))
    );
}

#[test]
fn invariant_duplicate_and_missing_accounts_are_rejected() {
    let mut value = input(CONSUMER);
    value.accounts.push(value.accounts[0].clone());
    assert!(matches!(
        validate_shape(&value),
        Err(ReplayError::DuplicateAccount(_))
    ));
    value.accounts.pop();
    let missing = value.accounts.pop().unwrap().0;
    assert_eq!(
        validate_shape(&value),
        Err(ReplayError::MissingAccount(missing))
    );
}

fn add_programs(value: &mut ExecutionInput, dir: &std::path::Path) {
    for ((key, _), filename) in PROGRAMS.into_iter().zip([
        "vrf_coordinator.so",
        "reference_verify.so",
        "test_consumer.so",
    ]) {
        let data_key =
            Pubkey::find_program_address(&[key.as_ref()], &bpf_loader_upgradeable::id()).0;
        let mut program_data = 2u32.to_le_bytes().to_vec();
        program_data.extend_from_slice(data_key.as_ref());
        let program = Account {
            lamports: 1_000_000_000,
            owner: bpf_loader_upgradeable::id(),
            executable: true,
            data: program_data,
            rent_epoch: u64::MAX,
        };
        if let Some((_, a)) = value.accounts.iter_mut().find(|(k, _)| k == &key) {
            *a = program;
        } else {
            value.accounts.push((key, program));
        }
        let mut data = vec![0; 45];
        data[..4].copy_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&std::fs::read(dir.join(filename)).unwrap());
        value.accounts.push((
            data_key,
            Account {
                lamports: 1_000_000_000,
                data,
                owner: bpf_loader_upgradeable::id(),
                executable: false,
                rent_epoch: u64::MAX,
            },
        ));
    }
}

fn sysvar(key: Pubkey, data: Vec<u8>) -> (Pubkey, Account) {
    (
        key,
        Account {
            lamports: 1,
            data,
            owner: sysvar::id(),
            executable: false,
            rent_epoch: u64::MAX,
        },
    )
}

#[test]
#[ignore = "requires freshly built, hash-pinned external ELFs in VRF_SBPF_PROGRAM_DIR"]
fn native_actual_interpreter_rejects_incomplete_request_and_fulfillment_arguments() {
    let directory = std::env::var_os("VRF_SBPF_PROGRAM_DIR").expect("VRF_SBPF_PROGRAM_DIR");
    for program in [CONSUMER, COORDINATOR] {
        let mut value = input(program);
        add_programs(&mut value, std::path::Path::new(&directory));
        value.accounts.extend([
            sysvar(
                sysvar::clock::id(),
                wincode::serialize(&Clock {
                    slot: 10,
                    ..Clock::default()
                })
                .unwrap(),
            ),
            sysvar(
                sysvar::rent::id(),
                wincode::serialize(&Rent::default()).unwrap(),
            ),
            sysvar(
                sysvar::slot_hashes::id(),
                wincode::serialize(&SlotHashes::new(
                    &[(9, solana_hash::Hash::default()).into()],
                ))
                .unwrap(),
            ),
        ]);
        let output = execute(&value).expect("actual pinned bytecode executes");
        assert!(
            matches!(output.status, Err(TransactionError::InstructionError(1, _))),
            "{:?}: {:?}",
            program,
            output.status
        );
        assert!(
            output.executed_units > 150,
            "SBPF instructions must execute beyond compute-budget builtin"
        );
        assert_eq!(output.resulting_accounts, value.accounts);
        assert!(output.logs.iter().any(|line| line.contains("failed")));
        for line in &output.logs {
            println!("{line}");
        }
        println!("program={program} executed_units={}", output.executed_units);
        let programdata =
            Pubkey::find_program_address(&[COORDINATOR.as_ref()], &bpf_loader_upgradeable::id()).0;
        let data = &mut value
            .accounts
            .iter_mut()
            .find(|(key, _)| key == &programdata)
            .unwrap()
            .1
            .data;
        data[45] ^= 1;
        assert!(matches!(
            execute(&value),
            Err(ReplayError::InvalidProgram(COORDINATOR))
        ));
        let data = &mut value
            .accounts
            .iter_mut()
            .find(|(key, _)| key == &programdata)
            .unwrap()
            .1
            .data;
        data[45] ^= 1;
        data[4..12].copy_from_slice(&10u64.to_le_bytes());
        assert!(matches!(
            execute(&value),
            Err(ReplayError::InvalidProgram(COORDINATOR))
        ));
    }
}
