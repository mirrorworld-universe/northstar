use {
    borsh::BorshDeserialize,
    northstar_portal::{
        DelegationRecord, FeeVault, PortalError, PortalInstruction, RegisterSessionBridge, Session,
        SessionBridge, SettleAccountLamports, SettlementStatus,
    },
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_instruction_error::InstructionError,
    solana_keypair::Keypair,
    solana_program_test::{BanksClientError, ProgramTest, ProgramTestContext},
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hashv,
    solana_signer::Signer,
    solana_system_interface::{instruction::transfer, program as system_program},
    solana_transaction::{Transaction, TransactionError},
};

const PORTAL: Pubkey = Pubkey::from_str_const("GikCSCpYUq7QR7esoK6GM4UbJzKgdKNvS5bR1rBYH5E4");
const BALANCE: u64 = 10_000_000;
const ER_SLOT: u64 = 44;

fn account(lamports: u64, data: Vec<u8>, owner: Pubkey) -> Account {
    Account {
        lamports,
        data,
        owner,
        executable: false,
        rent_epoch: 0,
    }
}

fn initial_checksum() -> [u8; 32] {
    hashv(&[b"northstar-settlement-v0", &ER_SLOT.to_le_bytes()]).to_bytes()
}

struct Fixture {
    program_test: ProgramTest,
    payer: Keypair,
    session: Pubkey,
    vault: Pubkey,
}

impl Fixture {
    fn new() -> Self {
        let mut program_test = ProgramTest::default();
        program_test.prefer_bpf(true);
        program_test.add_program("northstar_portal", PORTAL, None);
        let payer = Keypair::new();
        program_test.add_account(
            payer.pubkey(),
            account(1_000_000_000, vec![], system_program::id()),
        );
        let (session, bump) = northstar_portal::find_session_pda(&PORTAL);
        let state = Session {
            discriminator: Session::DISCRIMINATOR,
            grid_id: 1,
            ttl_slots: 100,
            fee_cap: 0,
            created_at: 0,
            nonce: 0,
            authority: payer.pubkey(),
            validator: payer.pubkey(),
            settlement_interval_slots: 10,
            last_settled_l1_slot: 0,
            last_settled_er_slot: 0,
            settlement_status: SettlementStatus::InProgress,
            settlement_er_slot: ER_SLOT,
            settlement_checksum: [42; 32],
            settlement_accumulator: initial_checksum(),
            settlement_started_l1_slot: 0,
            bump,
        };
        program_test.add_account(
            session,
            account(BALANCE, borsh::to_vec(&state).unwrap(), PORTAL),
        );
        let (vault, bump) = northstar_portal::find_fee_vault_pda(&PORTAL);
        let state = FeeVault {
            discriminator: FeeVault::DISCRIMINATOR,
            authority: payer.pubkey().to_bytes(),
            bump,
        };
        program_test.add_account(
            vault,
            account(BALANCE, borsh::to_vec(&state).unwrap(), PORTAL),
        );
        Self {
            program_test,
            payer,
            session,
            vault,
        }
    }

    fn delegated(&mut self, lamports: u64) -> Pubkey {
        let key = Pubkey::new_unique();
        self.program_test
            .add_account(key, account(lamports, vec![], PORTAL));
        let (record, bump) = northstar_portal::find_delegation_record_pda(&PORTAL, &key);
        let state = DelegationRecord {
            discriminator: DelegationRecord::DISCRIMINATOR,
            owner_program: system_program::id(),
            grid_id: 1,
            bump,
        };
        self.program_test.add_account(
            record,
            account(BALANCE, borsh::to_vec(&state).unwrap(), PORTAL),
        );
        key
    }

