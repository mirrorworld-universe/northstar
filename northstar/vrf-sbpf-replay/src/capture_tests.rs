//! Differential diagnostics against authoritative ER sidecars, not a wire adapter or checkpoint builder.
use {
    super::*,
    northstar_transaction_proof::vrf_candidate::admit_signed_transaction,
    serde_json::Value,
    solana_instruction_error::InstructionError,
    solana_message::{Message, MessageHeader, compiled_instruction::CompiledInstruction},
};

fn key(value: &Value) -> Pubkey {
    value.as_str().unwrap().parse().unwrap()
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::from_value(value.clone()).unwrap()
}
fn state(value: &Value) -> Account {
    if value.is_null() {
        return Account::default();
    }
    Account {
        lamports: value["lamports"].as_u64().unwrap(),
        owner: key(&value["owner"]),
        executable: value["executable"].as_bool().unwrap(),
        rent_epoch: value["rent_epoch"].as_u64().unwrap(),
        data: bytes(&value["data"]),
    }
}

#[test]
#[ignore = "requires authoritative VRF_CAPTURE_FILE sidecar; never inferred from RPC observations"]
fn differential_authoritative_er_capture() {
    let path = std::env::var("VRF_CAPTURE_FILE").expect("VRF_CAPTURE_FILE");
    let capture: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(capture["capture_rejected"], Value::Null);
    assert_eq!(capture["atomic_bank_operation_lock"], true);
    assert_eq!(capture["prestate_rechecked_before_commit"], true);
    let input = &capture["input"];
    assert_eq!(input["lamports_per_signature"], 0);
    assert_eq!(input["active_features"].as_array().unwrap().len(), 1);
    assert_eq!(
        input["active_features"][0][0],
        "7rcw5UtqgDTBBv2EcynNfYckgdAaH1MAsCjKgXMkN7Ri"
    );
    let budget = ComputeBudget::new_with_defaults(false);
    let environment = solana_syscalls::create_program_runtime_environment(
        &feature_set(),
        &budget.to_budget(),
        false,
        false,
    )
    .unwrap();
    assert_eq!(
        input["vm_config_debug"].as_str().unwrap(),
        format!("{:?}", environment.get_config())
    );
    assert_eq!(
        capture["execution"]["execution_cost_debug"]
            .as_str()
            .unwrap(),
        format!("{:?}", budget.to_cost())
    );
    let observed_registry: Vec<(u32, Vec<u8>)> =
        serde_json::from_value(input["syscall_registry"].clone()).unwrap();
    let actual_registry: Vec<_> = environment
        .get_function_registry()
        .iter()
        .map(|(k, (name, _))| (k, name.to_vec()))
        .collect();
    assert_eq!(observed_registry, actual_registry);
    let wire = admit_signed_transaction(&bytes(&input["signed_transaction"]))
        .expect("actual signature and bounded wire");
    let message = Message {
        header: MessageHeader {
            num_required_signatures: wire.header[0],
            num_readonly_signed_accounts: wire.header[1],
            num_readonly_unsigned_accounts: wire.header[2],
        },
        account_keys: wire
            .account_keys
            .into_iter()
            .map(Pubkey::new_from_array)
            .collect(),
        recent_blockhash: solana_hash::Hash::new_from_array(wire.recent_blockhash),
        instructions: wire
            .instructions
            .into_iter()
            .map(|i| CompiledInstruction {
                program_id_index: i.program_id_index,
                accounts: i.accounts,
                data: i.data,
            })
            .collect(),
    };
    let reserved = reserved_keys();
    let observed_reserved: std::collections::HashSet<_> = input["reserved_account_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(key)
        .collect();
    assert_eq!(reserved, observed_reserved);
    let message = SanitizedMessage::try_from_legacy_message(message, &reserved).unwrap();
    let accounts = input["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (key(&a["pubkey"]), state(&a["account"])))
        .collect();
    let output = execute(&ExecutionInput {
        message,
        accounts,
        slot: input["slot"].as_u64().unwrap(),
        compute_unit_limit: input["compute_unit_limit"].as_u64().unwrap(),
    })
    .expect("bounded actual interpreter replay");
    let expected = &capture["execution"];
    match &output.status {
        Ok(()) => {
            assert!(expected["status"]["Ok"].is_null() && expected["status"].get("Ok").is_some())
        }
        Err(TransactionError::InstructionError(index, InstructionError::Custom(code))) => {
            assert_eq!(
                expected["status"]["Err"]["InstructionError"][0].as_u64(),
                Some(u64::from(*index))
            );
            assert_eq!(
                expected["status"]["Err"]["InstructionError"][1]["Custom"].as_u64(),
                Some(u64::from(*code))
            );
        }
        error => panic!("unencoded replay outcome: {error:?}"),
    }
    assert_eq!(
        output.executed_units,
        expected["executed_units"].as_u64().unwrap()
    );
    assert_eq!(
        output.loaded_accounts_data_size,
        expected["loaded_accounts_data_size"].as_u64().unwrap()
    );
    assert_eq!(
        output.return_data_program,
        key(&expected["return_data_program_id"])
    );
    assert_eq!(output.return_data, bytes(&expected["return_data"]));
    assert_eq!(
        output.logs,
        serde_json::from_value::<Vec<String>>(expected["logs"].clone()).unwrap()
    );
    let committed = capture["committed_accounts"].as_array().unwrap();
    assert_eq!(committed.len(), output.resulting_accounts.len());
    for ((address, actual), expected) in output.resulting_accounts.iter().zip(committed) {
        assert_eq!(*address, key(&expected["pubkey"]));
        let actual = if actual.lamports == 0 {
            Account::default()
        } else {
            actual.clone()
        };
        assert_eq!(actual, state(&expected["account"]), "account {address}");
    }
    println!(
        "authoritative differential: status={:?} CU={} loaded_bytes={} accounts={} logs={}",
        output.status,
        output.executed_units,
        output.loaded_accounts_data_size,
        output.resulting_accounts.len(),
        output.logs.len()
    );
}
