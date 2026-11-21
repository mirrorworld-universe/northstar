use {
    super::*,
    northstar_portal::{DepositReceipt, DepositReceiptV2, SessionV2, SettleDepositReceipt},
};

async fn snapshot(context: &mut ProgramTestContext, keys: [Pubkey; 3]) -> [Account; 3] {
    [
        get(context, keys[0]).await,
        get(context, keys[1]).await,
        get(context, keys[2]).await,
    ]
}

async fn check_receipt(scoped: bool) {
    let fixture = Fixture::new();
    let session = fixture.session;
    let payer = fixture.payer;
    let mut context = fixture.program_test.start_with_context().await;
    let mut session_account = get(&mut context, session).await;
    let session_base = Session::try_from_slice(&session_account.data).unwrap();
    let source = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    for address in [source, recipient] {
        context.set_account(
            &address,
            &account(BALANCE, vec![], system_program::id()).into(),
        );
    }
    let binding = scoped.then(|| {
        let frame = SessionV2 {
            discriminator: SessionV2::DISCRIMINATOR,
            base: Session {
                nonce: 1,
                ..session_base
            },
            l1_genesis: [1; 32],
            profile: [2; 32],
            ingress_count: 0,
            ingress_root: [0; 32],
            consumed_ingress_count: 0,
            consumed_ingress_root: [0; 32],
        };
        session_account.data = borsh::to_vec(&frame).unwrap();
        context.set_account(&session, &session_account.clone().into());
        frame
            .identity(&PORTAL, &session)
            .commitment(|parts| hashv(parts).to_bytes())
            .unwrap()
    });
    let (receipt, bump) = match binding {
        Some(key) => northstar_portal::find_deposit_receipt_v2_pda(&PORTAL, &key, &source),
        None => northstar_portal::find_deposit_receipt_pda(&PORTAL, &session, &source),
    };
    let base = DepositReceipt {
        discriminator: DepositReceipt::DISCRIMINATOR,
        session,
        recipient: source,
        balance: 10,
        withdrawn: 1,
        bump,
    };
    let encode = |base: DepositReceipt| match binding {
        Some(key) => borsh::to_vec(&DepositReceiptV2 {
            discriminator: DepositReceiptV2::DISCRIMINATOR,
            base,
            session_context: key,
            total_deposited: 11,
        })
        .unwrap(),
        None => borsh::to_vec(&base).unwrap(),
    };
    let data = encode(base);
    let rent = context
        .banks_client
        .get_rent()
        .await
        .unwrap()
        .minimum_balance(data.len());
    context.set_account(&receipt, &account(rent + 12, data, PORTAL).into());
    let keys = [session, receipt, recipient];
    let before = snapshot(&mut context, keys).await;
    let ix = |balance, withdrawn, payout_lamports, to| Instruction {
        program_id: PORTAL,
        accounts: vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(session, false),
            AccountMeta::new(receipt, false),
            AccountMeta::new_readonly(source, false),
            AccountMeta::new(to, false),
        ],
        data: borsh::to_vec(&PortalInstruction::SettleDepositReceipt(
            SettleDepositReceipt {
                er_slot: ER_SLOT,
                checksum: [42; 32],
                balance,
                withdrawn,
                payout_lamports,
                l1_recipient: to,
            },
        ))
        .unwrap(),
    };
    let mut rejects = vec![
        (ix(6, 5, 3, recipient), PortalError::InvalidAccountData),
        (ix(9, 5, 4, recipient), PortalError::InsufficientFees),
        (ix(6, 5, 4, receipt), PortalError::InvalidAccountData),
    ];
    if scoped {
        rejects.push((
            ix(7, 5, 4, recipient),
            PortalError::DepositReceiptStateInvalid,
        ));
    }
    for (instruction, expected) in rejects {
        assert_portal_error(
            send(&mut context, &payer, &[instruction])
                .await
                .unwrap_err(),
            expected,
        );
        assert_eq!(snapshot(&mut context, keys).await, before);
    }
    send(&mut context, &payer, &[ix(6, 5, 4, recipient)])
        .await
        .unwrap();
    let after = snapshot(&mut context, keys).await;
    assert_eq!(after[1].lamports, rent + 8);
    assert_eq!(after[2].lamports, before[2].lamports + 4);
    assert_eq!(
        after[1].data,
        encode(DepositReceipt {
            balance: 6,
            withdrawn: 5,
            ..base
        })
    );
    let new_accumulator = hashv(&[
        &initial_checksum(),
        b"receipt",
        source.as_ref(),
        recipient.as_ref(),
        &6u64.to_le_bytes(),
        &5u64.to_le_bytes(),
        &4u64.to_le_bytes(),
    ])
    .to_bytes();
    if scoped {
        let mut expected = SessionV2::try_from_slice(&before[0].data).unwrap();
        expected.base.settlement_accumulator = new_accumulator;
        assert_eq!(after[0].data, borsh::to_vec(&expected).unwrap());
    } else {
        assert_eq!(
            after[0].data,
            borsh::to_vec(&Session {
                settlement_accumulator: new_accumulator,
                ..session_base
            })
            .unwrap()
        );
    }
    // A new signature runs the identical instruction instead of hitting the bank's transaction cache.
    context.get_new_latest_blockhash().await.unwrap();
    send(&mut context, &payer, &[ix(6, 5, 4, recipient)])
        .await
        .unwrap();
    assert_eq!(snapshot(&mut context, keys).await, after);
    send(&mut context, &payer, &[ix(6, 3, 9, recipient)])
        .await
        .unwrap();
    assert_eq!(snapshot(&mut context, keys).await, after);
}

#[tokio::test]
async fn legacy_receipt_pays_only_delta_and_retry_preserves_state() {
    check_receipt(false).await;
}

#[tokio::test]
async fn scoped_receipt_preserves_frame_cap_and_replay_state() {
    check_receipt(true).await;
}
