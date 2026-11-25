use {
    super::{Fixture, ReplayBootstrap},
    crate::{
        certified_replay::shadow::{FrozenSlotCapture, ShadowAnchor, ShadowCursor, ShadowReplica},
        slot_advancer::SlotAdvancer,
        EphemeralRollupSettings,
    },
    northstar_replay_harness::er_replay_receipt::slot_chain::{BankPin, SlotChain},
    solana_account::AccountSharedData,
    solana_commitment_config::CommitmentConfig,
    solana_hash::Hash,
    solana_keypair::Keypair,
    solana_leader_schedule::SlotLeader,
    solana_pubkey::Pubkey,
    solana_rpc::er_history::ErHistoryStore,
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_system_interface::instruction::transfer,
    solana_transaction::Transaction,
    solana_transaction_status::{TransactionStatusMeta, VersionedTransactionWithStatusMeta},
    std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
        path::{Path, PathBuf},
        sync::Arc,
    },
};

const AGE: u32 = 150;

fn private_directory() -> tempfile::TempDir {
    let parent = tempfile::tempdir().unwrap();
    fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
    parent
}

fn finish(bank: &Bank, last: u8) {
    for tick in bank.tick_height()..bank.max_tick_height() {
        let hash = if tick.checked_add(1) == Some(bank.max_tick_height()) {
            Hash::new_from_array([last; 32])
        } else {
            Hash::new_unique()
        };
        bank.register_tick_for_test(&hash);
    }
    bank.freeze();
}

struct Rig {
    f: Fixture,
    bootstrap: ReplayBootstrap,
    payer: Keypair,
}
impl Rig {
    fn new() -> Self {
        let f = Fixture::at(1);
        let bootstrap = f.open(&f.bank(|_| {}), f.pins.clone()).unwrap();
        Self {
            f,
            bootstrap,
            payer: Keypair::new(),
        }
    }
    fn base(&self) -> Bank {
        let root = Bank::new_for_tests(&self.f.genesis);
        root.store_account(
            &self.payer.pubkey(),
            &AccountSharedData::new(20_000_000, 0, &system_program::id()),
        );
        finish(&root, 20);
        let mut bank =
            Bank::new_from_parent_ephemeral_isolated(Arc::new(root), SlotLeader::default(), 1);
        bank.configure_er(&EphemeralRollupSettings::zero_fee_structure(), AGE as usize);
        finish(&bank, 21);
        bank
    }
    fn anchor(&self, pin: BankPin) -> ShadowAnchor {
        self.bootstrap.shadow_anchor(self.base(), pin, AGE).unwrap()
    }
}

fn source_slot(
    parent: &Arc<Bank>,
    last: u8,
    tx: Option<Transaction>,
) -> (Arc<Bank>, FrozenSlotCapture) {
    let slot = parent.slot().checked_add(1).unwrap();
    let mut bank = Bank::new_from_parent_ephemeral(Arc::clone(parent), SlotLeader::default(), slot);
    bank.configure_er(parent.fee_structure(), AGE as usize);
    SlotAdvancer::carry_forward_modified_accounts(parent, &bank);
    let forks = BankForks::new_rw_arc_ephemeral(bank);
    let bank = forks.read().unwrap().working_bank();
    let history = ErHistoryStore::default();
    if let Some(tx) = tx {
        bank.process_transaction(&tx).unwrap();
        assert_eq!(
            history.record_transaction(
                &bank,
                VersionedTransactionWithStatusMeta {
                    transaction: tx.into(),
                    meta: TransactionStatusMeta::default()
                }
            ),
            Some(0)
        );
    }
    finish(&bank, last);
    history.finalize_slot(&bank);
    let block = history.get_block(slot, CommitmentConfig::finalized());
    if block.is_some() {
        assert!(FrozenSlotCapture::from_bank(&bank, None).is_err());
    }
    let observed = FrozenSlotCapture::from_bank(&bank, block.as_ref()).unwrap();
    (bank, observed)
}
fn payment(bank: &Bank, sender: &Keypair, recipient: &Pubkey, lamports: u64) -> Transaction {
    Transaction::new_signed_with_payer(
        &[transfer(&sender.pubkey(), recipient, lamports)],
        Some(&sender.pubkey()),
        &[sender],
        bank.last_blockhash(),
    )
}
fn image(dir: &Path) -> PathBuf {
    dir.join("replay-v2")
}

