use {
    super::*,
    crate::{
        checkpoint_v2::CheckpointArtifactV2,
        portal_e2e_tests::token_bridge_v2::{delegated_token_world, TokenWorldV2},
    },
    solana_program_pack::Pack,
};

pub(super) type TestResult<T> = std::result::Result<T, String>;

fn token_amount(bank: &Bank, address: &Pubkey) -> TestResult<u64> {
    let value = bank
        .get_account(address)
        .ok_or_else(|| format!("missing token account {address}"))?;
    spl_token_interface::state::Account::unpack(value.data())
        .map(|value| value.amount)
        .map_err(|error| error.to_string())
}

fn er_amount(manager: &crate::Manager, account: &Pubkey) -> TestResult<u64> {
    let runtime = manager.runtime.as_ref().ok_or("runtime missing")?;
    let value = runtime
        .bank()
        .get_account(account)
        .ok_or("ER token account missing")?;
    borsh::from_slice::<northstar_token_bridge::state::ErTokenAccount>(value.data())
        .map(|value| value.amount)
        .map_err(|error| error.to_string())
}

fn withdraw(
    world: &TokenWorldV2,
    manager: &crate::Manager,
    destination: Pubkey,
    amount: u64,
) -> TestResult<()> {
    let before = er_amount(manager, &world.er)?;
    let runtime = manager.runtime.as_ref().ok_or("runtime missing")?;
    let transaction = Transaction::new_signed_with_payer(
        &[world.start_withdrawal(destination, amount)],
        Some(&world.payer.pubkey()),
        &[world.payer.as_ref()],
        runtime.bank().last_blockhash(),
    );
    TransactionClient::send_transactions_in_batch(
        &runtime._tx_client,
        vec![
            bincode::serialize(&solana_transaction::versioned::VersionedTransaction::from(
                transaction,
            ))
            .unwrap(),
        ],
        &SendTransactionServiceStats::default(),
    );
    let after = er_amount(manager, &world.er)?;
    if before.checked_sub(amount) != Some(after) {
        return Err(format!(
            "ER token withdrawal did not execute: before={before}, after={after}, amount={amount}"
        ));
    }
    Ok(())
}

fn settle(
    manager: &mut crate::Manager,
    root: &mut Arc<Bank>,
    destination: &Pubkey,
    expected: u64,
) -> TestResult<CheckpointArtifactV2> {
    settle_checkpoint(manager, root, |bank| {
        token_amount(bank, destination).map(|amount| amount == expected)
    })
}

pub(super) fn settle_checkpoint<F>(
    manager: &mut crate::Manager,
    root: &mut Arc<Bank>,
    asset_complete: F,
) -> TestResult<CheckpointArtifactV2>
where
    F: Fn(&Bank) -> TestResult<bool>,
{
    let runtime = manager.runtime.as_ref().ok_or("runtime missing")?;
    if !runtime.seal_checkpoint_if_nonempty() {
        return Err("checkpoint did not seal".into());
    }
    let artifact = runtime
        .checkpoint_artifact_v2()
        .ok_or("sealed artifact missing")?;
    for phase in 0..16 {
        manager.reanchor_to_l1_parent(root.clone());
        manager.process_finalized_l1_inputs(root);
        let runtime = manager.runtime.as_ref().ok_or("runtime missing")?;
        let store = runtime.lineage_store_v2().ok_or("lineage missing")?;
        let complete = {
            let store = store.lock().unwrap();
            store.sealed().is_none()
                && store
                    .accepted_head()
                    .is_some_and(|head| head.state_root == artifact.checkpoint.new_state_root)
        };
        drop(store);
        if complete && asset_complete(root)? {
            return Ok(artifact);
        }
        let bank = Bank::new_from_parent(
            root.clone(),
            SlotLeader::default(),
            root.slot() + crate::DEFAULT_CHECKPOINT_CHALLENGE_WINDOW_SLOTS + 1,
        );
        let (_, _, transactions) = manager
            .settlement_transactions_if_due(&bank, bank.last_blockhash())
            .ok_or_else(|| {
                format!(
                    "settlement phase {phase} produced no transactions; complete={complete}, \
                     asset_complete={:?}, active={}",
                    asset_complete(root),
                    manager.has_active_runtime()
                )
            })?;
        if transactions.is_empty() {
            return Err(format!("settlement phase {phase} is empty"));
        }
        for transaction in transactions {
            bank.process_transaction(&transaction).map_err(|error| {
                format!(
                    "settlement phase {phase}, selectors {:?}: {error:?}",
                    transaction
                        .message
                        .instructions
                        .iter()
                        .filter_map(|instruction| instruction.data.first().copied())
                        .collect::<Vec<_>>()
                )
            })?;
            if phase == 2 {
                break;
            }
        }
        bank.freeze();
        *root = Arc::new(bank);
        if phase == 2 {
            let config = manager.config.clone();
            manager.shutdown_runtime();
            *manager = crate::Manager::new(config);
            manager
                .init_runtime(
                    root.clone(),
                    create_test_cluster_info(),
                    find_free_addr(),
                    find_free_addr(),
                    find_free_addr(),
                )
                .map_err(|error| error.to_string())?;
            if !manager.resume_active_session_from_l1(root.clone()) {
                let session =
                    northstar_portal::find_session_pda(&manager.config.portal_program_id).0;
                let account = root.get_account(&session);
                let physical = account.as_ref().map(|account| {
                    (
                        account.owner(),
                        account.data().len(),
                        account.data().get(..8),
                    )
                });
                let frame = account.as_ref().and_then(|account| {
                    borsh::from_slice::<northstar_portal::SessionV2>(account.data()).ok()
                });
                let binding = frame.map(|frame| {
                    (
                        frame.is_valid(),
                        frame.base.created_at,
                        frame.base.ttl_slots,
                        frame.base.validator == manager.config.manager_account.pubkey(),
                        frame.base.settlement_status,
                    )
                });
                return Err(format!(
                    "partial metadata settlement did not resume: phase={phase}, L1 slot={}, \
                     active={}, physical={physical:?}, binding={binding:?}",
                    root.slot(),
                    manager.has_active_runtime()
                ));
            }
        }
    }
    Err("token payout did not reconcile within bounded phases".into())
}

