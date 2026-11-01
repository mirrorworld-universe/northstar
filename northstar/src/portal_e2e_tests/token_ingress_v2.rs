use {
    super::*,
    crate::continuity_v2::hash_parts,
    solana_program_pack::Pack,
    solana_transaction::{InstructionError, TransactionError},
};

#[test]
fn regression_v2_bridge_registration_requires_authority_and_session_context() {
    setup();
    let (bank, _forks, portal, payer) = setup_bank_with_portal();
    let owner = payer.pubkey();
    let session = find_session_pda(&portal).0;
    let fee_vault = find_fee_vault_pda(&portal).0;
    let epoch = northstar_portal::find_session_epoch_v2_pda(&portal).0;
    let open = Instruction {
        program_id: portal,
        accounts: vec![
            AccountMeta::new(owner, true),
            AccountMeta::new(session, false),
            AccountMeta::new(fee_vault, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(epoch, false),
        ],
        data: borsh::to_vec(&PortalInstruction::OpenSessionV2(
            northstar_portal::OpenSessionV2 {
                session: OpenSession {
                    grid_id: 0,
                    ttl_slots: 10_000,
                    fee_cap: 0,
                    validator: owner,
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
        Some(&owner),
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
    let mint = Pubkey::new_unique();
    let bridge = Pubkey::new_unique();
    let token = spl_token_interface::id();
    // Only Portal executes here; custody/CPI qualification uses a separate fixture.
    for program in [bridge, token] {
        let value: AccountSharedData = solana_account::Account {
            lamports: 1_000_000_000,
            data: vec![0],
            owner: solana_sdk_ids::bpf_loader::id(),
            executable: true,
            rent_epoch: 0,
        }
        .into();
        bank.store_account(&program, &value);
    }
    let mint_state = spl_token_interface::state::Mint {
        is_initialized: true,
        decimals: 6,
        ..Default::default()
    };
    let mut mint_data = vec![0; spl_token_interface::state::Mint::LEN];
    spl_token_interface::state::Mint::pack(mint_state, &mut mint_data).unwrap();
    let value: AccountSharedData = solana_account::Account {
        lamports: 1_000_000_000,
        data: mint_data,
        owner: token,
        executable: false,
        rent_epoch: 0,
    }
    .into();
    bank.store_account(&mint, &value);
    let registration = northstar_portal::find_session_bridge_v2_pda(&portal, &context, &mint).0;
    let vault = Pubkey::find_program_address(&[b"token_vault", registration.as_ref()], &bridge).0;
    let args = northstar_portal::RegisterSessionBridge {
        mint,
        bridge_program: bridge,
        vault,
        token_program: token,
    };
    let register = |payer_key| Instruction {
        program_id: portal,
        accounts: vec![
            AccountMeta::new(payer_key, true),
            AccountMeta::new_readonly(session, false),
            AccountMeta::new(registration, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(bridge, false),
            AccountMeta::new_readonly(token, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: [&[36u8], borsh::to_vec(&args).unwrap().as_slice()].concat(),
    };
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[register(owner)],
        Some(&owner),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let registered: northstar_portal::SessionBridgeV2 =
        borsh::from_slice(bank.get_account(&registration).unwrap().data()).unwrap();
    assert!(registered.is_valid());
    assert_eq!(bank.get_account(&registration).unwrap().data().len(), 209);
    assert_eq!(registered.base.bridge_program, bridge);
    assert_eq!(registered.base.vault, vault);
    assert_eq!(registered.base.token_program, token);
    assert_eq!(registered.session_context, context);
    assert_eq!(registered.base.session, session);
    assert_eq!(registered.base.mint, mint);
    assert_eq!(
        register(owner).data,
        borsh::to_vec(&PortalInstruction::RegisterSessionBridgeV2(args)).unwrap()
    );
    bank.process_transaction(&Transaction::new_signed_with_payer(
        &[register(owner), register(owner)],
        Some(&owner),
        &[&payer],
        bank.last_blockhash(),
    ))
    .unwrap();
    let mut wrong_context = register(owner);
    let different = northstar_portal::find_session_bridge_v2_pda(&portal, &[9; 32], &mint).0;
    wrong_context.accounts[2].pubkey = different;
    let rejected = bank.process_transaction(&Transaction::new_signed_with_payer(
        &[wrong_context],
        Some(&owner),
        &[&payer],
        bank.last_blockhash(),
    ));
    assert!(
        matches!(rejected,Err(TransactionError::InstructionError(0,InstructionError::Custom(code)))
        if code==northstar_portal::PortalError::InvalidPdaSeeds as u32)
    );
    assert!(bank.get_account(&different).is_none());
    let mut trailing = register(owner);
    trailing.data.push(0);
    let rejected = bank.process_transaction(&Transaction::new_signed_with_payer(
        &[trailing],
        Some(&owner),
        &[&payer],
        bank.last_blockhash(),
    ));
    assert!(
        matches!(
            &rejected,
            Err(TransactionError::InstructionError(
                0,
                InstructionError::InvalidInstructionData
            ))
        ),
        "unexpected trailing-byte result: {rejected:?}"
    );
    let other = Keypair::new();
    let funding = AccountSharedData::new(10_000_000, 0, &system_program::id());
    bank.store_account(&other.pubkey(), &funding);
    let result = bank.process_transaction(&Transaction::new_signed_with_payer(
        &[register(other.pubkey())],
        Some(&other.pubkey()),
        &[&other],
        bank.last_blockhash(),
    ));
    assert!(
        matches!(result,Err(TransactionError::InstructionError(0,InstructionError::Custom(code)))
        if code==northstar_portal::PortalError::Unauthorized as u32)
    );
    let next_context = [9; 32];
    assert_ne!(
        registration,
        northstar_portal::find_session_bridge_v2_pda(&portal, &next_context, &mint).0
    );
    let after: northstar_portal::SessionV2 =
        borsh::from_slice(bank.get_account(&session).unwrap().data()).unwrap();
    assert_eq!(after, frame);
}
