use {super::*, crate::event::Source};

fn event(program: Pubkey, slot: u64, tx_index: usize, log_index: usize) -> ProgramEvent {
    ProgramEvent {
        program_id: program.to_string(),
        slot,
        block_time: None,
        signature: "signature".into(),
        tx_index,
        log_index,
        source: Source::Log,
        commitment: Commitment::Confirmed,
        data: "AAAAAAAAAAA=".into(),
    }
}

fn publish(broker: &Broker, program: Pubkey, slot: u64) {
    broker.transaction(slot, slot, 0, vec![event(program, slot, 0, 0)]);
    broker.metadata(slot, slot, slot - 1, 1, Some(123));
    broker.status(slot, slot, Some(slot - 1), &SlotStatus::Confirmed);
}

fn next_event(subscription: &mut Subscription) -> ProgramEvent {
    match subscription.live.try_recv().unwrap().as_ref() {
        StreamMessage::Event(event) => event.clone(),
        message => panic!("expected event, got {message:?}"),
    }
}

#[test]
fn confirmation_waits_for_metadata_and_every_transaction() {
    let broker = Broker::new(Config::default());
    let key = Pubkey::new_unique();
    let mut subscription = broker.subscribe(key, Resume::default()).unwrap();
    broker.status(10, 10, Some(9), &SlotStatus::Confirmed);
    broker.metadata(10, 10, 9, 3, Some(123));
    broker.transaction(10, 10, 2, vec![event(key, 10, 2, 0)]);
    broker.transaction(10, 10, 0, vec![event(key, 10, 0, 0)]);
    assert!(subscription.live.try_recv().is_err());
    // An uninterested transaction still participates in the completion barrier.
    broker.transaction(10, 10, 1, vec![]);
    assert_eq!(next_event(&mut subscription).tx_index, 0);
    let second = next_event(&mut subscription);
    assert_eq!(second.tx_index, 2);
    assert_eq!(second.block_time, Some(123));
    broker.transaction(10, 10, 1, vec![]);
    assert!(subscription.live.try_recv().is_err());
}

#[test]
fn descendant_waits_for_parent_callbacks() {
    let broker = Broker::new(Config::default());
    let key = Pubkey::new_unique();
    let mut subscription = broker.subscribe(key, Resume::default()).unwrap();
    broker.status(10, 10, Some(9), &SlotStatus::CreatedBank);
    publish(&broker, key, 11);
    assert!(subscription.live.try_recv().is_err());
    publish(&broker, key, 10);
    assert_eq!(next_event(&mut subscription).slot, 10);
    assert_eq!(next_event(&mut subscription).slot, 11);
}

#[test]
fn bank_identity_selects_only_confirmed_fork() {
    let broker = Broker::new(Config::default());
    let key = Pubkey::new_unique();
    let mut subscription = broker.subscribe(key, Resume::default()).unwrap();
    broker.transaction(10, 1, 0, vec![event(key, 10, 0, 0)]);
    broker.metadata(10, 1, 9, 1, None);
    broker.transaction(10, 2, 0, vec![event(key, 10, 0, 1)]);
    broker.metadata(10, 2, 9, 1, None);
    broker.status(10, 2, Some(9), &SlotStatus::Confirmed);
    assert_eq!(next_event(&mut subscription).log_index, 1);
    assert!(subscription.live.try_recv().is_err());
}

#[test]
fn processed_and_dead_slots_are_never_published() {
    let broker = Broker::new(Config::default());
    let key = Pubkey::new_unique();
    let mut subscription = broker.subscribe(key, Resume::default()).unwrap();
    broker.transaction(10, 10, 0, vec![event(key, 10, 0, 0)]);
    broker.metadata(10, 10, 9, 1, None);
    broker.status(10, 10, Some(9), &SlotStatus::Processed);
    assert!(subscription.live.try_recv().is_err());
    broker.status(10, 0, Some(9), &SlotStatus::Dead("not retained".into()));
    broker.status(10, 10, Some(9), &SlotStatus::Confirmed);
    assert!(subscription.live.try_recv().is_err());
    assert!(broker.state.lock().unwrap().pending.is_empty());
}

#[test]
fn finalized_configuration_waits_for_root() {
    let broker = Broker::new(Config {
        commitment: Commitment::Finalized,
        ..Config::default()
    });
    let key = Pubkey::new_unique();
    let mut subscription = broker.subscribe(key, Resume::default()).unwrap();
    publish(&broker, key, 10);
    assert!(subscription.live.try_recv().is_err());
    broker.status(10, 10, Some(9), &SlotStatus::Rooted);
    assert_eq!(
        next_event(&mut subscription).commitment,
        Commitment::Finalized
    );
}

#[test]
fn resume_is_exclusive_and_from_slot_is_inclusive() {
    let broker = Broker::new(Config::default());
    let key = Pubkey::new_unique();
    let subscription = broker.subscribe(key, Resume::default()).unwrap();
    publish(&broker, key, 10);
    publish(&broker, key, 11);
    drop(subscription);
    let resumed = broker
        .subscribe(
            key,
            Resume {
                after: Some(EventId {
                    slot: 10,
                    tx_index: 0,
                    log_index: 0,
                }),
                from_slot: None,
            },
        )
        .unwrap();
    assert_eq!(resumed.replay.len(), 1);
    assert!(matches!(resumed.replay[0].as_ref(), StreamMessage::Event(event) if event.slot == 11));
    let inclusive = broker
        .subscribe(
            key,
            Resume {
                from_slot: Some(10),
                after: None,
            },
        )
        .unwrap();
    assert_eq!(inclusive.replay.len(), 2);
}

