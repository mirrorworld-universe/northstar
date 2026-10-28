use {
    crate::{Error, Metadata, OracleKey, Policy, Request, Result, VrfResult, verify},
    serde::{Deserialize, Serialize},
    sha2::{Digest, Sha256},
    std::{
        collections::BTreeMap,
        fs::{self, File, OpenOptions},
        io::Write,
        path::{Path, PathBuf},
    },
};

const MAX_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REQUESTS: usize = 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub enum Status {
    Ready { next_tick: u64 },
    Pending { submitted_at: u64, retry_at: u64 },
    Confirmed { confirmed_at: u64 },
    Expired,
    Exhausted,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub request_id: [u8; 32],
    pub binding: [u8; 32],
    pub attempt_id: String,
    pub result: VrfResult,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ingest {
    New,
    Duplicate,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    request: Request,
    attempts: u32,
    result: Option<VrfResult>,
    status: Status,
}

impl Record {
    fn attempt(&self, metadata: &Metadata) -> Result<Attempt> {
        let binding = self.request.binding(metadata)?;
        let mut hash = Sha256::new();
        hash.update(b"northstar/vrf-oracle/attempt-v1\0");
        hash.update(binding);
        hash.update(self.attempts.to_be_bytes());
        Ok(Attempt {
            request_id: self.request.id,
            binding,
            attempt_id: hex::encode(hash.finalize()),
            result: self.result.clone().ok_or(Error::Journal)?,
        })
    }

    fn matches(&self, metadata: &Metadata, attempt: &Attempt) -> Result<()> {
        if self.attempt(metadata)? != *attempt {
            return Err(Error::Receipt);
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    metadata: Metadata,
    policy: Policy,
    clock: u64,
    records: BTreeMap<String, Record>,
}

impl Snapshot {
    fn advance(&mut self, now: u64) -> Result<()> {
        if now < self.clock {
            return Err(Error::Clock);
        }
        self.clock = now;
        Ok(())
    }

    fn record(&mut self, id: [u8; 32]) -> Result<&mut Record> {
        self.records.get_mut(&hex::encode(id)).ok_or(Error::Unknown)
    }

    fn validate(&self, metadata: &Metadata, policy: &Policy) -> Result<()> {
        if self.version != 1
            || self.metadata != *metadata
            || self.policy != *policy
            || self.records.len() > MAX_REQUESTS
        {
            return Err(Error::Journal);
        }
        for (id, record) in &self.records {
            record.request.validate(metadata)?;
            if *id != hex::encode(record.request.id) || record.attempts > policy.max_attempts {
                return Err(Error::Journal);
            }
            match (&record.result, record.attempts) {
                (None, 0) => (),
                (Some(result), 1..) => {
                    verify(metadata, &record.request, result)?;
                }
                _ => return Err(Error::Journal),
            }
            let request = &record.request;
            let valid = match record.status {
                Status::Ready { next_tick } => {
                    record.attempts < policy.max_attempts
                        && next_tick >= request.not_before
                        && (record.attempts > 0 || next_tick == request.not_before)
                }
                Status::Pending {
                    submitted_at,
                    retry_at,
                } => {
                    record.attempts > 0
                        && submitted_at >= request.not_before
                        && submitted_at < request.expires_at
                        && submitted_at <= self.clock
                        && submitted_at.checked_add(policy.pending_timeout) == Some(retry_at)
                }
                Status::Confirmed { confirmed_at } => {
                    record.attempts > 0
                        && confirmed_at >= request.not_before
                        && confirmed_at < request.expires_at
                        && confirmed_at <= self.clock
                }
                Status::Expired => self.clock >= request.expires_at,
                Status::Exhausted => record.attempts == policy.max_attempts,
            };
            if !valid {
                return Err(Error::Journal);
            }
        }
        Ok(())
    }
}

/// Single-writer, bounded public journal. A persisted Pending record is an intent,
/// not evidence of network submission, inclusion, callback success, or finality.
pub struct Oracle {
    directory: PathBuf,
    _lock: File,
    snapshot: Snapshot,
    poisoned: bool,
}

impl Oracle {
    pub fn open(directory: impl AsRef<Path>, metadata: Metadata, policy: Policy) -> Result<Self> {
        metadata.validate()?;
        policy.validate()?;
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;
        let lock_path = directory.join("writer.lock");
        let (lock, initialized) = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(file) => (file, false),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
                OpenOptions::new().read(true).write(true).open(&lock_path)?,
                true,
            ),
            Err(error) => return Err(error.into()),
        };
        lock.try_lock().map_err(|_| Error::Locked)?;
        let path = directory.join("public-state.json");
        let (snapshot, new) = match fs::metadata(&path) {
            Ok(info) => {
                if !info.is_file() || info.len() > MAX_JOURNAL_BYTES {
                    return Err(Error::Journal);
                }
                let snapshot: Snapshot = serde_json::from_slice(&fs::read(path)?)?;
                snapshot.validate(&metadata, &policy)?;
                (snapshot, false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if initialized {
                    return Err(Error::Journal);
                }
                (
                    Snapshot {
                        version: 1,
                        metadata,
                        policy,
                        clock: 0,
                        records: BTreeMap::new(),
                    },
                    true,
                )
            }
            Err(error) => return Err(error.into()),
        };
        let oracle = Self {
            directory,
            _lock: lock,
            snapshot,
            poisoned: false,
        };
        if new {
            oracle.save(&oracle.snapshot)?;
        }
        Ok(oracle)
    }

    fn save(&self, snapshot: &Snapshot) -> Result<()> {
        let bytes = serde_json::to_vec(snapshot)?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(Error::Journal);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.directory.join("public-state.json"))
            .map_err(|error| Error::Io(error.error))?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    fn transact<T>(&mut self, change: impl FnOnce(&mut Snapshot) -> Result<T>) -> Result<T> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let mut next = self.snapshot.clone();
        let result = change(&mut next)?;
        if let Err(error) = self.save(&next) {
            // Rename may have succeeded even when directory fsync fails.
            self.poisoned = true;
            return Err(error);
        }
        self.snapshot = next;
        Ok(result)
    }

    pub fn ingest(&mut self, request: Request, now: u64) -> Result<Ingest> {
        self.transact(|snapshot| {
            snapshot.advance(now)?;
            request.validate(&snapshot.metadata)?;
            let id = hex::encode(request.id);
            if let Some(existing) = snapshot.records.get(&id) {
                return if existing.request == request {
                    Ok(Ingest::Duplicate)
                } else {
                    Err(Error::Conflict)
                };
            }
            if snapshot.records.len() >= MAX_REQUESTS {
                return Err(Error::Request);
            }
            let status = if now >= request.expires_at {
                Status::Expired
            } else {
                Status::Ready {
                    next_tick: request.not_before,
                }
            };
            snapshot.records.insert(
                id,
                Record {
                    request,
                    attempts: 0,
                    result: None,
                    status,
                },
            );
            Ok(Ingest::New)
        })
    }

    /// Durably records an intent BEFORE returning anything an adapter could send.
    /// An uncertain send stays Pending until its timeout; retries use the same proof.
    pub fn prepare(&mut self, id: [u8; 32], now: u64, key: &OracleKey) -> Result<Option<Attempt>> {
        self.transact(|snapshot| {
            snapshot.advance(now)?;
            let metadata = snapshot.metadata.clone();
            let policy = snapshot.policy.clone();
            let record = snapshot.record(id)?;
            if matches!(
                record.status,
                Status::Confirmed { .. } | Status::Expired | Status::Exhausted
            ) {
                return Ok(None);
            }
            if now >= record.request.expires_at {
                record.status = Status::Expired;
                return Ok(None);
            }
            let due = match record.status {
                Status::Ready { next_tick } => next_tick,
                Status::Pending { retry_at, .. } => retry_at,
                _ => unreachable!(),
            };
            if now < due {
                return Ok(None);
            }
            if key.public_key() != record.request.public_key {
                return Err(Error::Key);
            }
            if record.attempts >= policy.max_attempts {
                record.status = Status::Exhausted;
                return Ok(None);
            }
            let retry_at = now
                .checked_add(policy.pending_timeout)
                .ok_or(Error::Clock)?;
            if record.result.is_none() {
                record.result = Some(key.prove(&metadata, &record.request)?);
            }
            record.attempts += 1;
            record.status = Status::Pending {
                submitted_at: now,
                retry_at,
            };
            Ok(Some(record.attempt(&metadata)?))
        })
    }

    /// Explicit fixture confirmation only. A live adapter must independently prove
    /// application consumption at its agreed commitment before calling a replacement.
    pub fn confirm_fixture(&mut self, attempt: &Attempt, now: u64) -> Result<bool> {
        self.transact(|snapshot| {
            snapshot.advance(now)?;
            let metadata = snapshot.metadata.clone();
            let record = snapshot.record(attempt.request_id)?;
            record.matches(&metadata, attempt)?;
            if matches!(record.status, Status::Confirmed { .. }) {
                return Ok(false);
            }
            if !matches!(record.status, Status::Pending { .. }) || now >= record.request.expires_at
            {
                return Err(Error::Receipt);
            }
            record.status = Status::Confirmed { confirmed_at: now };
            Ok(true)
        })
    }

    /// Only a definite rejection uses this path. An unknown outcome is NOT a failure.
    pub fn rejected_fixture(&mut self, attempt: &Attempt, now: u64) -> Result<()> {
        self.transact(|snapshot| {
            snapshot.advance(now)?;
            let metadata = snapshot.metadata.clone();
            let policy = snapshot.policy.clone();
            let record = snapshot.record(attempt.request_id)?;
            record.matches(&metadata, attempt)?;
            if !matches!(record.status, Status::Pending { .. }) {
                return Err(Error::Receipt);
            }
            record.status = if now >= record.request.expires_at {
                Status::Expired
            } else if record.attempts >= policy.max_attempts {
                Status::Exhausted
            } else {
                Status::Ready {
                    next_tick: now.checked_add(policy.retry_delay).ok_or(Error::Clock)?,
                }
            };
            Ok(())
        })
    }

    pub fn status(&self, id: [u8; 32]) -> Result<Status> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        self.snapshot
            .records
            .get(&hex::encode(id))
            .map(|record| record.status.clone())
            .ok_or(Error::Unknown)
    }

    /// Recovery/reconciliation handle, not authorization to resend before timeout.
    pub fn pending(&self, id: [u8; 32]) -> Result<Option<Attempt>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let record = self
            .snapshot
            .records
            .get(&hex::encode(id))
            .ok_or(Error::Unknown)?;
        if matches!(record.status, Status::Pending { .. }) {
            Ok(Some(record.attempt(&self.snapshot.metadata)?))
        } else {
            Ok(None)
        }
    }
}