#[test]
fn one_replica_replays_frozen_slots_recovers_and_continues() {
    let rig = Rig::new();
    let base = Arc::new(rig.base());
    let pin = SlotChain::measure(&base).unwrap();
    let parent = private_directory();
    let dir = parent.path().join("shadow");
    let mut replica = ShadowReplica::create(&dir, rig.anchor(pin)).unwrap();
    let start = replica.cursor();
    assert!(ShadowReplica::resume(&dir, rig.anchor(pin), start).is_err());
    assert!(ShadowReplica::create(&dir, rig.anchor(pin)).is_err());
    let (source, empty) = source_slot(&base, 22, None);
    assert!(empty.input().packets.is_none());
    let first = replica.observe(empty.input()).unwrap();
    let owner = Keypair::new();
    let tx = payment(&source, &rig.payer, &owner.pubkey(), 4_000_000);
    let (source, credit) = source_slot(&source, 23, Some(tx));
    assert!(credit.input().packets.is_some());
    let second = replica.observe(credit.input()).unwrap();
    drop(replica);
    let bytes = fs::read(image(&dir)).unwrap();
    let mut recovered = ShadowReplica::resume(&dir, rig.anchor(pin), first).unwrap();
    assert_eq!(recovered.cursor(), second);
    assert_eq!(fs::read(image(&dir)).unwrap(), bytes);
    let recipient = Pubkey::new_unique();
    let tx = payment(&source, &owner, &recipient, 2_000_000);
    let (source, spend) = source_slot(&source, 24, Some(tx));
    assert_eq!(source.get_balance(&recipient), 2_000_000);
    let final_cursor = recovered.observe(spend.input()).unwrap();
    assert_eq!(final_cursor.slot, 4);
    drop(recovered);
    assert_eq!(
        ShadowReplica::resume(&dir, rig.anchor(pin), final_cursor)
            .unwrap()
            .cursor(),
        final_cursor
    );
}

#[test]
fn capture_requires_signatures_committed_in_this_frozen_slot() {
    let rig = Rig::new();
    let anchor = Arc::new(rig.base());
    let prior = payment(&anchor, &rig.payer, &Pubkey::new_unique(), 2_000_000);
    let (parent, _) = source_slot(&anchor, 22, Some(prior.clone()));
    let current = payment(&parent, &rig.payer, &Pubkey::new_unique(), 2_000_000);
    let (bank, _) = source_slot(&parent, 23, Some(current));
    let stale = ErHistoryStore::default();
    stale
        .record_transaction(
            &bank,
            VersionedTransactionWithStatusMeta {
                transaction: prior.into(),
                meta: TransactionStatusMeta::default(),
            },
        )
        .unwrap();
    stale.finalize_slot(&bank);
    let block = stale
        .get_block(bank.slot(), CommitmentConfig::finalized())
        .unwrap();
    assert!(FrozenSlotCapture::from_bank(&bank, Some(&block)).is_err());
}

#[test]
fn gap_or_pin_mismatch_stops_replay_without_moving_the_durable_cursor() {
    let rig = Rig::new();
    let base = Arc::new(rig.base());
    let pin = SlotChain::measure(&base).unwrap();
    let (slot2, empty) = source_slot(&base, 22, None);
    let (_, later) = source_slot(&slot2, 23, None);
    let parent = private_directory();
    let dir = parent.path().join("shadow");
    let mut observer = ShadowReplica::create(&dir, rig.anchor(pin)).unwrap();
    let floor = observer.cursor();
    let before = fs::read(image(&dir)).unwrap();
    assert!(observer.observe(later.input()).is_err());
    assert!(observer.observe(empty.input()).is_err());
    assert_eq!(observer.cursor(), floor);
    drop(observer);
    let mut observer = ShadowReplica::resume(&dir, rig.anchor(pin), floor).unwrap();
    let mut bad = empty.input();
    bad.expected.history_hash[0] ^= 1;
    assert!(observer.observe(bad).is_err());
    assert_eq!(fs::read(image(&dir)).unwrap(), before);
    drop(observer);
    let mut observer = ShadowReplica::resume(&dir, rig.anchor(pin), floor).unwrap();
    let tip = observer.observe(empty.input()).unwrap();
    drop(observer);
    fs::write(image(&dir), &before).unwrap();
    assert!(ShadowReplica::resume(&dir, rig.anchor(pin), tip).is_err());
    let wrong = ShadowCursor {
        digest: [0; 32],
        ..floor
    };
    assert!(ShadowReplica::resume(&dir, rig.anchor(pin), wrong).is_err());
    assert_eq!(fs::read(image(&dir)).unwrap(), before);
}

#[test]
fn restart_requires_the_original_private_store_and_reproduced_anchor() {
    let rig = Rig::new();
    let pin = SlotChain::measure(&rig.base()).unwrap();
    assert!(rig
        .bootstrap
        .shadow_anchor(rig.base(), pin, AGE + 1)
        .is_err());
    assert!(rig
        .bootstrap
        .shadow_anchor(
            rig.base(),
            BankPin {
                bank_hash: [99; 32],
                ..pin
            },
            AGE
        )
        .is_err());
    let parent = private_directory();
    let dir = parent.path().join("shadow");
    let observer = ShadowReplica::create(&dir, rig.anchor(pin)).unwrap();
    let floor = observer.cursor();
    drop(observer);
    let missing = parent.path().join("missing");
    assert!(ShadowReplica::resume(&missing, rig.anchor(pin), floor).is_err());
    assert!(!missing.exists());
    let file = image(&dir);
    let saved = fs::read(&file).unwrap();
    let mut damaged = saved.clone();
    damaged[0] ^= 1;
    fs::write(&file, &damaged).unwrap();
    assert!(ShadowReplica::resume(&dir, rig.anchor(pin), floor).is_err());
    assert_eq!(fs::read(&file).unwrap(), damaged);
    fs::write(&file, &saved).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(ShadowReplica::resume(&dir, rig.anchor(pin), floor).is_err());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let other = dir.join("original");
    fs::rename(&file, &other).unwrap();
    symlink(&other, &file).unwrap();
    assert!(ShadowReplica::resume(&dir, rig.anchor(pin), floor).is_err());
    assert_eq!(fs::read(other).unwrap(), saved);
}
