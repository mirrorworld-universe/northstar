use {
    super::*,
    northstar_portal::{
        AccumulateTokenWithdrawal, Checkpoint, CheckpointBondStatus, CheckpointCursor,
        CheckpointStatus, FinishSettlement, PortalInstruction, SessionBridgeV2, SessionV2,
        TokenWithdrawalAuthorization,
    },
    northstar_token_bridge::state::TokenVault,
    solana_sha256_hasher::hashv,
};

fn seed(
    context: &mut ProgramTestContext,
    key: Pubkey,
    owner: Pubkey,
    state: &impl borsh::BorshSerialize,
) {
    let data = borsh::to_vec(state).unwrap();
    context.set_account(
        &key,
        &Account {
            lamports: Rent::default().minimum_balance(data.len()),
            data,
            owner,
            executable: false,
            // Match the bank's canonical marker for an already rent-exempt account.
            rent_epoch: u64::MAX,
        }
        .into(),
    );
}

async fn read(context: &mut ProgramTestContext, key: Pubkey) -> Account {
    context
        .banks_client
        .get_account(key)
        .await
        .unwrap()
        .unwrap()
}

fn holding(registry: Pubkey, mint: Pubkey, owner: Pubkey, amount: u64) -> (Pubkey, ErTokenAccount) {
    let (key, bump) = find_er_token_account_pda(&northstar_token_bridge::id(), &registry, &owner);
    (
        key,
        ErTokenAccount {
            discriminator: ErTokenAccount::DISCRIMINATOR,
            session_bridge: registry.to_bytes(),
            mint: mint.to_bytes(),
            owner: owner.to_bytes(),
            amount,
            bump,
        },
    )
}

#[tokio::test]
async fn er_transfer_conserves_amount_and_preserves_account_identity() {
    let mut w = TestWorld::new().await;
    let id = northstar_token_bridge::id();
    let payer = w.payer.pubkey();
    let (a, alice) = holding(w.session_bridge, w.mint, w.alice.pubkey(), 8);
    let (b, bob) = holding(w.session_bridge, w.mint, w.bob.pubkey(), 1);
    seed(&mut w.context, a, id, &alice);
    seed(&mut w.context, b, id, &bob);
    let before = [read(&mut w.context, a).await, read(&mut w.context, b).await];
    let oversized = transfer_ix(&w, 9);
    assert!(
        process_result(&mut w.context, &payer, &[oversized], &[&w.payer, &w.alice])
            .await
            .is_err()
    );
    assert_eq!(
        [read(&mut w.context, a).await, read(&mut w.context, b).await],
        before
    );
    let exact = transfer_ix(&w, 8);
    process(&mut w.context, &payer, &[exact], &[&w.payer, &w.alice]).await;
    let mut expected = before;
    expected[0].data = borsh::to_vec(&ErTokenAccount { amount: 0, ..alice }).unwrap();
    expected[1].data = borsh::to_vec(&ErTokenAccount { amount: 9, ..bob }).unwrap();
    assert_eq!(
        [read(&mut w.context, a).await, read(&mut w.context, b).await],
        expected
    );
}

async fn settlement_snapshot(context: &mut ProgramTestContext, keys: [Pubkey; 4]) -> [Account; 4] {
    [
        read(context, keys[0]).await,
        read(context, keys[1]).await,
        read(context, keys[2]).await,
        read(context, keys[3]).await,
    ]
}

async fn execute(
    w: &mut TestWorld,
    ix: Instruction,
) -> (
    Result<(), solana_transaction::TransactionError>,
    Vec<String>,
) {
    let hash = w.context.get_new_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&w.payer.pubkey()), &[&w.payer], hash);
    let meta = w
        .context
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    (meta.result, meta.metadata.unwrap().log_messages)
}