#[test]
fn regression_v2_token_transfer_captures_both_delegated_balances() {
    let world = delegated_token_world(PORTAL_PROGRAM_ID, true);
    let owner = Keypair::new();
    let destination = world.delegate_token_owner(&owner, 3);
    let source = world.funded_tokens(100);
    world.send(&[world.deposit(source, 42, 4)], &[]).unwrap();
    world.bank.freeze();
    let original_source = world.bank.get_account(&world.er).unwrap();
    let original_destination = world.bank.get_account(&destination).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let config = crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    };
    let mut manager = crate::Manager::new(config.clone());
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
    manager.process_finalized_l1_inputs(&world.bank);
    let result: TestResult<()> =
        (|| {
            let before = (
                er_amount(&manager, &world.er)?,
                er_amount(&manager, &destination)?,
            );
            if before != (42, 0) {
                return Err(format!("authenticated token credit differs: {before:?}"));
            }
            let runtime = manager.runtime.as_ref().ok_or("runtime missing")?;
            let instruction = Instruction {
                program_id: world.bridge,
                accounts: vec![
                    solana_instruction::AccountMeta::new_readonly(world.payer.pubkey(), true),
                    solana_instruction::AccountMeta::new(world.er, false),
                    solana_instruction::AccountMeta::new(destination, false),
                ],
                data: borsh::to_vec(
                    &northstar_token_bridge::instruction::TokenBridgeInstruction::Transfer {
                        amount: 20,
                    },
                )
                .unwrap(),
            };
            let transaction = Transaction::new_signed_with_payer(
                &[instruction],
                Some(&world.payer.pubkey()),
                &[world.payer.as_ref()],
                runtime.bank().last_blockhash(),
            );
            TransactionClient::send_transactions_in_batch(
                &runtime._tx_client,
                vec![bincode::serialize(
                    &solana_transaction::versioned::VersionedTransaction::from(transaction),
                )
                .unwrap()],
                &SendTransactionServiceStats::default(),
            );
            let after = (
                er_amount(&manager, &world.er)?,
                er_amount(&manager, &destination)?,
            );
            if after != (22, 20) {
                return Err(format!(
                    "v2 token transfer did not execute: before={before:?}, after={after:?}"
                ));
            }
            let store = runtime.lineage_store_v2().ok_or("lineage missing")?;
            let executions = store
                .lock()
                .unwrap()
                .unsealed()
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.payload,
                        crate::continuity_v2::TransitionPayloadV2::Execution { .. }
                    )
                })
                .count();
            if executions != 1 {
                return Err(format!(
                    "ordinary token transfer capture count differs: {executions}"
                ));
            }
            for (authority, target) in [(&owner, destination), (world.payer.as_ref(), world.er)] {
                let instruction = Instruction {
                    program_id: world.bridge,
                    accounts: vec![
                        solana_instruction::AccountMeta::new_readonly(authority.pubkey(), true),
                        solana_instruction::AccountMeta::new(world.er, false),
                        solana_instruction::AccountMeta::new(target, false),
                    ],
                    data: borsh::to_vec(
                        &northstar_token_bridge::instruction::TokenBridgeInstruction::Transfer {
                            amount: 1,
                        },
                    )
                    .unwrap(),
                };
                let mut signers = vec![world.payer.as_ref()];
                if authority.pubkey() != world.payer.pubkey() {
                    signers.push(authority);
                }
                let transaction = Transaction::new_signed_with_payer(
                    &[instruction],
                    Some(&world.payer.pubkey()),
                    &signers,
                    runtime.bank().last_blockhash(),
                );
                TransactionClient::send_transactions_in_batch(
                    &runtime._tx_client,
                    vec![bincode::serialize(
                        &solana_transaction::versioned::VersionedTransaction::from(transaction),
                    )
                    .unwrap()],
                    &SendTransactionServiceStats::default(),
                );
                let preserved = (
                    er_amount(&manager, &world.er)?,
                    er_amount(&manager, &destination)?,
                );
                if preserved != (22, 20) {
                    return Err(format!(
                        "invalid transfer changed token balances: {preserved:?}"
                    ));
                }
            }
            Ok(())
        })();
    manager.shutdown_runtime();
    assert!(result.is_ok(), "{result:?}");
    let mut manager = crate::Manager::new(config);
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    let resumed = manager.resume_active_session_from_l1(world.bank.clone());
    let restored = (
        er_amount(&manager, &world.er),
        er_amount(&manager, &destination),
    );
    manager.shutdown_runtime();
    assert!(resumed);
    assert_eq!(restored, (Ok(22), Ok(20)));
    assert_eq!(world.bank.get_account(&world.er).unwrap(), original_source);
    assert_eq!(
        world.bank.get_account(&destination).unwrap(),
        original_destination
    );
}