#[test]
fn missing_replay_and_expired_buffers_require_resync() {
    let broker = Broker::new(Config {
        max_events_per_program: 1,
        ..Config::default()
    });
    let key = Pubkey::new_unique();
    let first = broker
        .subscribe(
            key,
            Resume {
                from_slot: Some(1),
                after: None,
            },
        )
        .unwrap();
    assert!(matches!(
        first.replay[0].as_ref(),
        StreamMessage::Resync { .. }
    ));
    publish(&broker, key, 10);
    publish(&broker, key, 11);
    let old = broker
        .subscribe(
            key,
            Resume {
                from_slot: Some(10),
                after: None,
            },
        )
        .unwrap();
    assert!(
        matches!(old.replay[0].as_ref(), StreamMessage::Resync { last_available_id: Some(id), .. } if id == "11:0:0")
    );
    // The exact last evicted event remains a safe exclusive cursor.
    let exact = broker
        .subscribe(
            key,
            Resume {
                after: Some(EventId {
                    slot: 10,
                    tx_index: 0,
                    log_index: 0,
                }),
                from_slot: None,
            },
        )
        .unwrap();
    assert_eq!(exact.replay.len(), 1);
}

#[test]
fn watch_grace_and_connection_limits_are_bounded() {
    let broker = Broker::new(Config {
        max_connections: 1,
        max_programs: 1,
        ..Config::default()
    });
    let key = Pubkey::new_unique();
    let subscription = broker.subscribe(key, Resume::default()).unwrap();
    assert!(broker.subscribe(key, Resume::default()).is_err());
    drop(subscription);
    assert!(broker.watched().contains(&key));
    assert!(
        broker
            .subscribe(Pubkey::new_unique(), Resume::default())
            .is_err()
    );
    let mut state = broker.state.lock().unwrap();
    broker.cleanup_locked(&mut state, Instant::now() + Duration::from_secs(31));
    drop(state);
    assert!(broker.watched().is_empty());
}

#[test]
fn ttl_and_global_byte_limits_evict_with_resync() {
    let broker = Broker::new(Config {
        max_replay_bytes: 1,
        ..Config::default()
    });
    let key = Pubkey::new_unique();
    let _subscription = broker.subscribe(key, Resume::default()).unwrap();
    publish(&broker, key, 10);
    assert_eq!(broker.state.lock().unwrap().replay_bytes, 0);
    let resumed = broker
        .subscribe(
            key,
            Resume {
                from_slot: Some(10),
                after: None,
            },
        )
        .unwrap();
    assert!(matches!(
        resumed.replay[0].as_ref(),
        StreamMessage::Resync { .. }
    ));

    let broker = Broker::new(Config::default());
    let _subscription = broker.subscribe(key, Resume::default()).unwrap();
    publish(&broker, key, 10);
    let mut state = broker.state.lock().unwrap();
    broker.cleanup_locked(&mut state, Instant::now() + Duration::from_secs(601));
    assert_eq!(state.replay_bytes, 0);
    assert!(state.programs[&key].replay.is_empty());
}

#[test]
fn pending_overflow_is_explicit_and_does_not_reorder_following_events() {
    let broker = Broker::new(Config {
        max_pending_transactions: 1,
        ..Config::default()
    });
    let key = Pubkey::new_unique();
    let mut subscription = broker.subscribe(key, Resume::default()).unwrap();
    broker.transaction(10, 10, 0, vec![event(key, 10, 0, 0)]);
    broker.transaction(10, 10, 1, vec![]);
    assert!(matches!(
        subscription.live.try_recv().unwrap().as_ref(),
        StreamMessage::Resync {
            reason: "pending_limit",
            ..
        }
    ));
    publish(&broker, key, 11);
    assert_eq!(next_event(&mut subscription).slot, 11);
}

#[test]
fn subscription_during_extraction_does_not_claim_complete_replay() {
    let broker = Broker::new(Config::default());
    let key = Pubkey::new_unique();
    let captured_watch = broker.watched();
    let _subscription = broker.subscribe(key, Resume::default()).unwrap();
    broker.transaction_with_watch(10, 10, 0, vec![], &captured_watch);
    broker.metadata(10, 10, 9, 1, None);
    broker.status(10, 10, Some(9), &SlotStatus::Confirmed);
    let resumed = broker
        .subscribe(
            key,
            Resume {
                from_slot: Some(10),
                after: None,
            },
        )
        .unwrap();
    assert!(
        matches!(
            resumed.replay.front().map(Arc::as_ref),
            Some(StreamMessage::Resync { .. })
        ),
        "a watch added after extraction began has incomplete coverage for that slot"
    );
}

#[test]
fn live_receiver_is_registered_before_replay_handoff() {
    let broker = Broker::new(Config::default());
    let key = Pubkey::new_unique();
    let _first = broker.subscribe(key, Resume::default()).unwrap();
    publish(&broker, key, 10);
    let mut resumed = broker
        .subscribe(
            key,
            Resume {
                from_slot: Some(10),
                after: None,
            },
        )
        .unwrap();
    publish(&broker, key, 11);
    assert_eq!(resumed.replay.len(), 1);
    assert_eq!(next_event(&mut resumed).slot, 11);
}
