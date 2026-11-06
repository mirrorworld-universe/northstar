use {
    crate::{
        config::{Commitment, Config},
        event::{EventId, ProgramEvent, StreamMessage},
    },
    agave_geyser_plugin_interface::geyser_plugin_interface::SlotStatus,
    solana_pubkey::Pubkey,
    std::{
        collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
        sync::{Arc, Mutex, RwLock},
        time::{Duration, Instant},
    },
    tokio::sync::broadcast,
};

type BankKey = (u64, u64);

#[derive(Default)]
struct PendingBank {
    parent: Option<u64>,
    transactions: BTreeSet<usize>,
    events: Vec<ProgramEvent>,
    bytes: usize,
    metadata: Option<(u64, Option<i64>)>,
}

struct Program {
    clients: usize,
    idle_since: Option<Instant>,
    since_slot: Option<u64>,
    evicted: Option<EventId>,
    gap_slot: Option<u64>,
    replay: VecDeque<(Instant, Arc<StreamMessage>)>,
    live: broadcast::Sender<Arc<StreamMessage>>,
}

#[derive(Default)]
struct State {
    programs: HashMap<Pubkey, Program>,
    connections: usize,
    pending: BTreeMap<BankKey, PendingBank>,
    selected: BTreeMap<u64, u64>,
    dead: BTreeSet<u64>,
    highest_seen: Option<u64>,
    published: Option<u64>,
    replay_bytes: usize,
    pending_bytes: usize,
    pending_transactions: usize,
}

pub struct Broker {
    pub config: Config,
    state: Mutex<State>,
    watched: RwLock<Arc<HashSet<Pubkey>>>,
}

#[derive(Clone, Copy, Default)]
pub struct Resume {
    pub after: Option<EventId>,
    pub from_slot: Option<u64>,
}

impl Resume {
    pub fn accepts(self, id: EventId) -> bool {
        self.after.is_none_or(|after| id > after)
            && self.from_slot.is_none_or(|from| id.slot >= from)
    }
}

pub struct Subscription {
    broker: Arc<Broker>,
    pub program: Pubkey,
    pub replay: VecDeque<Arc<StreamMessage>>,
    pub live: broadcast::Receiver<Arc<StreamMessage>>,
    pub resume: Resume,
}

impl Subscription {
    pub fn broker(&self) -> &Broker {
        &self.broker
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut state = self.broker.state.lock().unwrap();
        state.connections = state.connections.saturating_sub(1);
        if let Some(program) = state.programs.get_mut(&self.program) {
            program.clients = program.clients.saturating_sub(1);
            if program.clients == 0 {
                program.idle_since = Some(Instant::now());
            }
        }
    }
}

