use {
    super::*,
    agave_geyser_plugin_interface::transaction_status_meta::{
        InnerInstruction, InnerInstructions, TransactionStatusMeta,
    },
    solana_hash::Hash,
    solana_message::{
        Message, VersionedMessage, compiled_instruction::CompiledInstruction, v0::LoadedAddresses,
    },
    solana_signature::Signature,
    solana_transaction::versioned::VersionedTransaction,
    solana_transaction_error::TransactionError,
};

fn with_info(
    keys: Vec<Pubkey>,
    loaded: LoadedAddresses,
    logs: &[String],
    instructions: &[CompiledInstruction],
    failed: bool,
    test: impl FnOnce(ReplicaTransactionInfoV4<'_>),
) {
    let signature = Signature::default();
    let hash = Hash::default();
    let transaction = VersionedTransaction {
        signatures: vec![signature],
        message: VersionedMessage::Legacy(Message {
            account_keys: keys,
            ..Message::default()
        }),
    };
    let inner: Vec<_> = instructions
        .iter()
        .map(|instruction| InnerInstruction {
            instruction,
            stack_height: Some(2),
        })
        .collect();
    let groups = [InnerInstructions {
        index: 0,
        instructions: &inner,
    }];
    let logs: Vec<_> = logs.iter().map(String::as_str).collect();
    let error = TransactionError::AccountNotFound;
    let meta = TransactionStatusMeta {
        status: if failed { Err(&error) } else { Ok(()) },
        fee: 0,
        pre_balances: &[],
        post_balances: &[],
        inner_instructions: Some(&groups),
        log_messages: Some(&logs),
        pre_token_balances: None,
        post_token_balances: None,
        rewards: None,
        loaded_addresses: &loaded,
        return_data: None,
        compute_units_consumed: None,
        cost_units: None,
    };
    test(ReplicaTransactionInfoV4 {
        signature: &signature,
        message_hash: &hash,
        is_vote: false,
        transaction: &transaction,
        transaction_status_meta: &meta,
        index: 4,
    });
}

fn payload() -> String {
    format!("Program data: {}", BASE64_STANDARD.encode([3; 8]))
}

#[test]
fn nested_logs_use_current_program_and_stable_ordinals() {
    let outer = Pubkey::new_unique();
    let inner = Pubkey::new_unique();
    let logs = vec![
        format!("Program {outer} invoke [1]"),
        payload(),
        format!("Program {inner} invoke [2]"),
        payload(),
        format!("Program {inner} success"),
        payload(),
        format!("Program {outer} success"),
    ];
    with_info(
        vec![outer, inner],
        LoadedAddresses::default(),
        &logs,
        &[],
        false,
        |info| {
            let all = extract(&info, 8, &HashSet::from([outer, inner]));
            assert_eq!(
                all.iter()
                    .map(|event| (event.program_id.clone(), event.log_index))
                    .collect::<Vec<_>>(),
                vec![
                    (outer.to_string(), 0),
                    (inner.to_string(), 1),
                    (outer.to_string(), 2)
                ]
            );
            let filtered = extract(&info, 8, &HashSet::from([inner]));
            assert_eq!(filtered[0].id(), all[1].id());
            assert_eq!(filtered[0].tx_index, 4);
        },
    );
}

#[test]
fn failed_transaction_and_votes_have_no_events() {
    let program = Pubkey::new_unique();
    let logs = vec![
        format!("Program {program} invoke [1]"),
        payload(),
        format!("Program {program} success"),
    ];
    with_info(
        vec![program],
        LoadedAddresses::default(),
        &logs,
        &[],
        true,
        |info| assert!(extract(&info, 8, &HashSet::from([program])).is_empty()),
    );
    with_info(
        vec![program],
        LoadedAddresses::default(),
        &logs,
        &[],
        false,
        |mut info| {
            info.is_vote = true;
            assert!(extract(&info, 8, &HashSet::from([program])).is_empty());
        },
    );
}

#[test]
fn caught_inner_failure_discards_its_logs_and_cpi_events() {
    let outer = Pubkey::new_unique();
    let inner = Pubkey::new_unique();
    let logs = vec![
        format!("Program {outer} invoke [1]"),
        format!("Program {inner} invoke [2]"),
        payload(),
        format!("Program {inner} failed: custom program error: 1"),
        payload(),
        format!("Program {outer} success"),
    ];
    let instruction = CompiledInstruction {
        program_id_index: 1,
        accounts: vec![],
        data: [EVENT_IX_TAG.as_slice(), &[7; 8]].concat(),
    };
    with_info(
        vec![outer, inner],
        LoadedAddresses::default(),
        &logs,
        &[instruction],
        false,
        |info| {
            let events = extract(&info, 8, &HashSet::from([outer, inner]));
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].program_id, outer.to_string());
            assert_eq!(events[0].log_index, 1);
        },
    );
}

#[test]
fn cpi_payload_uses_loaded_program_and_strips_only_instruction_tag() {
    let outer = Pubkey::new_unique();
    let program = Pubkey::new_unique();
    let instruction = CompiledInstruction {
        program_id_index: 1,
        accounts: vec![],
        data: [EVENT_IX_TAG.as_slice(), &[7; 16]].concat(),
    };
    with_info(
        vec![outer],
        LoadedAddresses {
            writable: vec![],
            readonly: vec![program],
        },
        &[],
        &[instruction],
        false,
        |info| {
            let events = extract(&info, 8, &HashSet::from([program]));
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].source, Source::Cpi);
            assert_eq!(
                BASE64_STANDARD.decode(&events[0].data).unwrap(),
                vec![7; 16]
            );
        },
    );
}

#[test]
fn malformed_and_incomplete_logs_do_not_leak_program_attribution() {
    let program = Pubkey::new_unique();
    let other = Pubkey::new_unique();
    let logs = vec![
        payload(),
        format!("Program {program} invoke [1]"),
        "Program data: invalid!".into(),
        format!("Program {other} success"),
        payload(),
        format!("Program {program} invoke [1]"),
        payload(),
    ];
    with_info(
        vec![program],
        LoadedAddresses::default(),
        &logs,
        &[],
        false,
        |info| assert!(extract(&info, 8, &HashSet::from([program])).is_empty()),
    );
}

#[test]
fn cpi_requires_tag_and_event_discriminator() {
    let program = Pubkey::new_unique();
    let instructions = vec![
        CompiledInstruction {
            program_id_index: 0,
            accounts: vec![],
            data: EVENT_IX_TAG.to_vec(),
        },
        CompiledInstruction {
            program_id_index: 0,
            accounts: vec![],
            data: vec![0; 16],
        },
        CompiledInstruction {
            program_id_index: 255,
            accounts: vec![],
            data: [EVENT_IX_TAG.as_slice(), &[7; 8]].concat(),
        },
    ];
    with_info(
        vec![program],
        LoadedAddresses::default(),
        &[],
        &instructions,
        false,
        |info| assert!(extract(&info, 8, &HashSet::from([program])).is_empty()),
    );
}