#[test]
fn invariant_v2_token_reconciliation_rejects_changed_authorization_tuple() {
    let mut world = delegated_token_world(PORTAL_PROGRAM_ID, true);
    let source = world.funded_tokens(100);
    let destination = world.funded_tokens(0);
    world.send(&[world.deposit(source, 42, 3)], &[]).unwrap();
    world.bank.freeze();
    let directory = tempfile::tempdir().unwrap();
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    });
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
    manager.process_finalized_l1_inputs(&world.bank);
    let withdrawn = withdraw(&world, &manager, destination, 20);
    let plan = manager.settlement_plan().unwrap();
    let payout = &plan.token_withdrawals[0];
    world.bank = Arc::new(Bank::new_from_parent(
        world.bank.clone(),
        SlotLeader::default(),
        world.bank.slot() + 1,
    ));
    let mut physical = world.bank.get_account(&payout.vault).unwrap();
    let mut vault: northstar_token_bridge::state::TokenVault =
        borsh::from_slice(physical.data()).unwrap();
    vault.withdrawn = payout.withdrawn;
    physical.set_data_from_slice(&borsh::to_vec(&vault).unwrap());
    world.bank.store_account(&payout.vault, &physical);
    let checkpoint = Pubkey::new_unique();
    let (key, bump) = northstar_portal::find_token_withdrawal_authorization_pda(
        &PORTAL_PROGRAM_ID,
        &checkpoint,
        &payout.vault,
        payout.withdrawn,
    );
    let authorization = northstar_portal::TokenWithdrawalAuthorization {
        discriminator: northstar_portal::TokenWithdrawalAuthorization::DISCRIMINATOR,
        checkpoint,
        tuple_hash: [7; 32],
        consumed: true,
        bump,
    };
    let data = borsh::to_vec(&authorization).unwrap();
    let mut physical = AccountSharedData::new(1_000_000, data.len(), &PORTAL_PROGRAM_ID);
    physical.set_data_from_slice(&data);
    world.bank.store_account(&key, &physical);
    let runtime = manager.runtime.as_ref().unwrap();
    let rejected = runtime
        .token_payouts_complete_v2(&world.bank, checkpoint, &plan.token_withdrawals)
        .is_err();
    manager.shutdown_runtime();
    assert!(withdrawn.is_ok(), "{withdrawn:?}");
    assert!(
        rejected,
        "a consumed authorization with another tuple must not resolve a payout"
    );
}

