use {
    super::*,
    crate::continuity_v2::hash_parts,
    northstar_token_bridge::instruction::TokenBridgeInstruction,
    northstar_zk_types::continuity::{IngressPayloadV2, IngressRecordV2},
    solana_program_pack::Pack,
};

#[test]
fn regression_v2_bridge_cpi_preserves_owner_authorized_delegation() {
    setup();
    let (bank, _forks, portal, payer) = setup_bank_with_portal();
    let payer_key = payer.pubkey();
    let bridge = northstar_token_bridge::id();
    let directory = std::env::var_os("TOKEN_BRIDGE_BPF_OUT_DIR")
        .or_else(|| std::env::var_os("BPF_OUT_DIR"))
        .expect("explicit bridge SBF directory required");
    let program =
        std::fs::read(std::path::PathBuf::from(directory).join("northstar_token_bridge.so"))
            .unwrap();
    let executable: AccountSharedData = solana_account::Account {
        lamports: Rent::default().minimum_balance(program.len()),
        data: program,
        owner: solana_sdk_ids::bpf_loader::id(),
        executable: true,
        rent_epoch: 0,
    }
    .into();
    bank.store_account(&bridge, &executable);
    for (key, value) in
        solana_program_binaries::by_id(&spl_token_interface::id(), &Rent::default()).unwrap()
    {
        bank.store_account(&key, &value);
    }
    let session = find_session_pda(&portal).0;
    let open = Instruction {
        program_id: portal,
        accounts: vec![
            AccountMeta::new(payer_key, true),
            AccountMeta::new(session, false),
            AccountMeta::new(find_fee_vault_pda(&portal).0, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(
                northstar_portal::find_session_epoch_v2_pda(&portal).0,
                false,
            ),
        ],
        data: borsh::to_vec(&PortalInstruction::OpenSessionV2(
            northstar_portal::OpenSessionV2 {
                session: OpenSession {
                    grid_id: 0,
                    ttl_slots: 10_000,
                    fee_cap: 0,
                    validator: payer_key,
                    settlement_interval_slots: 10,
                },
                l1_genesis: [1; 32],
                profile: [2; 32],
            },
        ))
        .unwrap(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[open],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let frame: northstar_portal::SessionV2 =
        borsh::from_slice(bank.get_account(&session).unwrap().data()).unwrap();
    let context = frame
        .identity(&portal, &session)
        .commitment(hash_parts)
        .unwrap();
    let mint = Keypair::new();
    let token = spl_token_interface::id();
    let create_mint = solana_system_interface::instruction::create_account(
        &payer_key,
        &mint.pubkey(),
        Rent::default().minimum_balance(spl_token_interface::state::Mint::LEN),
        spl_token_interface::state::Mint::LEN as u64,
        &token,
    );
    let initialize_mint = spl_token_interface::instruction::initialize_mint2(
        &token,
        &mint.pubkey(),
        &payer_key,
        None,
        6,
    )
    .unwrap();
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[create_mint, initialize_mint],
        Some(&payer_key),
        &[&payer, &mint],
        bank.last_blockhash(),
    ))
    .unwrap();
    let registry =
        northstar_portal::find_session_bridge_v2_pda(&portal, &context, &mint.pubkey()).0;
    let vault = northstar_token_bridge::find_token_vault_pda(&bridge, &registry).0;
    let registration = Instruction {
        program_id: portal,
        accounts: vec![
            AccountMeta::new(payer_key, true),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(registry, false),
            AccountMeta::new_readonly(mint.pubkey(), false),
            AccountMeta::new_readonly(bridge, false),
            AccountMeta::new_readonly(token, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&PortalInstruction::RegisterSessionBridgeV2(
            northstar_portal::RegisterSessionBridge {
                mint: mint.pubkey(),
                bridge_program: bridge,
                vault,
                token_program: token,
            },
        ))
        .unwrap(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[registration],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let vault_token = Keypair::new();
    let create_token = solana_system_interface::instruction::create_account(
        &payer_key,
        &vault_token.pubkey(),
        Rent::default().minimum_balance(spl_token_interface::state::Account::LEN),
        spl_token_interface::state::Account::LEN as u64,
        &token,
    );
    let initialize_token = spl_token_interface::instruction::initialize_account3(
        &token,
        &vault_token.pubkey(),
        &mint.pubkey(),
        &vault,
    )
    .unwrap();
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[create_token, initialize_token],
        Some(&payer_key),
        &[&payer, &vault_token],
        bank.last_blockhash(),
    ))
    .unwrap();
    let initialize_vault = Instruction {
        program_id: bridge,
        accounts: vec![
            AccountMeta::new(payer_key, true),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(registry, false),
            AccountMeta::new_readonly(portal, false),
            AccountMeta::new_readonly(vault_token.pubkey(), false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::InitializeVault).unwrap(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[initialize_vault],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let er = northstar_token_bridge::find_er_token_account_pda(&bridge, &registry, &payer_key).0;
    let initialize_er = Instruction {
        program_id: bridge,
        accounts: vec![
            AccountMeta::new(payer_key, true),
            AccountMeta::new(er, false),
            AccountMeta::new_readonly(registry, false),
            AccountMeta::new_readonly(portal, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::InitializeErTokenAccount {
            owner: payer_key.to_bytes(),
        })
        .unwrap(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[initialize_er],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let original = bank.get_account(&er).unwrap().data().to_vec();
    let delegation = northstar_portal::find_delegation_record_pda(&portal, &er).0;
    let buffer = northstar_token_bridge::find_buffer_pda(&bridge, &er).0;
    let receipt = northstar_token_bridge::find_token_deposit_receipt_pda(&bridge, &registry, &er).0;
    let origin = Pubkey::find_program_address(
        &[
            northstar_token_bridge::state::TokenDepositProgress::ORIGIN_SEED,
            er.as_ref(),
        ],
        &bridge,
    )
    .0;
    let ingress = northstar_portal::find_ingress_record_v2_pda(&portal, &context, 1).0;
    let delegate = Instruction {
        program_id: bridge,
        accounts: vec![
            AccountMeta::new(payer_key, true),
            AccountMeta::new_readonly(payer_key, true),
            AccountMeta::new(er, false),
            AccountMeta::new_readonly(bridge, false),
            AccountMeta::new_readonly(registry, false),
            AccountMeta::new_readonly(portal, false),
            AccountMeta::new(session, false),
            AccountMeta::new(delegation, false),
            AccountMeta::new(buffer, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(receipt, false),
            AccountMeta::new(origin, false),
            AccountMeta::new(ingress, false),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::DelegateErTokenAccount { grid_id: 0 })
            .unwrap(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[delegate],
        Some(&payer_key),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let value = bank.get_account(&er).unwrap();
    assert_eq!(value.owner(), &portal);
    assert_eq!(value.data(), original);
    let record: IngressRecordV2 =
        borsh::from_slice(bank.get_account(&ingress).unwrap().data()).unwrap();
    record
        .validate_next(context, 0, [0; 32], hash_parts)
        .unwrap();
    assert!(
        matches!(record.payload,IngressPayloadV2::Delegation {account,owner,..}
        if account==er.to_bytes() && owner==bridge.to_bytes())
    );
    let binding: northstar_portal::DelegationRecord =
        borsh::from_slice(bank.get_account(&delegation).unwrap().data()).unwrap();
    assert_eq!(binding.owner_program, bridge);
    assert_eq!(binding.grid_id, 0);
}