#[tokio::test]
async fn withdrawal_pays_once_and_token_failure_rolls_back_portal_consumption() {
    const SLOT: u64 = 44;
    const AMOUNT: u64 = 6;
    const TARGET: u64 = 10;
    let mut w = TestWorld::new().await;
    let portal = PORTAL_PROGRAM_ID;
    let bridge = northstar_token_bridge::id();
    let spl = spl_token_interface::id();
    let validator = w.payer.pubkey();
    let mut session = Session::try_from_slice(&read(&mut w.context, w.session).await.data).unwrap();
    session.nonce = 1;
    let mut frame = SessionV2 {
        discriminator: SessionV2::DISCRIMINATOR,
        base: session,
        l1_genesis: [1; 32],
        profile: [2; 32],
        ingress_count: 0,
        ingress_root: [0; 32],
        consumed_ingress_count: 0,
        consumed_ingress_root: [0; 32],
    };
    let binding = frame
        .identity(&portal, &w.session)
        .commitment(|parts| hashv(parts).to_bytes())
        .unwrap();
    let v2 = cfg!(feature = "checkpoint-continuity-v2");
    let (registry, registry_bump) = if v2 {
        northstar_portal::find_session_bridge_v2_pda(&portal, &binding, &w.mint)
    } else {
        northstar_portal::find_session_bridge_pda(&portal, &w.session, &w.mint)
    };
    let (vault, vault_bump) = find_token_vault_pda(&bridge, &registry);
    let (er, er_state) = holding(registry, w.mint, w.bob.pubkey(), 0);
    seed(&mut w.context, er, bridge, &er_state);
    let registration = SessionBridge {
        discriminator: SessionBridge::DISCRIMINATOR,
        session: w.session,
        mint: w.mint,
        bridge_program: bridge,
        vault,
        token_program: spl,
        bump: registry_bump,
    };
    let initial = hashv(&[b"northstar-settlement-v0", &SLOT.to_le_bytes()]).to_bytes();
    let checksum = hashv(&[
        &initial,
        b"token_withdrawal",
        bridge.as_ref(),
        registry.as_ref(),
        er.as_ref(),
        vault.as_ref(),
        w.vault_token.as_ref(),
        w.bob_token.as_ref(),
        w.mint.as_ref(),
        spl.as_ref(),
        &AMOUNT.to_le_bytes(),
        &TARGET.to_le_bytes(),
        &[DECIMALS],
    ])
    .to_bytes();
    session.settlement_er_slot = SLOT;
    session.settlement_status = SettlementStatus::InProgress;
    session.settlement_checksum = checksum;
    session.settlement_accumulator = initial;
    if v2 {
        frame.base = session;
        seed(&mut w.context, w.session, portal, &frame);
        seed(
            &mut w.context,
            registry,
            portal,
            &SessionBridgeV2 {
                discriminator: SessionBridgeV2::DISCRIMINATOR,
                base: registration,
                session_context: binding,
            },
        );
    } else {
        seed(&mut w.context, w.session, portal, &session);
        seed(&mut w.context, registry, portal, &registration);
    }
    let vault_state = TokenVault {
        discriminator: TokenVault::DISCRIMINATOR,
        session_bridge: registry.to_bytes(),
        mint: w.mint.to_bytes(),
        vault_token_account: w.vault_token.to_bytes(),
        token_program: spl.to_bytes(),
        deposited: TARGET,
        withdrawn: TARGET - AMOUNT,
        bump: vault_bump,
    };
    seed(&mut w.context, vault, bridge, &vault_state);
    w.context
        .set_account(&w.vault_token, &token_account(w.mint, vault, AMOUNT).into());
    let mut mint_account = mint_account();
    let mut mint = Mint::unpack(&mint_account.data).unwrap();
    mint.freeze_authority = Some(validator).into();
    Mint::pack(mint, &mut mint_account.data).unwrap();
    w.context.set_account(&w.mint, &mint_account.into());
    let (checkpoint, bump) = northstar_portal::find_checkpoint_pda(&portal, &w.session, SLOT);
    let chk = Checkpoint {
        discriminator: Checkpoint::DISCRIMINATOR,
        session: w.session,
        er_slot: SLOT,
        step_count: 16,
        previous_state_root: [1; 32],
        new_state_root: [2; 32],
        trace_root: [3; 32],
        tx_effect_root: [4; 32],
        readonly_l1_root: [5; 32],
        da_commitment: [6; 32],
        effect_commitment: checksum,
        proposer: validator,
        proposed_at_l1_slot: 0,
        challenge_deadline_l1_slot: 1,
        status: CheckpointStatus::Committed,
        bond_lamports: 1_000_000,
        bond_status: CheckpointBondStatus::Released,
        challenger: Pubkey::default(),
        challenged_at_l1_slot: 0,
        challenge_resolved: false,
        bump,
    };
    seed(&mut w.context, checkpoint, portal, &chk);
    let (cursor, bump) = northstar_portal::find_checkpoint_cursor_pda(&portal, &w.session);
    seed(
        &mut w.context,
        cursor,
        portal,
        &CheckpointCursor {
            discriminator: CheckpointCursor::DISCRIMINATOR,
            session: w.session,
            latest_finalized_checkpoint: checkpoint,
            latest_finalized_er_slot: SLOT,
            latest_finalized_state_root: chk.new_state_root,
            active_checkpoint: checkpoint,
            active_er_slot: SLOT,
            bump,
        },
    );
    let auth = northstar_portal::find_token_withdrawal_authorization_pda(
        &portal,
        &checkpoint,
        &vault,
        TARGET,
    )
    .0;
    let ro = |key| AccountMeta::new_readonly(key, false);
    let rw = |key| AccountMeta::new(key, false);
    let validator_meta = AccountMeta::new_readonly(validator, true);
    let accumulate = Instruction {
        program_id: portal,
        accounts: vec![
            AccountMeta::new(validator, true),
            rw(w.session),
            ro(registry),
            ro(vault),
            ro(er),
            ro(w.vault_token),
            ro(w.bob_token),
            ro(w.mint),
            ro(spl),
            rw(auth),
            ro(system_program::id()),
        ],
        data: borsh::to_vec(&PortalInstruction::AccumulateTokenWithdrawal(
            AccumulateTokenWithdrawal {
                er_slot: SLOT,
                checksum,
                amount: AMOUNT,
                withdrawn: TARGET,
                decimals: DECIMALS,
            },
        ))
        .unwrap(),
    };
    let finish = Instruction {
        program_id: portal,
        accounts: vec![
            validator_meta.clone(),
            rw(w.session),
            rw(checkpoint),
            rw(cursor),
        ],
        data: borsh::to_vec(&PortalInstruction::FinishSettlement(FinishSettlement {
            er_slot: SLOT,
            checksum,
        }))
        .unwrap(),
    };
    let freeze = spl_token_interface::instruction::freeze_account(
        &spl,
        &w.bob_token,
        &w.mint,
        &validator,
        &[],
    )
    .unwrap();
    process(
        &mut w.context,
        &validator,
        &[accumulate, finish, freeze],
        &[&w.payer],
    )
    .await;

    let settle = Instruction {
        program_id: bridge,
        accounts: vec![
            validator_meta,
            ro(w.session),
            ro(checkpoint),
            rw(auth),
            ro(registry),
            ro(portal),
            rw(vault),
            ro(er),
            rw(w.vault_token),
            rw(w.bob_token),
            ro(w.mint),
            ro(spl),
        ],
        data: borsh::to_vec(&TokenBridgeInstruction::SettleWithdrawal {
            er_slot: SLOT,
            checksum,
            amount: AMOUNT,
            withdrawn: TARGET,
            decimals: DECIMALS,
        })
        .unwrap(),
    };
    let keys = [auth, vault, w.vault_token, w.bob_token];
    let before = settlement_snapshot(&mut w.context, keys).await;
    assert!(
        !TokenWithdrawalAuthorization::try_from_slice(&before[0].data)
            .unwrap()
            .consumed
    );
    let (failure, logs) = execute(&mut w, settle.clone()).await;
    assert!(failure.is_err());
    assert!(logs.contains(&format!("Program {portal} success")));
    assert!(logs
        .iter()
        .any(|m| m.starts_with(&format!("Program {spl} failed:"))));
    assert_eq!(settlement_snapshot(&mut w.context, keys).await, before);
    let thaw = spl_token_interface::instruction::thaw_account(
        &spl,
        &w.bob_token,
        &w.mint,
        &validator,
        &[],
    )
    .unwrap();
    process(&mut w.context, &validator, &[thaw], &[&w.payer]).await;
    let (paid, logs) = execute(&mut w, settle.clone()).await;
    paid.unwrap();
    for program in [portal, spl] {
        assert!(logs.contains(&format!("Program {program} success")));
    }
    let after = settlement_snapshot(&mut w.context, keys).await;
    assert!(
        TokenWithdrawalAuthorization::try_from_slice(&after[0].data)
            .unwrap()
            .consumed
    );
    assert_eq!(
        TokenVault::try_from_slice(&after[1].data).unwrap(),
        TokenVault {
            withdrawn: TARGET,
            ..vault_state
        }
    );
    assert_eq!(SplTokenAccount::unpack(&after[2].data).unwrap().amount, 0);
    assert_eq!(
        SplTokenAccount::unpack(&after[3].data).unwrap().amount,
        AMOUNT
    );
    let (replay, logs) = execute(&mut w, settle).await;
    replay.unwrap();
    for program in [portal, spl] {
        assert!(!logs.contains(&format!("Program {program} invoke [2]")));
    }
    assert_eq!(settlement_snapshot(&mut w.context, keys).await, after);
}