    fn settle(&self, keys: &[Pubkey], targets: &[u64]) -> Instruction {
        let mut lamports = [0; 7];
        lamports[..targets.len()].copy_from_slice(targets);
        let mut data = borsh::to_vec(&PortalInstruction::SettleAccountLamports(
            SettleAccountLamports {
                er_slot: ER_SLOT,
                checksum: [42; 32],
                account_count: keys.len() as u8,
                lamports,
            },
        ))
        .unwrap();
        // Older Portal versions ignore this extension, exercising the original conservation check.
        data.extend_from_slice(&initial_checksum());
        let mut accounts = vec![
            AccountMeta::new_readonly(self.payer.pubkey(), true),
            AccountMeta::new(self.session, false),
        ];
        for key in keys {
            accounts.push(AccountMeta::new(*key, false));
            accounts.push(AccountMeta::new_readonly(
                northstar_portal::find_delegation_record_pda(&PORTAL, key).0,
                false,
            ));
        }
        accounts.push(AccountMeta::new(self.vault, false));
        Instruction {
            program_id: PORTAL,
            accounts,
            data,
        }
    }
}

async fn send(
    context: &mut ProgramTestContext,
    payer: &Keypair,
    instructions: &[Instruction],
) -> Result<(), BanksClientError> {
    let tx = Transaction::new_signed_with_payer(
        instructions,
        Some(&payer.pubkey()),
        &[payer],
        context.banks_client.get_latest_blockhash().await.unwrap(),
    );
    context.banks_client.process_transaction(tx).await
}

async fn get(context: &mut ProgramTestContext, key: Pubkey) -> Account {
    context
        .banks_client
        .get_account(key)
        .await
        .unwrap()
        .unwrap()
}

fn assert_portal_error(error: BanksClientError, expected: PortalError) {
    assert_eq!(
        error.unwrap(),
        TransactionError::InstructionError(0, InstructionError::Custom(expected as u32))
    );
}

