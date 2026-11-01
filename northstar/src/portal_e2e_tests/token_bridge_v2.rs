use {
    super::*,
    crate::continuity_v2::hash_parts,
    northstar_token_bridge::instruction::TokenBridgeInstruction,
    northstar_zk_types::continuity::{IngressPayloadV2, IngressRecordV2},
    solana_program_pack::Pack,
};

pub(crate) struct TokenWorldV2 {
    pub(crate) bank: Arc<Bank>,
    _forks: Arc<RwLock<BankForks>>,
    pub(crate) payer: Arc<Keypair>,
    pub(crate) portal: Pubkey,
    pub(crate) bridge: Pubkey,
    pub(crate) session: Pubkey,
    pub(crate) context: [u8; 32],
    mint: Pubkey,
    registry: Pubkey,
    vault: Pubkey,
    vault_token: Pubkey,
    pub(crate) er: Pubkey,
    pub(crate) receipt: Pubkey,
    origin: Pubkey,
    delegation: Pubkey,
    first_ingress: Pubkey,
    original_data: Vec<u8>,
}

fn delegated_world() -> TokenWorldV2 {
    delegated_token_world(Pubkey::new_unique(), false)
}

pub(crate) fn delegated_token_world(portal: Pubkey, bootstrap_fee_payer: bool) -> TokenWorldV2 {
    setup();
    let (bank, _forks, portal, payer) = setup_bank_with_portal_id(portal);
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
    let delegation_sequence = if bootstrap_fee_payer {
        let receipt =
            northstar_portal::find_deposit_receipt_v2_pda(&portal, &context, &payer_key).0;
        let ingress = northstar_portal::find_ingress_record_v2_pda(&portal, &context, 1).0;
        let deposit = Instruction {
            program_id: portal,
            accounts: vec![
                AccountMeta::new(payer_key, true),
                AccountMeta::new(session, false),
                AccountMeta::new(receipt, false),
                AccountMeta::new_readonly(payer_key, false),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new(ingress, false),
            ],
            data: borsh::to_vec(&PortalInstruction::DepositFeeV2 {
                lamports: 100_000_000,
            })
            .unwrap(),
        };
        bank.process_transaction(&Transaction::new_signed_with_payer(
            &[deposit],
            Some(&payer_key),
            &[&payer],
            bank.last_blockhash(),
        ))
        .unwrap();
        2
    } else {
        1
    };
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
    let ingress =
        northstar_portal::find_ingress_record_v2_pda(&portal, &context, delegation_sequence).0;
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
    TokenWorldV2 {
        bank,
        _forks,
        payer: Arc::new(payer),
        portal,
        bridge,
        session,
        context,
        mint: mint.pubkey(),
        registry,
        vault,
        vault_token: vault_token.pubkey(),
        er,
        receipt,
        origin,
        delegation,
        first_ingress: ingress,
        original_data: original,
    }
}

#[test]
fn regression_v2_bridge_cpi_preserves_owner_authorized_delegation() {
    let world = delegated_world();
    let TokenWorldV2 {
        bank,
        portal,
        bridge,
        context,
        er,
        delegation,
        first_ingress: ingress,
        original_data: original,
        ..
    } = &world;
    let value = bank.get_account(er).unwrap();
    assert_eq!(value.owner(), portal);
    assert_eq!(value.data(), original);
    let record: IngressRecordV2 =
        borsh::from_slice(bank.get_account(ingress).unwrap().data()).unwrap();
    record
        .validate_next(*context, 0, [0; 32], hash_parts)
        .unwrap();
    assert!(
        matches!(record.payload,IngressPayloadV2::Delegation {account,owner,..}
        if account==er.to_bytes() && owner==bridge.to_bytes())
    );
    let binding: northstar_portal::DelegationRecord =
        borsh::from_slice(bank.get_account(delegation).unwrap().data()).unwrap();
    assert_eq!(binding.owner_program, *bridge);
    assert_eq!(binding.grid_id, 0);
}

impl TokenWorldV2 {
    pub(crate) fn send(
        &self,
        instructions: &[Instruction],
        extra: &[&Keypair],
    ) -> std::result::Result<(), solana_transaction::TransactionError> {
        let mut signers = vec![self.payer.as_ref()];
        signers.extend_from_slice(extra);
        self.bank
            .process_transaction(&Transaction::new_signed_with_payer(
                instructions,
                Some(&self.payer.pubkey()),
                &signers,
                self.bank.last_blockhash(),
            ))
    }