#[test]
fn regression_v2_token_plan_rejects_missing_registry_without_dropping_payout() {
    let mut world = delegated_token_world(PORTAL_PROGRAM_ID, true);
    let source = world.funded_tokens(100);
    let destination = world.funded_tokens(0);
    world.send(&[world.deposit(source, 42, 3)], &[]).unwrap();
    world.bank.freeze();
    let directory = tempfile::tempdir().unwrap();
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    });
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
    manager.process_finalized_l1_inputs(&world.bank);
    let withdrawn = withdraw(&world, &manager, destination, 20);
    let value = world.bank.get_account(&world.er).unwrap();
    let token: northstar_token_bridge::state::ErTokenAccount =
        borsh::from_slice(value.data()).unwrap();
    let registry = Pubkey::new_from_array(token.session_bridge);
    world.bank = Arc::new(Bank::new_from_parent(
        world.bank.clone(),
        SlotLeader::default(),
        world.bank.slot() + 1,
    ));
    world
        .bank
        .store_account(&registry, &AccountSharedData::default());
    world.bank.freeze();
    manager.reanchor_to_l1_parent(world.bank.clone());
    let rejected = manager.settlement_plan().is_none();
    let active = manager.has_active_runtime();
    manager.shutdown_runtime();
    assert!(withdrawn.is_ok(), "{withdrawn:?}");
    assert!(rejected);
    assert!(!active);
    assert_eq!(token_amount(&world.bank, &destination).unwrap(), 0);
}

#[test]
fn regression_v2_cpi_token_withdrawal_records_and_settles_one_effect() {
    let mut world = delegated_token_world(PORTAL_PROGRAM_ID, true);
    let source = world.funded_tokens(100);
    let destination = world.funded_tokens(0);
    world.send(&[world.deposit(source, 42, 3)], &[]).unwrap();
    let caller = Pubkey::new_unique();
    let directory =
        std::env::var_os("BPF_OUT_DIR").expect("explicit CPI fixture artifact required");
    let program =
        std::fs::read(std::path::PathBuf::from(directory).join("northstar_continuity_fixture.so"))
            .unwrap();
    let executable: AccountSharedData = solana_account::Account {
        lamports: solana_rent::Rent::default().minimum_balance(program.len()),
        data: program,
        owner: solana_sdk_ids::bpf_loader::id(),
        executable: true,
        rent_epoch: 0,
    }
    .into();
    world.bank.store_account(&caller, &executable);
    world.bank.freeze();
    let directory = tempfile::tempdir().unwrap();
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    });
    let result =
        (|| -> TestResult<()> {
            manager
                .init_runtime(
                    world.bank.clone(),
                    create_test_cluster_info(),
                    find_free_addr(),
                    find_free_addr(),
                    find_free_addr(),
                )
                .map_err(|error| error.to_string())?;
            manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
            manager.process_finalized_l1_inputs(&world.bank);
            let runtime = manager.runtime.as_ref().ok_or("runtime missing")?;
            let inner = world.start_withdrawal(destination, 20);
            let mut accounts = vec![solana_instruction::AccountMeta::new_readonly(
                world.bridge,
                false,
            )];
            accounts.extend(inner.accounts);
            let mut data = vec![0];
            data.extend(inner.data);
            let transaction = Transaction::new_signed_with_payer(
                &[solana_instruction::Instruction {
                    program_id: caller,
                    accounts,
                    data,
                }],
                Some(&world.payer.pubkey()),
                &[world.payer.as_ref()],
                runtime.bank().last_blockhash(),
            );
            TransactionClient::send_transactions_in_batch(
                &runtime._tx_client,
                vec![bincode::serialize(
                    &solana_transaction::versioned::VersionedTransaction::from(transaction),
                )
                .unwrap()],
                &SendTransactionServiceStats::default(),
            );
            if er_amount(&manager, &world.er)? != 22 {
                return Err("CPI withdrawal did not execute".into());
            }
            let store = runtime.lineage_store_v2().ok_or("lineage missing")?;
            let events = store
                .lock()
                .unwrap()
                .pending_payout_events()
                .map_err(|error| error.to_string())?
                .1;
            if events.len() != 1
                || events[0].amount != 20
                || events[0].l1_destination_token_account != destination
            {
                return Err("CPI withdrawal was not captured exactly once".into());
            }
            drop(store);
            settle(&mut manager, &mut world.bank, &destination, 20)?;
            Ok(())
        })();
    manager.shutdown_runtime();
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn regression_v2_token_withdrawal_rejects_wrong_mint_decimals() {
    let world = delegated_token_world(PORTAL_PROGRAM_ID, true);
    let source = world.funded_tokens(100);
    let destination = world.funded_tokens(0);
    world.send(&[world.deposit(source, 42, 3)], &[]).unwrap();
    world.bank.freeze();
    let directory = tempfile::tempdir().unwrap();
    let mut manager = crate::Manager::new(crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    });
    manager
        .init_runtime(
            world.bank.clone(),
            create_test_cluster_info(),
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
        )
        .unwrap();
    manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
    manager.process_finalized_l1_inputs(&world.bank);
    let runtime = manager.runtime.as_ref().unwrap();
    let mut instruction = world.start_withdrawal(destination, 20);
    *instruction.data.last_mut().unwrap() = 7;
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&world.payer.pubkey()),
        &[world.payer.as_ref()],
        runtime.bank().last_blockhash(),
    );
    TransactionClient::send_transactions_in_batch(
        &runtime._tx_client,
        vec![
            bincode::serialize(&solana_transaction::versioned::VersionedTransaction::from(
                transaction,
            ))
            .unwrap(),
        ],
        &SendTransactionServiceStats::default(),
    );
    let after = er_amount(&manager, &world.er);
    let store = runtime.lineage_store_v2().unwrap();
    let payouts = store
        .lock()
        .unwrap()
        .pending_payout_events()
        .unwrap()
        .1
        .len();
    drop(store);
    manager.shutdown_runtime();
    assert_eq!(
        after.unwrap(),
        42,
        "invalid token decimals must not create a debit that only fails during L1 planning"
    );
    assert_eq!(payouts, 0);
}