#[tokio::test]
async fn invariant_deposit_recipient_must_be_plain_system_account() {
    let mut fixture = Fixture::new();
    let delegated = fixture.delegated(BALANCE);
    let foreign = Pubkey::new_unique();
    let system_data = Pubkey::new_unique();
    fixture
        .program_test
        .add_account(foreign, account(BALANCE, vec![], Pubkey::new_unique()));
    fixture.program_test.add_account(
        system_data,
        account(BALANCE, vec![1; 4], system_program::id()),
    );
    let mut context = fixture.program_test.start_with_context().await;
    for recipient in [delegated, foreign, system_data, system_program::id()] {
        let before = get(&mut context, recipient).await;
        let receipt =
            northstar_portal::find_deposit_receipt_pda(&PORTAL, &fixture.session, &recipient).0;
        let instruction = Instruction {
            program_id: PORTAL,
            accounts: vec![
                AccountMeta::new(fixture.payer.pubkey(), true),
                AccountMeta::new_readonly(fixture.session, false),
                AccountMeta::new(receipt, false),
                AccountMeta::new_readonly(recipient, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data: borsh::to_vec(&PortalInstruction::DepositFee { lamports: 1 }).unwrap(),
        };
        assert_portal_error(
            send(&mut context, &fixture.payer, &[instruction])
                .await
                .unwrap_err(),
            PortalError::InvalidAccountData,
        );
        assert_eq!(get(&mut context, recipient).await, before);
        assert!(context
            .banks_client
            .get_account(receipt)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn invariant_settlement_surplus_preserves_total_and_checkpoint_balances() {
    let mut fixture = Fixture::new();
    let a = fixture.delegated(BALANCE + 1);
    let b = fixture.delegated(BALANCE);
    let instruction = fixture.settle(&[a, b], &[BALANCE - 3, BALANCE + 3]);
    let mut context = fixture.program_test.start_with_context().await;
    send(
        &mut context,
        &fixture.payer,
        std::slice::from_ref(&instruction),
    )
    .await
    .unwrap();
    assert_eq!(get(&mut context, a).await.lamports, BALANCE - 3);
    assert_eq!(get(&mut context, b).await.lamports, BALANCE + 3);
    assert_eq!(get(&mut context, fixture.vault).await.lamports, BALANCE + 1);
    let settled_session = get(&mut context, fixture.session).await;
    let expected_accumulator = [(a, BALANCE - 3), (b, BALANCE + 3)].into_iter().fold(
        initial_checksum(),
        |accumulator, (key, target)| {
            hashv(&[
                &accumulator,
                b"lamports",
                key.as_ref(),
                &target.to_le_bytes(),
            ])
            .to_bytes()
        },
    );
    assert_eq!(
        Session::try_from_slice(&settled_session.data)
            .unwrap()
            .settlement_accumulator,
        expected_accumulator
    );
    // A later donation must not make a retry accumulate the same settlement operation twice.
    send(
        &mut context,
        &fixture.payer,
        &[transfer(&fixture.payer.pubkey(), &a, 2), instruction],
    )
    .await
    .unwrap();
    assert_eq!(get(&mut context, fixture.session).await, settled_session);
    assert_eq!(get(&mut context, a).await.lamports, BALANCE - 1);
    assert_eq!(get(&mut context, fixture.vault).await.lamports, BALANCE + 1);
}

#[tokio::test]
async fn invariant_guarded_equal_balances_still_advance_accumulator() {
    let mut fixture = Fixture::new();
    let key = fixture.delegated(BALANCE);
    let instruction = fixture.settle(&[key], &[BALANCE]);
    let mut context = fixture.program_test.start_with_context().await;
    send(&mut context, &fixture.payer, &[instruction])
        .await
        .unwrap();
    let state = Session::try_from_slice(&get(&mut context, fixture.session).await.data).unwrap();
    assert_ne!(state.settlement_accumulator, initial_checksum());
}

#[tokio::test]
async fn invariant_surplus_overflow_and_rent_checks_preserve_state() {
    for below_rent in [false, true] {
        let mut fixture = Fixture::new();
        let key = fixture.delegated(BALANCE + 1);
        let instruction = fixture.settle(&[key], &[if below_rent { 0 } else { BALANCE }]);
        let mut context = fixture.program_test.start_with_context().await;
        let mut source = get(&mut context, key).await;
        let mut vault = get(&mut context, fixture.vault).await;
        let expected = if below_rent {
            source.data = vec![1];
            context.set_account(&key, &source.clone().into());
            PortalError::SettlementLamportsBelowRentExempt
        } else {
            vault.lamports = u64::MAX;
            context.set_account(&fixture.vault, &vault.clone().into());
            PortalError::ArithmeticOverflow
        };
        assert_portal_error(
            send(&mut context, &fixture.payer, &[instruction])
                .await
                .unwrap_err(),
            expected,
        );
        assert_eq!(get(&mut context, key).await, source);
        assert_eq!(get(&mut context, fixture.vault).await, vault);
    }
}

#[tokio::test]
async fn invariant_settlement_guard_rejects_wrong_accumulator_and_vault() {
    let mut fixture = Fixture::new();
    let a = fixture.delegated(BALANCE + 1);
    let instruction = fixture.settle(&[a], &[BALANCE]);
    let mut context = fixture.program_test.start_with_context().await;
    let before = get(&mut context, a).await;
    let mut wrong_guard = instruction.clone();
    *wrong_guard.data.last_mut().unwrap() ^= 1;
    assert_portal_error(
        send(&mut context, &fixture.payer, &[wrong_guard])
            .await
            .unwrap_err(),
        PortalError::SettlementChecksumMismatch,
    );
    let mut wrong_vault = instruction;
    wrong_vault.accounts.last_mut().unwrap().pubkey = fixture.payer.pubkey();
    assert_portal_error(
        send(&mut context, &fixture.payer, &[wrong_vault])
            .await
            .unwrap_err(),
        PortalError::InvalidPdaSeeds,
    );
    assert_eq!(get(&mut context, a).await, before);
}

#[tokio::test]
async fn invariant_settlement_supports_seven_accounts_and_legacy_encoding() {
    let mut fixture = Fixture::new();
    let keys = (0..7)
        .map(|_| fixture.delegated(BALANCE + 1))
        .collect::<Vec<_>>();
    let instruction = fixture.settle(&keys, &[BALANCE; 7]);
    let mut context = fixture.program_test.start_with_context().await;
    send(&mut context, &fixture.payer, &[instruction])
        .await
        .unwrap();
    assert_eq!(get(&mut context, fixture.vault).await.lamports, BALANCE + 7);
    for key in keys {
        assert_eq!(get(&mut context, key).await.lamports, BALANCE);
    }

    let mut fixture = Fixture::new();
    let a = fixture.delegated(BALANCE);
    let b = fixture.delegated(BALANCE);
    let mut legacy = fixture.settle(&[a, b], &[BALANCE - 1, BALANCE + 1]);
    legacy.data.truncate(legacy.data.len() - 32);
    legacy.accounts.pop();
    let mut context = fixture.program_test.start_with_context().await;
    send(&mut context, &fixture.payer, &[legacy]).await.unwrap();
    assert_eq!(get(&mut context, a).await.lamports, BALANCE - 1);
    assert_eq!(get(&mut context, b).await.lamports, BALANCE + 1);
}

#[tokio::test]
async fn invariant_settlement_deficit_preserves_state() {
    let mut fixture = Fixture::new();
    let a = fixture.delegated(BALANCE - 1);
    let instruction = fixture.settle(&[a], &[BALANCE]);
    let mut context = fixture.program_test.start_with_context().await;
    let before = get(&mut context, a).await;
    let vault_before = get(&mut context, fixture.vault).await;
    assert_portal_error(
        send(&mut context, &fixture.payer, &[instruction])
            .await
            .unwrap_err(),
        PortalError::SettlementLamportsNotConserved,
    );
    assert_eq!(get(&mut context, a).await, before);
    assert_eq!(get(&mut context, fixture.vault).await, vault_before);
}

#[tokio::test]
async fn invariant_prefunded_session_bridge_initializes_and_preserves_excess() {
    for prefunding in [1, BALANCE] {
        let mut fixture = Fixture::new();
        let mint = Pubkey::new_unique();
        let bridge_program = Pubkey::new_unique();
        let token_program = pinocchio_token::ID;
        let (bridge, _) =
            northstar_portal::find_session_bridge_pda(&PORTAL, &fixture.session, &mint);
        let vault =
            Pubkey::find_program_address(&[b"token_vault", bridge.as_ref()], &bridge_program).0;
        let mut mint_data = vec![0; 82];
        mint_data[44] = 9;
        mint_data[45] = 1;
        fixture
            .program_test
            .add_account(mint, account(BALANCE, mint_data, token_program));
        for program in [bridge_program, token_program] {
            let mut executable = account(BALANCE, vec![], solana_sdk_ids::native_loader::id());
            executable.executable = true;
            fixture.program_test.add_account(program, executable);
        }
        fixture
            .program_test
            .add_account(bridge, account(prefunding, vec![], system_program::id()));
        let instruction = Instruction {
            program_id: PORTAL,
            accounts: vec![
                AccountMeta::new(fixture.payer.pubkey(), true),
                AccountMeta::new_readonly(fixture.session, false),
                AccountMeta::new(bridge, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(bridge_program, false),
                AccountMeta::new_readonly(token_program, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data: borsh::to_vec(&PortalInstruction::RegisterSessionBridge(
                RegisterSessionBridge {
                    mint,
                    bridge_program,
                    vault,
                    token_program,
                },
            ))
            .unwrap(),
        };
        let mut context = fixture.program_test.start_with_context().await;
        send(
            &mut context,
            &fixture.payer,
            std::slice::from_ref(&instruction),
        )
        .await
        .unwrap();
        let initialized = get(&mut context, bridge).await;
        let state = SessionBridge::try_from_slice(&initialized.data).unwrap();
        assert!(state.is_valid());
        assert_eq!(state.session, fixture.session);
        assert_eq!(initialized.owner, PORTAL);
        let rent = context.banks_client.get_rent().await.unwrap();
        assert_eq!(
            initialized.lamports,
            prefunding.max(rent.minimum_balance(initialized.data.len()))
        );
        send(
            &mut context,
            &fixture.payer,
            &[
                transfer(&fixture.payer.pubkey(), &fixture.vault, 1),
                instruction,
            ],
        )
        .await
        .unwrap();
        assert_eq!(get(&mut context, bridge).await, initialized);
    }
}