    pub(crate) fn delegate_token_owner(&self, owner: &Keypair, sequence: u64) -> Pubkey {
        let er = northstar_token_bridge::find_er_token_account_pda(
            &self.bridge,
            &self.registry,
            &owner.pubkey(),
        )
        .0;
        let initialize = Instruction {
            program_id: self.bridge,
            accounts: vec![
                AccountMeta::new(self.payer.pubkey(), true),
                AccountMeta::new(er, false),
                AccountMeta::new_readonly(self.registry, false),
                AccountMeta::new_readonly(self.portal, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data: borsh::to_vec(&TokenBridgeInstruction::InitializeErTokenAccount {
                owner: owner.pubkey().to_bytes(),
            })
            .unwrap(),
        };
        self.send(&[initialize], &[]).unwrap();
        let receipt = northstar_token_bridge::find_token_deposit_receipt_pda(
            &self.bridge,
            &self.registry,
            &er,
        )
        .0;
        let origin = Pubkey::find_program_address(
            &[
                northstar_token_bridge::state::TokenDepositProgress::ORIGIN_SEED,
                er.as_ref(),
            ],
            &self.bridge,
        )
        .0;
        let delegate = Instruction {
            program_id: self.bridge,
            accounts: vec![
                AccountMeta::new(self.payer.pubkey(), true),
                AccountMeta::new_readonly(owner.pubkey(), true),
                AccountMeta::new(er, false),
                AccountMeta::new_readonly(self.bridge, false),
                AccountMeta::new_readonly(self.registry, false),
                AccountMeta::new_readonly(self.portal, false),
                AccountMeta::new(self.session, false),
                AccountMeta::new(
                    northstar_portal::find_delegation_record_pda(&self.portal, &er).0,
                    false,
                ),
                AccountMeta::new(
                    northstar_token_bridge::find_buffer_pda(&self.bridge, &er).0,
                    false,
                ),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new_readonly(receipt, false),
                AccountMeta::new(origin, false),
                AccountMeta::new(
                    northstar_portal::find_ingress_record_v2_pda(
                        &self.portal,
                        &self.context,
                        sequence,
                    )
                    .0,
                    false,
                ),
            ],
            data: borsh::to_vec(&TokenBridgeInstruction::DelegateErTokenAccount { grid_id: 0 })
                .unwrap(),
        };
        self.send(&[delegate], &[owner]).unwrap();
        er
    }

    pub(crate) fn funded_tokens(&self, amount: u64) -> Pubkey {
        let account = Keypair::new();
        let token = spl_token_interface::id();
        let create = solana_system_interface::instruction::create_account(
            &self.payer.pubkey(),
            &account.pubkey(),
            Rent::default().minimum_balance(spl_token_interface::state::Account::LEN),
            spl_token_interface::state::Account::LEN as u64,
            &token,
        );
        let initialize = spl_token_interface::instruction::initialize_account3(
            &token,
            &account.pubkey(),
            &self.mint,
            &self.payer.pubkey(),
        )
        .unwrap();
        let mint = spl_token_interface::instruction::mint_to_checked(
            &token,
            &self.mint,
            &account.pubkey(),
            &self.payer.pubkey(),
            &[],
            amount,
            6,
        )
        .unwrap();
        self.send(&[create, initialize, mint], &[&account]).unwrap();
        account.pubkey()
    }

    fn token_amount(&self, address: &Pubkey) -> u64 {
        spl_token_interface::state::Account::unpack(self.bank.get_account(address).unwrap().data())
            .unwrap()
            .amount
    }

    fn unsigned_record(&self, amount: u64, sequence: u64) -> Instruction {
        let ingress =
            northstar_portal::find_ingress_record_v2_pda(&self.portal, &self.context, sequence).0;
        Instruction {
            program_id: self.portal,
            accounts: vec![
                AccountMeta::new(self.payer.pubkey(), true),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new(self.session, false),
                AccountMeta::new_readonly(self.registry, false),
                AccountMeta::new_readonly(self.vault, false),
                AccountMeta::new_readonly(self.er, false),
                AccountMeta::new_readonly(self.delegation, false),
                AccountMeta::new_readonly(self.receipt, false),
                AccountMeta::new_readonly(self.origin, false),
                AccountMeta::new(ingress, false),
            ],
            data: borsh::to_vec(&PortalInstruction::RecordTokenDepositV2(
                northstar_portal::RecordTokenDepositV2 { amount },
            ))
            .unwrap(),
        }
    }

    pub(crate) fn start_withdrawal(&self, destination: Pubkey, amount: u64) -> Instruction {
        Instruction {
            program_id: self.bridge,
            accounts: vec![
                AccountMeta::new_readonly(self.payer.pubkey(), true),
                AccountMeta::new(self.er, false),
                AccountMeta::new_readonly(self.registry, false),
                AccountMeta::new_readonly(self.portal, false),
                AccountMeta::new_readonly(destination, false),
                AccountMeta::new_readonly(spl_token_interface::id(), false),
                AccountMeta::new_readonly(self.mint, false),
            ],
            data: borsh::to_vec(&TokenBridgeInstruction::StartWithdrawal {
                amount,
                decimals: 6,
            })
            .unwrap(),
        }
    }

    pub(crate) fn deposit(&self, source: Pubkey, amount: u64, sequence: u64) -> Instruction {
        let request = northstar_portal::find_undelegation_request_pda(&self.portal, &self.er).0;
        let ingress =
            northstar_portal::find_ingress_record_v2_pda(&self.portal, &self.context, sequence).0;
        Instruction {
            program_id: self.bridge,
            accounts: vec![
                AccountMeta::new(self.payer.pubkey(), true),
                AccountMeta::new_readonly(self.payer.pubkey(), true),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(self.er, false),
                AccountMeta::new_readonly(self.registry, false),
                AccountMeta::new_readonly(self.portal, false),
                AccountMeta::new(source, false),
                AccountMeta::new(self.vault_token, false),
                AccountMeta::new_readonly(self.mint, false),
                AccountMeta::new_readonly(spl_token_interface::id(), false),
                AccountMeta::new(self.receipt, false),
                AccountMeta::new_readonly(self.delegation, false),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new_readonly(request, false),
                AccountMeta::new(self.session, false),
                AccountMeta::new_readonly(self.origin, false),
                AccountMeta::new(ingress, false),
            ],
            data: borsh::to_vec(&TokenBridgeInstruction::Deposit {
                amount,
                decimals: 6,
            })
            .unwrap(),
        }
    }
}

#[test]
fn regression_v2_bridge_deposit_records_only_authorized_cumulative_credit() {
    let world = delegated_world();
    let source = world.funded_tokens(1_000);
    let mut expected = vec![37];
    expected.extend_from_slice(&42u64.to_le_bytes());
    assert_eq!(world.unsigned_record(42, 2).data, expected);
    world.send(&[world.deposit(source, 42, 2)], &[]).unwrap();
    assert_eq!(world.token_amount(&source), 958);
    assert_eq!(world.token_amount(&world.vault_token), 42);
    let receipt: northstar_token_bridge::state::TokenDepositReceipt =
        borsh::from_slice(world.bank.get_account(&world.receipt).unwrap().data()).unwrap();
    assert_eq!(receipt.balance, 42);
    let key = northstar_portal::find_ingress_record_v2_pda(&world.portal, &world.context, 2).0;
    let record: IngressRecordV2 =
        borsh::from_slice(world.bank.get_account(&key).unwrap().data()).unwrap();
    let previous: IngressRecordV2 =
        borsh::from_slice(world.bank.get_account(&world.first_ingress).unwrap().data()).unwrap();
    record
        .validate_next(world.context, 1, previous.root, hash_parts)
        .unwrap();
    assert!(
        matches!(record.payload,IngressPayloadV2::TokenDeposit {bridge,session_bridge,account,
        receipt,mint,token_program,origin_deposited:0,amount:42,cumulative_deposited:42}
        if bridge==world.bridge.to_bytes() && session_bridge==world.registry.to_bytes() && account==world.er.to_bytes()
            && receipt==world.receipt.to_bytes() && mint==world.mint.to_bytes() && token_program==spl_token_interface::id().to_bytes())
    );
    assert_eq!(
        world.bank.get_account(&world.er).unwrap().data(),
        world.original_data
    );
    assert_eq!(
        world.bank.get_account(&world.er).unwrap().owner(),
        &world.portal
    );
    assert!(world.send(&[world.unsigned_record(42, 3)], &[]).is_err());
    assert!(world.send(&[world.deposit(source, 7, 99)], &[]).is_err());
    let mut trailing = world.deposit(source, 7, 3);
    trailing.data.push(0);
    assert!(world.send(&[trailing], &[]).is_err());
    assert_eq!(world.token_amount(&source), 958);
    assert_eq!(world.token_amount(&world.vault_token), 42);
    let unchanged: northstar_token_bridge::state::TokenDepositReceipt =
        borsh::from_slice(world.bank.get_account(&world.receipt).unwrap().data()).unwrap();
    assert_eq!(unchanged, receipt);
    world.send(&[world.deposit(source, 8, 3)], &[]).unwrap();
    assert_eq!(world.token_amount(&world.vault_token), 50);
    let key = northstar_portal::find_ingress_record_v2_pda(&world.portal, &world.context, 3).0;
    let next: IngressRecordV2 =
        borsh::from_slice(world.bank.get_account(&key).unwrap().data()).unwrap();
    next.validate_next(world.context, 2, record.root, hash_parts)
        .unwrap();
    assert!(matches!(
        next.payload,
        IngressPayloadV2::TokenDeposit {
            origin_deposited: 0,
            amount: 8,
            cumulative_deposited: 50,
            ..
        }
    ));
}
