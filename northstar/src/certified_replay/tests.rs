use {
    super::*,
    crate::{checkpoint::CheckpointCommitmentV1, checkpoint_v2::checkpoint_binding_hash_v2},
    northstar_portal::{Checkpoint, CheckpointCursor, Session, SessionV2, SettlementStatus},
    solana_account::{Account, AccountSharedData},
    solana_leader_schedule::SlotLeader,
    solana_runtime::genesis_utils::create_genesis_config,
    std::sync::{Arc, RwLock},
};

const ROOT_SLOT: u64 = 80;
const ER_SLOT: u64 = (1 << 40) + 17;

struct Fixture {
    genesis: GenesisConfig,
    pins: BootstrapPins,
    state: CommittedStateV2,
    accounts: Vec<(Pubkey, AccountSharedData)>,
}

fn account(owner: Pubkey, data: Vec<u8>, executable: bool) -> AccountSharedData {
    Account {
        lamports: 10_000_000,
        data,
        owner,
        executable,
        rent_epoch: u64::MAX,
    }
    .into()
}

impl Fixture {
    fn new() -> Self {
        let genesis = create_genesis_config(1_000_000_000).genesis_config;
        let portal = Pubkey::new_unique();
        let (session, bump) = northstar_portal::find_session_pda(&portal);
        let frame = SessionV2 {
            discriminator: SessionV2::DISCRIMINATOR,
            base: Session {
                discriminator: Session::DISCRIMINATOR,
                grid_id: 0,
                ttl_slots: 150,
                fee_cap: 0,
                created_at: 1,
                nonce: 7,
                authority: Pubkey::new_unique(),
                validator: Pubkey::new_unique(),
                settlement_interval_slots: 10,
                last_settled_l1_slot: 70,
                last_settled_er_slot: ER_SLOT,
                settlement_status: SettlementStatus::Idle,
                settlement_er_slot: 0,
                settlement_checksum: [0; 32],
                settlement_accumulator: [0; 32],
                settlement_started_l1_slot: 0,
                bump,
            },
            l1_genesis: genesis.hash().to_bytes(),
            profile: [3; 32],
            ingress_count: 0,
            ingress_root: [0; 32],
            consumed_ingress_count: 0,
            consumed_ingress_root: [0; 32],
        };
        let identity = frame.identity(&portal, &session);
        let state = CommittedStateV2::new(identity).unwrap();
        let opening = state.opening().unwrap();
        let state_root = opening.commitment(hash_parts).unwrap();
        let (address, bump) = northstar_portal::find_checkpoint_pda(&portal, &session, ER_SLOT);
        let cp = Checkpoint {
            discriminator: Checkpoint::DISCRIMINATOR,
            session,
            er_slot: ER_SLOT,
            step_count: 1,
            previous_state_root: state_root,
            new_state_root: state_root,
            trace_root: [3; 32],
            tx_effect_root: [4; 32],
            readonly_l1_root: [5; 32],
            da_commitment: [6; 32],
            effect_commitment: [7; 32],
            proposer: frame.base.validator,
            proposed_at_l1_slot: 50,
            challenge_deadline_l1_slot: 60,
            status: CheckpointStatus::Settled,
            bond_lamports: 0,
            bond_status: CheckpointBondStatus::Released,
            challenger: Pubkey::default(),
            challenged_at_l1_slot: 0,
            challenge_resolved: false,
            bump,
        };
        let (binding_key, bump) =
            northstar_portal::find_checkpoint_binding_v2_pda(&portal, &address);
        let binding = CheckpointBindingV2 {
            discriminator: CheckpointBindingV2::DISCRIMINATOR,
            checkpoint: address.to_bytes(),
            pre: opening,
            post: opening,
            prefix_record: [0; 32],
            input_context_root: cp.readonly_l1_root,
            bump,
        };
        let (cursor_key, bump) = northstar_portal::find_checkpoint_cursor_pda(&portal, &session);
        let cursor = CheckpointCursor {
            discriminator: CheckpointCursor::DISCRIMINATOR,
            session,
            latest_finalized_checkpoint: address,
            latest_finalized_er_slot: ER_SLOT,
            latest_finalized_state_root: state_root,
            active_checkpoint: Pubkey::default(),
            active_er_slot: 0,
            bump,
        };
        let mut accounts = vec![
            (
                session,
                account(portal, borsh::to_vec(&frame).unwrap(), false),
            ),
            (address, account(portal, borsh::to_vec(&cp).unwrap(), false)),
            (
                binding_key,
                account(portal, borsh::to_vec(&binding).unwrap(), false),
            ),
            (
                cursor_key,
                account(portal, borsh::to_vec(&cursor).unwrap(), false),
            ),
        ];
        let header = CheckpointCommitmentV1 {
            version: 2,
            session,
            er_slot: ER_SLOT,
            step_count: 1,
            previous_state_root: state_root,
            new_state_root: state_root,
            trace_root: cp.trace_root,
            transaction_effect_root: cp.tx_effect_root,
            readonly_l1_root: cp.readonly_l1_root,
            da_commitment: cp.da_commitment,
            effect_commitment: cp.effect_commitment,
        };
        let checkpoint = L1CheckpointHeadV2 {
            checkpoint: address.to_bytes(),
            er_slot: ER_SLOT,
            state_root,
            checkpoint_hash: checkpoint_binding_hash_v2(&header, &opening, &opening).unwrap(),
        };
        let loader = bpf_loader_upgradeable::id();
        let [portal, coordinator, verifier, consumer] = [
            portal,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        ]
        .map(|key| {
            let data_key = Pubkey::find_program_address(&[key.as_ref()], &loader).0;
            let mut program = 2u32.to_le_bytes().to_vec();
            program.extend_from_slice(data_key.as_ref());
            let mut data = vec![0u8; 45];
            data[..4].copy_from_slice(&3u32.to_le_bytes());
            data.extend_from_slice(key.as_ref());
            let digest = hash_parts(&[&data]);
            accounts.push((key, account(loader, program, true)));
            accounts.push((data_key, account(loader, data, false)));
            ProgramImage {
                address: key,
                programdata_sha256: digest,
            }
        });
        let pins = BootstrapPins {
            identity,
            checkpoint,
            finalized_l1_slot_floor: ROOT_SLOT,
            minimum_challenge_slots: 10,
            portal,
            coordinator,
            verifier,
            consumer,
        };
        Self {
            genesis,
            pins,
            state,
            accounts,
        }
    }