impl Broker {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config,
            state: Mutex::new(State::default()),
            watched: RwLock::new(Arc::new(HashSet::new())),
        })
    }

    pub fn watched(&self) -> Arc<HashSet<Pubkey>> {
        self.watched.read().unwrap().clone()
    }

    pub fn subscribe(
        self: &Arc<Self>,
        key: Pubkey,
        resume: Resume,
    ) -> Result<Subscription, &'static str> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        self.cleanup_locked(&mut state, now);
        if state.connections >= self.config.max_connections {
            return Err("connection limit reached");
        }
        if !state.programs.contains_key(&key) {
            if state.programs.len() >= self.config.max_programs {
                return Err("program limit reached");
            }
            let since_slot = state.highest_seen.map(|slot| slot.saturating_add(1));
            state.programs.insert(
                key,
                Program {
                    clients: 0,
                    idle_since: None,
                    since_slot,
                    evicted: None,
                    gap_slot: None,
                    replay: VecDeque::new(),
                    live: broadcast::channel(self.config.live_capacity).0,
                },
            );
            self.refresh_watch(&state);
        }
        state.connections += 1;
        let program = state.programs.get_mut(&key).unwrap();
        program.clients += 1;
        program.idle_since = None;
        // Install the live receiver while holding the publication lock. Replay and
        // live delivery must not leave a window in which an event can disappear.
        let live = program.live.subscribe();
        let requested_slot = resume.after.map(|id| id.slot).or(resume.from_slot);
        let outside_coverage =
            requested_slot.is_some_and(|slot| program.since_slot.is_none_or(|since| slot < since));
        let expired = resume
            .after
            .is_some_and(|id| program.evicted.is_some_and(|floor| id < floor))
            || resume
                .from_slot
                .is_some_and(|slot| program.evicted.is_some_and(|floor| slot <= floor.slot));
        let gap =
            requested_slot.is_some_and(|slot| program.gap_slot.is_some_and(|gap| slot <= gap));
        let mut replay = VecDeque::new();
        if outside_coverage || expired || gap {
            replay.push_back(Arc::new(Self::resync(key, program, "replay_unavailable")));
        }
        if requested_slot.is_some() {
            replay.extend(program.replay.iter().filter_map(
                |(_, message)| match message.as_ref() {
                    StreamMessage::Event(event) if resume.accepts(event.id()) => {
                        Some(message.clone())
                    }
                    _ => None,
                },
            ));
        }
        Ok(Subscription {
            broker: self.clone(),
            program: key,
            replay,
            live,
            resume,
        })
    }

    pub fn lag_notice(&self, key: Pubkey) -> StreamMessage {
        let state = self.state.lock().unwrap();
        Self::resync(key, state.programs.get(&key).unwrap(), "slow_consumer")
    }

    fn resync(key: Pubkey, program: &Program, reason: &'static str) -> StreamMessage {
        let last_available_id =
            program
                .replay
                .front()
                .and_then(|(_, message)| match message.as_ref() {
                    StreamMessage::Event(event) => Some(event.id().to_string()),
                    _ => None,
                });
        StreamMessage::Resync {
            program_id: key.to_string(),
            last_available_id,
            reason,
        }
    }

    #[cfg(test)]
    pub fn transaction(&self, slot: u64, bank_id: u64, index: usize, events: Vec<ProgramEvent>) {
        self.transaction_with_watch(slot, bank_id, index, events, &self.watched());
    }

    pub fn transaction_with_watch(
        &self,
        slot: u64,
        bank_id: u64,
        index: usize,
        events: Vec<ProgramEvent>,
        watched: &Arc<HashSet<Pubkey>>,
    ) {
        let mut state = self.state.lock().unwrap();
        Self::observe_slot(&mut state, slot);
        // A subscription can arrive while a callback parses an older watch snapshot.
        // Such a slot is not complete replay coverage for newly watched programs.
        if !Arc::ptr_eq(watched, &self.watched()) {
            for (key, program) in &mut state.programs {
                if !watched.contains(key) {
                    program.since_slot = Some(
                        program
                            .since_slot
                            .unwrap_or_default()
                            .max(slot.saturating_add(1)),
                    );
                }
            }
        }
        if state.dead.contains(&slot) {
            return;
        }
        if state.published.is_some_and(|published| slot <= published) {
            if !events.is_empty() {
                self.gap(&mut state, slot, "late_transaction");
            }
            return;
        }
        let bank = state.pending.entry((slot, bank_id)).or_default();
        if !bank.transactions.insert(index) {
            return;
        }
        let bytes: usize = events.iter().map(ProgramEvent::size).sum();
        bank.bytes = bank.bytes.saturating_add(bytes);
        bank.events.extend(events);
        state.pending_transactions += 1;
        state.pending_bytes = state.pending_bytes.saturating_add(bytes);
        self.enforce_pending_limits(&mut state);
        self.flush(&mut state);
    }

    pub fn metadata(
        &self,
        slot: u64,
        bank_id: u64,
        parent: u64,
        count: u64,
        block_time: Option<i64>,
    ) {
        let mut state = self.state.lock().unwrap();
        if state.dead.contains(&slot) || state.published.is_some_and(|published| slot <= published)
        {
            return;
        }
        Self::observe_slot(&mut state, slot);
        let bank = state.pending.entry((slot, bank_id)).or_default();
        bank.parent = Some(parent);
        bank.metadata = Some((count, block_time));
        self.enforce_pending_limits(&mut state);
        self.flush(&mut state);
    }

    pub fn status(&self, slot: u64, bank_id: u64, parent: Option<u64>, status: &SlotStatus) {
        let mut state = self.state.lock().unwrap();
        if matches!(status, SlotStatus::Dead(_)) {
            Self::observe_slot(&mut state, slot);
            state.dead.insert(slot);
            let keys: Vec<_> = state
                .pending
                .range((slot, 0)..=(slot, u64::MAX))
                .map(|(key, _)| *key)
                .collect();
            for key in keys {
                Self::remove_bank(&mut state, key);
            }
            state.selected.remove(&slot);
            self.enforce_pending_limits(&mut state);
            self.flush(&mut state);
            return;
        }
        if !matches!(
            status,
            SlotStatus::CreatedBank
                | SlotStatus::Processed
                | SlotStatus::Confirmed
                | SlotStatus::Rooted
        ) || state.dead.contains(&slot)
            || state.published.is_some_and(|published| slot <= published)
        {
            return;
        }
        Self::observe_slot(&mut state, slot);
        let bank = state.pending.entry((slot, bank_id)).or_default();
        bank.parent = parent.or(bank.parent);
        if matches!(status, SlotStatus::Rooted)
            || (self.config.commitment == Commitment::Confirmed
                && matches!(status, SlotStatus::Confirmed))
        {
            state.selected.insert(slot, bank_id);
        }
        self.enforce_pending_limits(&mut state);
        self.flush(&mut state);
    }

    fn flush(&self, state: &mut State) {
        while let Some((&slot, &bank_id)) = state.selected.first_key_value() {
            let Some(bank) = state.pending.get(&(slot, bank_id)) else {
                break;
            };
            let Some((count, block_time)) = bank.metadata else {
                break;
            };
            if bank.transactions.len() as u64 != count {
                break;
            }
            // Slot confirmation and TransactionStatusService callbacks run on
            // different threads. A descendant must wait for its pending parent.
            if bank.parent.is_some_and(|parent| {
                parent < slot
                    && state
                        .pending
                        .keys()
                        .any(|(pending_slot, _)| *pending_slot == parent)
            }) {
                break;
            }
            let mut bank = Self::remove_bank(state, (slot, bank_id)).unwrap();
            state.selected.remove(&slot);
            bank.events.sort_unstable_by_key(ProgramEvent::id);
            let now = Instant::now();
            for mut event in bank.events {
                event.block_time = block_time;
                event.commitment = self.config.commitment;
                let key: Pubkey = event.program_id.parse().expect("extracted program ID");
                let size = event.size();
                if let Some(program) = state.programs.get_mut(&key) {
                    let message = Arc::new(StreamMessage::Event(event));
                    program.replay.push_back((now, message.clone()));
                    let _ = program.live.send(message);
                    state.replay_bytes = state.replay_bytes.saturating_add(size);
                }
            }
            state.published = Some(slot);
            let obsolete: Vec<_> = state
                .pending
                .keys()
                .take_while(|(pending_slot, _)| *pending_slot <= slot)
                .copied()
                .collect();
            for key in obsolete {
                Self::remove_bank(state, key);
            }
            self.cleanup_locked(state, now);
        }
    }

    fn observe_slot(state: &mut State, slot: u64) {
        if state.highest_seen.is_none() {
            for program in state.programs.values_mut() {
                if program.since_slot.is_none() {
                    program.since_slot = Some(slot);
                }
            }
        }
        state.highest_seen = Some(state.highest_seen.unwrap_or(slot).max(slot));
    }

    fn remove_bank(state: &mut State, key: BankKey) -> Option<PendingBank> {
        let bank = state.pending.remove(&key)?;
        state.pending_bytes = state.pending_bytes.saturating_sub(bank.bytes);
        state.pending_transactions = state
            .pending_transactions
            .saturating_sub(bank.transactions.len());
        Some(bank)
    }

    fn gap(&self, state: &mut State, slot: u64, reason: &'static str) {
        for (&key, program) in &mut state.programs {
            program.gap_slot = Some(program.gap_slot.unwrap_or(slot).max(slot));
            let _ = program
                .live
                .send(Arc::new(Self::resync(key, program, reason)));
        }
    }

    fn enforce_pending_limits(&self, state: &mut State) {
        while state.pending.len() > self.config.max_pending_slots
            || state.pending_transactions > self.config.max_pending_transactions
            || state.pending_bytes > self.config.max_pending_bytes
        {
            let Some(&(slot, _)) = state.pending.keys().next() else {
                break;
            };
            let keys: Vec<_> = state
                .pending
                .range((slot, 0)..=(slot, u64::MAX))
                .map(|(key, _)| *key)
                .collect();
            for key in keys {
                Self::remove_bank(state, key);
            }
            state.selected.remove(&slot);
            state.published = Some(state.published.unwrap_or(slot).max(slot));
            self.gap(state, slot, "pending_limit");
        }
        let cutoff = state
            .highest_seen
            .unwrap_or_default()
            .saturating_sub(self.config.max_pending_slots as u64);
        state.dead.retain(|slot| *slot >= cutoff);
        while state.dead.len() > self.config.max_pending_slots {
            state.dead.pop_first();
        }
    }

    pub fn cleanup(&self) {
        self.cleanup_locked(&mut self.state.lock().unwrap(), Instant::now());
    }

    fn cleanup_locked(&self, state: &mut State, now: Instant) {
        let grace = Duration::from_secs(self.config.unsubscribe_grace_seconds);
        let remove: Vec<_> = state
            .programs
            .iter()
            .filter_map(|(key, program)| {
                (program.clients == 0
                    && program
                        .idle_since
                        .is_some_and(|idle| now.duration_since(idle) >= grace))
                .then_some(*key)
            })
            .collect();
        for key in &remove {
            let program = state.programs.remove(key).unwrap();
            for (_, message) in program.replay {
                if let StreamMessage::Event(event) = message.as_ref() {
                    state.replay_bytes = state.replay_bytes.saturating_sub(event.size());
                }
            }
        }
        if !remove.is_empty() {
            self.refresh_watch(state);
        }
        let retention = Duration::from_secs(self.config.replay_seconds);
        for program in state.programs.values_mut() {
            while program.replay.len() > self.config.max_events_per_program
                || program
                    .replay
                    .front()
                    .is_some_and(|(at, _)| now.duration_since(*at) >= retention)
            {
                Self::evict(program, &mut state.replay_bytes);
            }
        }
        while state.replay_bytes > self.config.max_replay_bytes {
            let oldest = state
                .programs
                .iter()
                .filter_map(|(key, program)| program.replay.front().map(|(at, _)| (*key, *at)))
                .min_by_key(|(_, at)| *at);
            let Some((key, _)) = oldest else {
                break;
            };
            Self::evict(
                state.programs.get_mut(&key).unwrap(),
                &mut state.replay_bytes,
            );
        }
    }

    fn evict(program: &mut Program, total: &mut usize) {
        if let Some((_, message)) = program.replay.pop_front()
            && let StreamMessage::Event(event) = message.as_ref()
        {
            program.evicted = Some(event.id());
            *total = total.saturating_sub(event.size());
        }
    }

    fn refresh_watch(&self, state: &State) {
        *self.watched.write().unwrap() = Arc::new(state.programs.keys().copied().collect());
    }
}

#[cfg(test)]
mod tests;
