use {
    agave_feature_set::curve25519_syscall_enabled,
    agave_transaction_view::transaction_view::UnsanitizedTransactionView,
    northstar_vrf_runtime_probe::{
        AUTHORITY_SEED, CALLBACK_FAILURE, CLOCK_ID, NOT_PENDING, SLOT_NOT_LATER,
    },
    solana_account::{Account, AccountSharedData, ReadableAccount},
    solana_instruction::{AccountMeta, Instruction},
    solana_instruction_error::InstructionError,
    solana_program_binaries::bpf_loader_upgradeable_program_accounts,
    solana_pubkey::Pubkey,
    solana_rent::Rent,
    solana_runtime::{
        bank::{Bank, SlotLeader},
        genesis_utils::{create_genesis_config, GenesisConfigInfo},
    },
    solana_signer::Signer,
    solana_transaction::{
        versioned::VersionedTransaction, Transaction, TransactionVerificationMode,
    },
    solana_transaction_error::TransactionError,
    std::{path::PathBuf, sync::Arc},
};

const PROBE: Pubkey = Pubkey::new_from_array([81; 32]);
const CONSUMER: Pubkey = Pubkey::new_from_array([82; 32]);
const QUEUE: Pubkey = Pubkey::new_from_array([83; 32]);
const STATE: Pubkey = Pubkey::new_from_array([84; 32]);
const NONCE: u64 = 7;

fn bank(genesis: &GenesisConfigInfo, elf: &[u8], enabled: bool) -> Bank {
    let mut config = genesis.genesis_config.clone();
    if !enabled {
        // The runtime environment must be constructed from the feature-disabled parent.
        config.accounts.remove(&curve25519_syscall_enabled::id());
    }
    let parent = Bank::new_for_tests(&config);
    for id in [PROBE, CONSUMER] {
        for (key, account) in bpf_loader_upgradeable_program_accounts(
            &id,
            elf,
            &genesis.mint_keypair.pubkey(),
            &Rent::default(),
        ) {
            parent.store_account(&key, &AccountSharedData::from(account));
        }
    }
    let bank = Bank::new_from_parent_ephemeral_isolated(Arc::new(parent), SlotLeader::default(), 1);
    let mut queue = CONSUMER.to_bytes().to_vec();
    queue.extend_from_slice(&NONCE.to_le_bytes());
    queue.push(1);
    let mut state = PROBE.to_bytes().to_vec();
    state.extend_from_slice(&[0; 24]);
    for (key, owner, data) in [
        (QUEUE, PROBE, queue),
        (STATE, CONSUMER, state),
        (authority(), Pubkey::default(), vec![]),
    ] {
        bank.store_account(
            &key,
            &AccountSharedData::from(Account {
                lamports: 10_000_000,
                data,
                owner,
                executable: false,
                rent_epoch: 0,
            }),
        );
    }
    bank
}

fn authority() -> Pubkey {
    Pubkey::find_program_address(&[AUTHORITY_SEED, QUEUE.as_ref()], &PROBE).0
}

fn fulfill(mode: u8, nonce: u64, request_slot: u64) -> Instruction {
    let mut data = vec![1, mode];
    data.extend_from_slice(&nonce.to_le_bytes());
    data.extend_from_slice(&request_slot.to_le_bytes());
    Instruction::new_with_bytes(
        PROBE,
        &data,
        vec![
            AccountMeta::new(QUEUE, false),
            AccountMeta::new(STATE, false),
            AccountMeta::new_readonly(authority(), false),
            AccountMeta::new_readonly(CONSUMER, false),
            AccountMeta::new_readonly(CLOCK_ID, false),
        ],
    )
}

fn transaction(bank: &Bank, genesis: &GenesisConfigInfo, instruction: Instruction) -> Transaction {
    Transaction::new_signed_with_payer(
        &[instruction],
        Some(&genesis.mint_keypair.pubkey()),
        &[&genesis.mint_keypair],
        bank.last_blockhash(),
    )
}

