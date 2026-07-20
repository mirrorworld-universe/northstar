//! Crash-durable storage for ER state that has not reached L1 settlement.
//!
//! Mutations are synced to a framed, checksummed journal before ER RPC execution returns success.
//! Periodic atomic snapshots compact the journal. Recovery is allowed only when the complete
//! immutable Portal/session identity matches the active L1 Session.
use {
    crate::settlement::WithdrawalPayoutEvent,
    borsh::{BorshDeserialize, BorshSerialize},
    solana_account::{AccountSharedData, ReadableAccount},
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hash,
    solana_signature::Signature,
    std::{
        collections::{BTreeMap, BTreeSet},
        fs::{self, File, OpenOptions},
        io::{self, Read, Write},
        path::{Path, PathBuf},
        sync::Mutex,
    },
};

const FORMAT_VERSION: u16 = 2;
const SNAPSHOT_FILE: &str = "state.borsh";
const JOURNAL_FILE: &str = "journal.bin";
const COMPACT_RECORDS: usize = 128;
const COMPACT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct UnsettledSessionIdentity {
    pub format_version: u16,
    pub portal_program_id: [u8; 32],
    pub session_pda: [u8; 32],
    pub nonce: u128,
    pub created_at: u64,
    pub authority: [u8; 32],
    pub validator: [u8; 32],
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
    pub settlement_interval_slots: u64,
    pub bump: u8,
}