#[test]
fn regression_v2_spl_payout_chains_after_restart_and_later_topup() {
    let mut world = delegated_token_world(PORTAL_PROGRAM_ID, true);
    let source = world.funded_tokens(1_000);
    let destination = world.funded_tokens(0);
    world.send(&[world.deposit(source, 42, 3)], &[]).unwrap();
    world.bank.freeze();
    let directory = tempfile::tempdir().unwrap();
    let config = crate::ManagerConfig {
        portal_program_id: PORTAL_PROGRAM_ID,
        manager_account: world.payer.clone(),
        checkpoint_plan_dir: Some(directory.path().join("plans")),
    };
    let mut manager = crate::Manager::new(config.clone());
    let result = (|| -> TestResult<()> {
        manager
            .init_runtime(
                world.bank.clone(),
                create_test_cluster_info(),
                find_free_addr(),
                find_free_addr(),
                find_free_addr(),
            )
            .map_err(|error| error.to_string())?;
        manager.activate_session(world.bank.clone(), world.session, 0, 10_000, 0);
        manager.process_finalized_l1_inputs(&world.bank);
        if er_amount(&manager, &world.er)? != 42 {
            return Err("authenticated initial token credit missing".into());
        }
        withdraw(&world, &manager, destination, 20)?;
        let first = settle(&mut manager, &mut world.bank, &destination, 20)
            .map_err(|error| format!("first SPL checkpoint: {error}"))?;
        if token_amount(&world.bank, &destination)? != 20 {
            return Err("first L1 payout missing".into());
        }
        manager.shutdown_runtime();
        manager = crate::Manager::new(config);
        manager
            .init_runtime(
                world.bank.clone(),
                create_test_cluster_info(),
                find_free_addr(),
                find_free_addr(),
                find_free_addr(),
            )
            .map_err(|error| error.to_string())?;
        if !manager.resume_active_session_from_l1(world.bank.clone()) {
            return Err("settled token session did not resume".into());
        }
        manager.process_finalized_l1_inputs(&world.bank);
        if er_amount(&manager, &world.er)? != 22 {
            return Err("settled ER balance did not resume".into());
        }
        world.bank = Arc::new(Bank::new_from_parent(
            world.bank.clone(),
            SlotLeader::default(),
            world.bank.slot() + 1,
        ));
        world
            .send(&[world.deposit(source, 7, 4)], &[])
            .map_err(|error| format!("later topup: {error:?}"))?;
        world.bank.freeze();
        manager.reanchor_to_l1_parent(world.bank.clone());
        manager.process_finalized_l1_inputs(&world.bank);
        if er_amount(&manager, &world.er)? != 29 {
            return Err("later topup did not execute after restart".into());
        }
        withdraw(&world, &manager, destination, 9)?;
        let second = settle(&mut manager, &mut world.bank, &destination, 29)
            .map_err(|error| format!("second SPL checkpoint: {error}"))?;
        if second.checkpoint.previous_state_root != first.checkpoint.new_state_root {
            return Err("second SPL checkpoint does not chain from accepted first root".into());
        }
        if er_amount(&manager, &world.er)? != 20 || token_amount(&world.bank, &destination)? != 29 {
            return Err("second SPL payout or ER balance differs".into());
        }
        Ok(())
    })();
    manager.shutdown_runtime();
    assert!(result.is_ok(), "{result:?}");
}