fn simulate(
    bank: &Bank,
    transaction: Transaction,
) -> solana_runtime::bank::TransactionSimulationResult {
    let bytes = wincode::serialize(&VersionedTransaction::from(transaction)).unwrap();
    let view = UnsanitizedTransactionView::try_new_unsanitized(bytes.as_slice()).unwrap();
    let transaction = bank
        .verify_transaction(view, TransactionVerificationMode::FullVerification)
        .unwrap();
    bank.simulate_transaction_unchecked(&transaction, false)
}

#[test]
fn feature_and_pda_fixture() {
    let genesis = create_genesis_config(1_000_000_000);
    for enabled in [false, true] {
        let mut config = genesis.genesis_config.clone();
        if !enabled {
            config.accounts.remove(&curve25519_syscall_enabled::id());
        }
        let parent = Arc::new(Bank::new_for_tests(&config));
        let er = Bank::new_from_parent_ephemeral_isolated(parent, SlotLeader::default(), 1);
        assert_eq!(
            er.feature_set.is_active(&curve25519_syscall_enabled::id()),
            enabled
        );
    }
    assert_eq!(
        authority().to_string(),
        "AMkbnjBBkuPDdc17TgybYQRQ69SDv3LCVXHD7tKDAeTk"
    );
    let (address, bump) = Pubkey::find_program_address(&[AUTHORITY_SEED, &[1; 32]], &PROBE);
    assert_eq!(
        (address.to_string(), bump),
        ("2ntPdWBrek8nFxkwg5dXZZZFTXttRUKjPyeRVk9VTFhN".into(), 253)
    );
}