impl UnsettledSessionIdentity {
    pub fn new(
        portal_program_id: Pubkey,
        session_pda: Pubkey,
        session: &northstar_portal::Session,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            portal_program_id: portal_program_id.to_bytes(),
            session_pda: session_pda.to_bytes(),
            nonce: session.nonce,
            created_at: session.created_at,
            authority: session.authority,
            validator: session.validator,
            grid_id: session.grid_id,
            ttl_slots: session.ttl_slots,
            fee_cap: session.fee_cap,
            settlement_interval_slots: session.settlement_interval_slots,
            bump: session.bump,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredUnsettledState {
    pub accounts: Vec<(Pubkey, AccountSharedData)>,
    pub touched_accounts: Vec<Pubkey>,
    pub payout_events: Vec<WithdrawalPayoutEvent>,
    pub processed_signatures: Vec<(Signature, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDisposition {
    New,
    Recovered,
    DroppedIdentityMismatch,
    DroppedCorrupt,
}

#[derive(Debug)]
pub struct RecoveryOutcome {
    pub disposition: RecoveryDisposition,
    pub state: RecoveredUnsettledState,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
struct PersistedAccount {
    pubkey: [u8; 32],
    lamports: u64,
    data: Vec<u8>,
    owner: [u8; 32],
    executable: bool,
    rent_epoch: u64,
}

impl PersistedAccount {
    fn new(pubkey: Pubkey, account: &AccountSharedData) -> Self {
        Self {
            pubkey: pubkey.to_bytes(),
            lamports: account.lamports(),
            data: account.data().to_vec(),
            owner: account.owner().to_bytes(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
        }
    }

    fn into_runtime(self) -> (Pubkey, AccountSharedData) {
        let account = solana_account::Account {
            lamports: self.lamports,
            data: self.data,
            owner: Pubkey::new_from_array(self.owner),
            executable: self.executable,
            rent_epoch: self.rent_epoch,
        };
        (Pubkey::new_from_array(self.pubkey), account.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
struct PersistedPayoutEvent {
    er_source: [u8; 32],
    l1_recipient: [u8; 32],
    lamports: u64,
    cumulative_withdrawn: u64,
    signature: [u8; 64],
    er_slot: u64,
}

impl From<&WithdrawalPayoutEvent> for PersistedPayoutEvent {
    fn from(event: &WithdrawalPayoutEvent) -> Self {
        Self {
            er_source: event.er_source.to_bytes(),
            l1_recipient: event.l1_recipient.to_bytes(),
            lamports: event.lamports,
            cumulative_withdrawn: event.cumulative_withdrawn,
            signature: event.signature.as_ref().try_into().unwrap(),
            er_slot: event.er_slot,
        }
    }
}

impl From<PersistedPayoutEvent> for WithdrawalPayoutEvent {
    fn from(event: PersistedPayoutEvent) -> Self {
        Self {
            er_source: Pubkey::new_from_array(event.er_source),
            l1_recipient: Pubkey::new_from_array(event.l1_recipient),
            lamports: event.lamports,
            cumulative_withdrawn: event.cumulative_withdrawn,
            signature: Signature::from(event.signature),
            er_slot: event.er_slot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
struct PersistedSignature {
    signature: [u8; 64],
    slot: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
struct PersistedState {
    accounts: BTreeMap<[u8; 32], PersistedAccount>,
    touched_accounts: BTreeSet<[u8; 32]>,
    payout_events: Vec<PersistedPayoutEvent>,
    processed_signatures: BTreeMap<[u8; 64], u64>,
}

impl PersistedState {
    fn recovered(&self) -> RecoveredUnsettledState {
        RecoveredUnsettledState {
            accounts: self
                .accounts
                .values()
                .cloned()
                .map(PersistedAccount::into_runtime)
                .collect(),
            touched_accounts: self
                .touched_accounts
                .iter()
                .map(|key| Pubkey::new_from_array(*key))
                .collect(),
            payout_events: self
                .payout_events
                .iter()
                .cloned()
                .map(WithdrawalPayoutEvent::from)
                .collect(),
            processed_signatures: self
                .processed_signatures
                .iter()
                .map(|(signature, slot)| (Signature::from(*signature), *slot))
                .collect(),
        }
    }

    fn apply(&mut self, mutation: Mutation) {
        match mutation {
            Mutation::Update {
                accounts,
                touched_accounts,
                payout_events,
                processed_signatures,
            } => {
                for account in accounts {
                    self.accounts.insert(account.pubkey, account);
                }
                self.touched_accounts.extend(touched_accounts);
                if let Some(events) = payout_events {
                    self.payout_events = events;
                }
                for signature in processed_signatures {
                    self.processed_signatures
                        .insert(signature.signature, signature.slot);
                }
            }
        }
    }
}

#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
struct Snapshot {
    identity: UnsettledSessionIdentity,
    last_sequence: u64,
    state: PersistedState,
}

#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
struct JournalRecord {
    sequence: u64,
    mutation: Mutation,
}

#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
enum Mutation {
    Update {
        accounts: Vec<PersistedAccount>,
        touched_accounts: Vec<[u8; 32]>,
        payout_events: Option<Vec<PersistedPayoutEvent>>,
        processed_signatures: Vec<PersistedSignature>,
    },
}

#[derive(Debug)]
struct ActiveStore {
    identity: UnsettledSessionIdentity,
    state: PersistedState,
    sequence: u64,
    records_since_compaction: usize,
    journal_bytes: u64,
}

#[derive(Debug, Default)]
struct StoreInner {
    dir: Option<PathBuf>,
    active: Option<ActiveStore>,
    writes_enabled: bool,
}

#[derive(Debug, Default)]
pub struct UnsettledStateStore {
    inner: Mutex<StoreInner>,
}

impl UnsettledStateStore {
    pub fn configure(&self, dir: PathBuf) {
        let mut inner = self.inner.lock().unwrap();
        inner.dir = Some(dir);
    }

    pub fn enable_writes(&self) {
        self.inner.lock().unwrap().writes_enabled = true;
    }

    pub fn begin_session(&self, identity: UnsettledSessionIdentity) -> io::Result<RecoveryOutcome> {
        let mut inner = self.inner.lock().unwrap();
        inner.writes_enabled = false;
        let Some(dir) = inner.dir.clone() else {
            inner.active = Some(ActiveStore {
                identity,
                state: PersistedState::default(),
                sequence: 0,
                records_since_compaction: 0,
                journal_bytes: 0,
            });
            return Ok(RecoveryOutcome {
                disposition: RecoveryDisposition::New,
                state: PersistedState::default().recovered(),
            });
        };
        fs::create_dir_all(&dir)?;

        let snapshot_path = dir.join(SNAPSHOT_FILE);
        let journal_path = dir.join(JOURNAL_FILE);
        let (mut snapshot, disposition) = if snapshot_path.exists() {
            match fs::read(&snapshot_path)
                .ok()
                .and_then(|bytes| borsh::from_slice::<Snapshot>(&bytes).ok())
            {
                Some(snapshot) if snapshot.identity == identity => {
                    (snapshot, RecoveryDisposition::Recovered)
                }
                Some(_) => {
                    Self::reset_files(&dir, identity.clone())?;
                    (
                        Snapshot {
                            identity: identity.clone(),
                            last_sequence: 0,
                            state: PersistedState::default(),
                        },
                        RecoveryDisposition::DroppedIdentityMismatch,
                    )
                }
                None => {
                    Self::reset_files(&dir, identity.clone())?;
                    (
                        Snapshot {
                            identity: identity.clone(),
                            last_sequence: 0,
                            state: PersistedState::default(),
                        },
                        RecoveryDisposition::DroppedCorrupt,
                    )
                }
            }
        } else if journal_path.exists() && fs::metadata(&journal_path)?.len() > 0 {
            Self::reset_files(&dir, identity.clone())?;
            (
                Snapshot {
                    identity: identity.clone(),
                    last_sequence: 0,
                    state: PersistedState::default(),
                },
                RecoveryDisposition::DroppedCorrupt,
            )
        } else {
            Self::reset_files(&dir, identity.clone())?;
            (
                Snapshot {
                    identity: identity.clone(),
                    last_sequence: 0,
                    state: PersistedState::default(),
                },
                RecoveryDisposition::New,
            )
        };

        let mut replayed = 0usize;
        let mut journal_bytes = 0u64;
        if disposition == RecoveryDisposition::Recovered && journal_path.exists() {
            match Self::read_journal(&journal_path) {
                Ok(records) => {
                    journal_bytes = fs::metadata(&journal_path)?.len();
                    for record in records {
                        if record.sequence <= snapshot.last_sequence {
                            continue;
                        }
                        if record.sequence != snapshot.last_sequence.saturating_add(1) {
                            Self::reset_files(&dir, identity.clone())?;
                            snapshot.last_sequence = 0;
                            snapshot.state = PersistedState::default();
                            inner.active = Some(ActiveStore {
                                identity,
                                state: snapshot.state.clone(),
                                sequence: 0,
                                records_since_compaction: 0,
                                journal_bytes: 0,
                            });
                            return Ok(RecoveryOutcome {
                                disposition: RecoveryDisposition::DroppedCorrupt,
                                state: snapshot.state.recovered(),
                            });
                        }
                        snapshot.state.apply(record.mutation);
                        snapshot.last_sequence = record.sequence;
                        replayed += 1;
                    }
                }
                Err(_) => {
                    Self::reset_files(&dir, identity.clone())?;
                    snapshot.last_sequence = 0;
                    snapshot.state = PersistedState::default();
                    inner.active = Some(ActiveStore {
                        identity,
                        state: snapshot.state.clone(),
                        sequence: 0,
                        records_since_compaction: 0,
                        journal_bytes: 0,
                    });
                    return Ok(RecoveryOutcome {
                        disposition: RecoveryDisposition::DroppedCorrupt,
                        state: snapshot.state.recovered(),
                    });
                }
            }
        }

        let recovered = snapshot.state.recovered();
        inner.active = Some(ActiveStore {
            identity,
            state: snapshot.state,
            sequence: snapshot.last_sequence,
            records_since_compaction: replayed,
            journal_bytes,
        });
        Ok(RecoveryOutcome {
            disposition,
            state: recovered,
        })
    }

    pub fn append_update(
        &self,
        accounts: &[(Pubkey, AccountSharedData)],
        touched_accounts: &[Pubkey],
        payout_events: Option<&[WithdrawalPayoutEvent]>,
        processed_signatures: &[(Signature, u64)],
    ) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.writes_enabled {
            return Ok(());
        }
        let Some(dir) = inner.dir.clone() else {
            return Ok(());
        };
        let Some(active) = inner.active.as_mut() else {
            return Ok(());
        };
        let mutation = Mutation::Update {
            accounts: accounts
                .iter()
                .map(|(pubkey, account)| PersistedAccount::new(*pubkey, account))
                .collect(),
            touched_accounts: touched_accounts.iter().map(Pubkey::to_bytes).collect(),
            payout_events: payout_events
                .map(|events| events.iter().map(PersistedPayoutEvent::from).collect()),
            processed_signatures: processed_signatures
                .iter()
                .map(|(signature, slot)| PersistedSignature {
                    signature: signature.as_ref().try_into().unwrap(),
                    slot: *slot,
                })
                .collect(),
        };
        let record = JournalRecord {
            sequence: active.sequence.saturating_add(1),
            mutation: mutation.clone(),
        };
        let frame = Self::encode_record(&record)?;
        let journal_path = dir.join(JOURNAL_FILE);
        let mut journal = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal_path)?;
        journal.write_all(&frame)?;
        journal.sync_data()?;

        active.state.apply(mutation);
        active.sequence = record.sequence;
        active.records_since_compaction += 1;
        active.journal_bytes = active.journal_bytes.saturating_add(frame.len() as u64);
        if active.records_since_compaction >= COMPACT_RECORDS
            || active.journal_bytes >= COMPACT_BYTES
        {
            Self::compact(&dir, active)?;
        }
        Ok(())
    }

    pub fn clear(&self) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(dir) = inner.dir.as_ref() {
            Self::remove_file_if_exists(&dir.join(SNAPSHOT_FILE))?;
            Self::remove_file_if_exists(&dir.join(JOURNAL_FILE))?;
        }
        inner.active = None;
        inner.writes_enabled = false;
        Ok(())
    }

    fn reset_files(dir: &Path, identity: UnsettledSessionIdentity) -> io::Result<()> {
        Self::remove_file_if_exists(&dir.join(JOURNAL_FILE))?;
        let snapshot = Snapshot {
            identity,
            last_sequence: 0,
            state: PersistedState::default(),
        };
        Self::write_snapshot(dir, &snapshot)
    }

    fn compact(dir: &Path, active: &mut ActiveStore) -> io::Result<()> {
        let snapshot = Snapshot {
            identity: active.identity.clone(),
            last_sequence: active.sequence,
            state: active.state.clone(),
        };
        Self::write_snapshot(dir, &snapshot)?;
        let journal_path = dir.join(JOURNAL_FILE);
        let journal = File::create(journal_path)?;
        journal.sync_all()?;
        active.records_since_compaction = 0;
        active.journal_bytes = 0;
        Ok(())
    }

    fn write_snapshot(dir: &Path, snapshot: &Snapshot) -> io::Result<()> {
        let bytes = borsh::to_vec(snapshot).map_err(io::Error::other)?;
        let tmp_path = dir.join(format!("{SNAPSHOT_FILE}.tmp.{}", std::process::id()));
        let final_path = dir.join(SNAPSHOT_FILE);
        let mut file = File::create(&tmp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp_path, &final_path)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    }

    fn encode_record(record: &JournalRecord) -> io::Result<Vec<u8>> {
        let payload = borsh::to_vec(record).map_err(io::Error::other)?;
        let len = u64::try_from(payload.len()).map_err(io::Error::other)?;
        let checksum = hash(&payload).to_bytes();
        let mut frame = Vec::with_capacity(8 + checksum.len() + payload.len());
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&checksum);
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    fn read_journal(path: &Path) -> io::Result<Vec<JournalRecord>> {
        let mut file = File::open(path)?;
        let mut records = Vec::new();
        loop {
            let mut len_bytes = [0u8; 8];
            let bytes_read = file.read(&mut len_bytes)?;
            if bytes_read == 0 {
                return Ok(records);
            }
            if bytes_read < len_bytes.len() {
                file.read_exact(&mut len_bytes[bytes_read..])
                    .map_err(|err| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("torn journal header: {err}"),
                        )
                    })?;
            }
            let len = usize::try_from(u64::from_le_bytes(len_bytes)).map_err(io::Error::other)?;
            if len > MAX_RECORD_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "journal record too large",
                ));
            }
            let mut expected_checksum = [0u8; 32];
            file.read_exact(&mut expected_checksum)?;
            let mut payload = vec![0u8; len];
            file.read_exact(&mut payload)?;
            if hash(&payload).to_bytes() != expected_checksum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "journal checksum mismatch",
                ));
            }
            records.push(borsh::from_slice(&payload).map_err(io::Error::other)?);
        }
    }

    fn remove_file_if_exists(path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, solana_account::WritableAccount, solana_sdk_ids::system_program,
        tempfile::TempDir,
    };

    fn identity(portal: Pubkey, grid_id: u64) -> UnsettledSessionIdentity {
        UnsettledSessionIdentity {
            format_version: FORMAT_VERSION,
            portal_program_id: portal.to_bytes(),
            session_pda: Pubkey::new_unique().to_bytes(),
            nonce: 3,
            created_at: 10,
            authority: Pubkey::new_unique().to_bytes(),
            validator: Pubkey::new_unique().to_bytes(),
            grid_id,
            ttl_slots: 100,
            fee_cap: 1_000,
            settlement_interval_slots: 20,
            bump: 254,
        }
    }

    #[test]
    fn test_journal_recovers_unsettled_state() {
        let dir = TempDir::new().unwrap();
        let portal = Pubkey::new_unique();
        let identity = identity(portal, 7);
        let account_key = Pubkey::new_unique();
        let mut account = AccountSharedData::new(42, 3, &system_program::id());
        account.data_as_mut_slice().copy_from_slice(&[1, 2, 3]);
        let signature = Signature::from([7; 64]);
        let payout = WithdrawalPayoutEvent {
            er_source: account_key,
            l1_recipient: Pubkey::new_unique(),
            lamports: 12,
            cumulative_withdrawn: 12,
            signature,
            er_slot: 99,
        };

        let store = UnsettledStateStore::default();
        store.configure(dir.path().to_path_buf());
        assert_eq!(
            store.begin_session(identity.clone()).unwrap().disposition,
            RecoveryDisposition::New
        );
        store.enable_writes();
        store
            .append_update(
                &[(account_key, account.clone())],
                &[account_key],
                Some(std::slice::from_ref(&payout)),
                &[(signature, 99)],
            )
            .unwrap();
        drop(store);

        let recovered_store = UnsettledStateStore::default();
        recovered_store.configure(dir.path().to_path_buf());
        let outcome = recovered_store.begin_session(identity).unwrap();
        assert_eq!(outcome.disposition, RecoveryDisposition::Recovered);
        assert_eq!(outcome.state.accounts, vec![(account_key, account)]);
        assert_eq!(outcome.state.touched_accounts, vec![account_key]);
        assert_eq!(outcome.state.processed_signatures, vec![(signature, 99)]);
        assert_eq!(outcome.state.payout_events, vec![payout]);
    }

    #[test]
    fn test_identity_mismatch_drops_unsettled_state() {
        let base = identity(Pubkey::new_unique(), 7);
        let mut mismatches = Vec::new();
        macro_rules! mismatch {
            ($field:ident, $value:expr) => {{
                let mut changed = base.clone();
                changed.$field = $value;
                mismatches.push(changed);
            }};
        }
        mismatch!(format_version, FORMAT_VERSION + 1);
        mismatch!(portal_program_id, Pubkey::new_unique().to_bytes());
        mismatch!(session_pda, Pubkey::new_unique().to_bytes());
        mismatch!(nonce, base.nonce + 1);
        mismatch!(created_at, base.created_at + 1);
        mismatch!(authority, Pubkey::new_unique().to_bytes());
        mismatch!(validator, Pubkey::new_unique().to_bytes());
        mismatch!(grid_id, base.grid_id + 1);
        mismatch!(ttl_slots, base.ttl_slots + 1);
        mismatch!(fee_cap, base.fee_cap + 1);
        mismatch!(
            settlement_interval_slots,
            base.settlement_interval_slots + 1
        );
        mismatch!(bump, base.bump.wrapping_add(1));

        for changed in mismatches {
            let dir = TempDir::new().unwrap();
            let store = UnsettledStateStore::default();
            store.configure(dir.path().to_path_buf());
            store.begin_session(base.clone()).unwrap();
            store.enable_writes();
            store
                .append_update(
                    &[(
                        Pubkey::new_unique(),
                        AccountSharedData::new(1, 0, &system_program::id()),
                    )],
                    &[],
                    None,
                    &[],
                )
                .unwrap();

            let outcome = store.begin_session(changed).unwrap();
            assert_eq!(
                outcome.disposition,
                RecoveryDisposition::DroppedIdentityMismatch
            );
            assert!(outcome.state.accounts.is_empty());
        }
    }

    #[test]
    fn test_journal_compacts_without_losing_state() {
        let dir = TempDir::new().unwrap();
        let identity = identity(Pubkey::new_unique(), 7);
        let account_key = Pubkey::new_unique();
        let store = UnsettledStateStore::default();
        store.configure(dir.path().to_path_buf());
        store.begin_session(identity.clone()).unwrap();
        store.enable_writes();
        for lamports in 1..=COMPACT_RECORDS as u64 {
            store
                .append_update(
                    &[(
                        account_key,
                        AccountSharedData::new(lamports, 0, &system_program::id()),
                    )],
                    &[account_key],
                    None,
                    &[],
                )
                .unwrap();
        }
        assert_eq!(
            fs::metadata(dir.path().join(JOURNAL_FILE)).unwrap().len(),
            0
        );
        drop(store);

        let recovered_store = UnsettledStateStore::default();
        recovered_store.configure(dir.path().to_path_buf());
        let outcome = recovered_store.begin_session(identity).unwrap();
        assert_eq!(outcome.disposition, RecoveryDisposition::Recovered);
        assert_eq!(
            outcome.state.accounts[0].1.lamports(),
            COMPACT_RECORDS as u64
        );
    }

    #[test]
    fn test_corrupt_journal_drops_unsettled_state() {
        let dir = TempDir::new().unwrap();
        let identity = identity(Pubkey::new_unique(), 7);
        let store = UnsettledStateStore::default();
        store.configure(dir.path().to_path_buf());
        store.begin_session(identity.clone()).unwrap();
        store.enable_writes();
        store
            .append_update(
                &[(
                    Pubkey::new_unique(),
                    AccountSharedData::new(1, 0, &system_program::id()),
                )],
                &[],
                None,
                &[],
            )
            .unwrap();
        fs::write(dir.path().join(JOURNAL_FILE), b"torn").unwrap();

        let recovered_store = UnsettledStateStore::default();
        recovered_store.configure(dir.path().to_path_buf());
        let outcome = recovered_store.begin_session(identity).unwrap();
        assert_eq!(outcome.disposition, RecoveryDisposition::DroppedCorrupt);
        assert!(outcome.state.accounts.is_empty());
    }
}
