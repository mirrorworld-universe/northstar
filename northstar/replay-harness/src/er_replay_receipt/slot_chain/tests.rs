use {
    super::*,
    crate::er_replay_receipt::ordered::encode,
    solana_leader_schedule::SlotLeader,
    solana_pubkey::Pubkey,
    solana_runtime::genesis_utils::{GenesisConfigInfo, create_genesis_config},
    solana_signer::Signer,
    solana_system_interface::instruction::transfer,
    solana_transaction::{Transaction, versioned::VersionedTransaction},
};

fn finish(bank: &Bank, tick: u8) {
    for _ in bank.tick_height()..bank.max_tick_height() {
        bank.register_tick_for_test(&Hash::new_from_array([tick; 32]));
    }
    bank.freeze();
}

fn anchor(genesis: &GenesisConfigInfo) -> Arc<Bank> {
    let root = Bank::new_for_tests(&genesis.genesis_config);
    finish(&root, 10);
    let bank = Bank::new_from_parent_ephemeral_isolated(Arc::new(root), SlotLeader::default(), 1);
    finish(&bank, 11);
    Arc::new(bank)
}

fn child(parent: &Arc<Bank>, slot: u64) -> Bank {
    Bank::new_from_parent_ephemeral(Arc::clone(parent), SlotLeader::default(), slot)
}

#[test]
fn independent_bank_replay_reproduces_the_enrolled_pin_and_v2_child() {
    let genesis = create_genesis_config(1_000_000_000);
    let start_id = [77; 32];
    let pin = SlotChain::measure(&anchor(&genesis)).unwrap();
    let epoch = RecentHashEpoch::new(start_id, pin.slot, 2).unwrap();
    let payer = &genesis.mint_keypair;
    let recipient = Pubkey::new_unique();
    let tx = Transaction::new_signed_with_payer(
        &[transfer(&payer.pubkey(), &recipient, 2_000_000)],
        Some(&payer.pubkey()),
        &[payer],
        epoch.completed_slot_hash(1).unwrap(),
    );
    let wire = wincode::serialize(&VersionedTransaction::from(tx)).unwrap();
    let packets = encode(3, &[&wire]).unwrap();
    let replay = || {
        let base = anchor(&genesis);
        let chain = SlotChain::enroll(pin, start_id, 2, &base).unwrap();
        let skipped = child(&base, 3);
        assert!(chain.replay_empty(skipped).is_err());
        let empty = chain.replay_empty(child(&base, 2)).unwrap();
        assert!(empty.batch.is_none());
        let mut old = packets.clone();
        old[..8].copy_from_slice(b"NSERPK01");
        assert!(empty.next.replay(child(&empty.frozen, 3), &old).is_err());
        let executed = empty
            .next
            .replay(child(&empty.frozen, 3), &packets)
            .unwrap();
        assert_eq!(executed.frozen.get_balance(&recipient), 2_000_000);
        assert_eq!(base.get_balance(&recipient), 0);
        let receipt = executed.batch.unwrap();
        assert_eq!(receipt.transactions.len(), 1);
        (empty.next.pin(), executed.next.pin(), receipt.receipts_root)
    };
    assert_eq!(replay(), replay());
    for wrong in [
        BankPin {
            bank_hash: [9; 32],
            ..pin
        },
        BankPin {
            history_hash: [9; 32],
            ..pin
        },
    ] {
        assert!(SlotChain::enroll(wrong, start_id, 2, &anchor(&genesis)).is_err());
    }
}