#[test]
#[ignore = "requires the locally built fixture ELF; see northstar/docs/vrf-runtime-probes.md"]
fn sbf_runtime_conformance() {
    let elf = std::fs::read(
        PathBuf::from(std::env::var_os("BPF_OUT_DIR").expect("BPF_OUT_DIR"))
            .join("northstar_vrf_runtime_probe.so"),
    )
    .unwrap();
    let genesis = create_genesis_config(1_000_000_000);
    let mut observations = Vec::new();
    for enabled in [true, false] {
        let bank = bank(&genesis, &elf, enabled);
        let result = simulate(
            &bank,
            transaction(
                &bank,
                &genesis,
                Instruction::new_with_bytes(PROBE, &[0], vec![]),
            ),
        );
        // Agave registers curve syscalls independently of the legacy feature account.
        assert_eq!(
            result.result,
            Ok(()),
            "legacy feature={enabled}: {:?}",
            result.logs
        );
        observations.push(serde_json::json!({
            "case": "curve_known_answers", "feature_enabled": enabled,
            "result": format!("{:?}", result.result), "units_consumed": result.units_consumed,
            "logs": result.logs,
        }));
    }
    for case in [
        "success",
        "unsigned_cpi",
        "callback_failure",
        "wrong_authority",
        "readonly_queue",
        "readonly_state",
        "wrong_nonce",
        "same_slot",
        "wrong_consumer",
        "wrong_parent_binding",
        "direct_callback",
    ] {
        let bank = bank(&genesis, &elf, true);
        if case == "wrong_parent_binding" {
            let mut state = bank.get_account(&STATE).unwrap();
            let mut data = state.data().to_vec();
            data[..32].fill(90);
            state.set_data_from_slice(&data);
            bank.store_account(&STATE, &state);
        }
        let queue_before = bank.get_account(&QUEUE).unwrap();
        let state_before = bank.get_account(&STATE).unwrap();
        let mut instruction = fulfill(0, NONCE, 0);
        let expected = match case {
            "success" => Ok(()),
            "unsigned_cpi" => {
                instruction.data[1] = 1;
                Err(InstructionError::PrivilegeEscalation)
            }
            "callback_failure" => {
                instruction.data[1] = 2;
                Err(InstructionError::Custom(CALLBACK_FAILURE))
            }
            "wrong_authority" => {
                instruction.accounts[2].pubkey = Pubkey::default();
                Err(InstructionError::InvalidSeeds)
            }
            "readonly_queue" => {
                instruction.accounts[0].is_writable = false;
                Err(InstructionError::InvalidAccountData)
            }
            "readonly_state" => {
                instruction.accounts[1].is_writable = false;
                Err(InstructionError::InvalidAccountData)
            }
            "wrong_nonce" => {
                instruction.data[2] ^= 1;
                Err(InstructionError::InvalidInstructionData)
            }
            "same_slot" => {
                instruction = fulfill(0, NONCE, bank.clock().slot);
                Err(InstructionError::Custom(SLOT_NOT_LATER))
            }
            "wrong_consumer" => {
                instruction.accounts[3].pubkey = PROBE;
                Err(InstructionError::IncorrectProgramId)
            }
            "wrong_parent_binding" => Err(InstructionError::InvalidSeeds),
            "direct_callback" => {
                let mut data = vec![2, 0];
                data.extend_from_slice(&NONCE.to_le_bytes());
                let mut metas = instruction.accounts[..3].to_vec();
                metas.push(AccountMeta::new_readonly(CLOCK_ID, false));
                instruction = Instruction::new_with_bytes(CONSUMER, &data, metas);
                Err(InstructionError::MissingRequiredSignature)
            }
            _ => unreachable!(),
        };
        let tx = transaction(&bank, &genesis, instruction);
        let simulated = simulate(&bank, tx.clone());
        let expected = expected.map_err(|error| TransactionError::InstructionError(0, error));
        assert_eq!(simulated.result, expected, "{case}: {:?}", simulated.logs);
        assert_eq!(bank.process_transaction(&tx), expected, "{case}");
        let queue_after = bank.get_account(&QUEUE).unwrap();
        let state_after = bank.get_account(&STATE).unwrap();
        if case == "success" {
            let mut expected_queue = queue_before.data().to_vec();
            expected_queue[40] = 0;
            assert_eq!(queue_after.data(), expected_queue);
            let mut expected_state = state_before.data().to_vec();
            expected_state[32..40].copy_from_slice(&NONCE.to_le_bytes());
            expected_state[40..48].copy_from_slice(&1u64.to_le_bytes());
            expected_state[48..56].copy_from_slice(&bank.clock().slot.to_le_bytes());
            assert_eq!(state_after.data(), expected_state);
            for (before, after) in [(&queue_before, &queue_after), (&state_before, &state_after)] {
                assert_eq!(before.owner(), after.owner());
                assert_eq!(before.lamports(), after.lamports());
                assert_eq!(before.data().len(), after.data().len());
            }
            // A new message avoids the ordinary duplicate-signature cache and reaches the fixture guard.
            let duplicate = transaction(&bank, &genesis, fulfill(0, NONCE, 1));
            assert_eq!(
                bank.process_transaction(&duplicate),
                Err(TransactionError::InstructionError(
                    0,
                    InstructionError::Custom(NOT_PENDING)
                ))
            );
        } else {
            assert_eq!(queue_before, queue_after, "{case}: queue rollback");
            assert_eq!(state_before, state_after, "{case}: consumer rollback");
        }
        observations.push(serde_json::json!({
            "case": case, "result": format!("{:?}", simulated.result),
            "units_consumed": simulated.units_consumed, "bank_slot": bank.slot(),
            "clock_slot": bank.clock().slot, "logs": simulated.logs,
        }));
    }
    let report = serde_json::json!({
        "scope": "local Northstar isolated ER Bank, public SBF conformance fixtures; not hosted ER RPC or proofs",
        "authority": authority().to_string(), "observations": observations,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Some(path) = std::env::var_os("VRF_RUNTIME_BANK_RESULT") {
        std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
}