    fn bank(&self, change: impl FnOnce(&Bank)) -> Arc<RwLock<BankForks>> {
        let root = Bank::new_from_parent(
            Arc::new(Bank::new_for_tests(&self.genesis)),
            SlotLeader::default(),
            ROOT_SLOT,
        );
        for (address, value) in &self.accounts {
            root.store_account(address, value);
        }
        change(&root);
        root.freeze();
        BankForks::new_rw_arc(root)
    }
    fn open(&self, forks: &RwLock<BankForks>, pins: BootstrapPins) -> io::Result<ReplayBootstrap> {
        ReplayBootstrap::from_root(
            &self.genesis,
            &forks.read().unwrap(),
            pins,
            self.state.clone(),
        )
    }
}

#[test]
fn loads_only_the_pinned_root_not_a_newer_working_bank() {
    let f = Fixture::new();
    let forks = f.bank(|_| {});
    let parent = forks.read().unwrap().root_bank();
    let child = Bank::new_from_parent(parent, SlotLeader::default(), ROOT_SLOT + 1);
    child.store_account(
        &Pubkey::from(f.pins.identity.session),
        &AccountSharedData::default(),
    );
    forks.write().unwrap().insert(child);
    assert_eq!(forks.read().unwrap().working_bank().slot(), ROOT_SLOT + 1);
    let loaded = f.open(&forks, f.pins.clone()).unwrap();
    assert_eq!(loaded.observed_l1_slot(), ROOT_SLOT);
    assert_eq!(loaded.pins(), &f.pins);
    assert_eq!(loaded.state(), &f.state);
    let mut pins = f.pins.clone();
    pins.finalized_l1_slot_floor = ROOT_SLOT + 1;
    assert!(f.open(&forks, pins).is_err());
}

#[test]
fn immutable_bootstrap_bindings_must_match_before_state_is_returned() {
    let f = Fixture::new();
    let forks = f.bank(|_| {});
    for change in [
        (|p: &mut BootstrapPins| p.identity.nonce += 1) as fn(&mut BootstrapPins),
        |p| p.identity.l1_genesis[0] ^= 1,
        |p| p.checkpoint.checkpoint_hash[0] ^= 1,
        |p| p.checkpoint.state_root[0] ^= 1,
        |p| p.minimum_challenge_slots = 11,
        |p| p.coordinator = p.portal,
        |p| p.portal.programdata_sha256[0] ^= 1,
        |p| p.coordinator.programdata_sha256[0] ^= 1,
        |p| p.verifier.programdata_sha256[0] ^= 1,
        |p| p.consumer.programdata_sha256[0] ^= 1,
    ] {
        let mut pins = f.pins.clone();
        change(&mut pins);
        assert!(f.open(&forks, pins).is_err());
    }
}

#[test]
fn committed_checkpoint_and_mismatched_prefix_are_not_bootstrap_evidence() {
    let f = Fixture::new();
    let key = Pubkey::from(f.pins.checkpoint.checkpoint);
    let portal = f.pins.portal.address;
    let committed = f.bank(|bank| {
        let mut cp = Checkpoint::try_from_slice(bank.get_account(&key).unwrap().data()).unwrap();
        cp.status = CheckpointStatus::Committed;
        bank.store_account(&key, &account(portal, borsh::to_vec(&cp).unwrap(), false));
    });
    assert!(f.open(&committed, f.pins.clone()).is_err());
    let binding_key = northstar_portal::find_checkpoint_binding_v2_pda(&portal, &key).0;
    let different_prefix = f.bank(|bank| {
        let mut binding =
            CheckpointBindingV2::try_from_slice(bank.get_account(&binding_key).unwrap().data())
                .unwrap();
        binding.prefix_record = [1; 32];
        bank.store_account(
            &binding_key,
            &account(portal, borsh::to_vec(&binding).unwrap(), false),
        );
    });
    assert!(f.open(&different_prefix, f.pins.clone()).is_err());
}
