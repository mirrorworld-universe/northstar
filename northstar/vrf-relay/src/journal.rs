use {
    anyhow::{Context, Result, ensure},
    serde::{Deserialize, Serialize},
    solana_pubkey::Pubkey,
    std::{
        collections::BTreeMap,
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
    },
};
const MAX_BYTES: u64 = 16 * 1024 * 1024;
#[cfg(test)]
mod tests {
    use {
        super::*,
        std::{sync::mpsc, thread, time::Duration},
    };
    #[test]
    fn concurrent_first_opens_leave_one_usable_journal() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let (ready, wait_ready) = mpsc::channel();
        let (resume_a, wait_a) = mpsc::channel();
        let first_path = directory.clone();
        let first = thread::spawn(move || {
            Journal::open_inner(&first_path, "binding", 10, |step| {
                if step == LockStep::Opened {
                    ready.send(()).unwrap();
                    wait_a.recv_timeout(Duration::from_secs(5)).unwrap();
                }
            })
        });
        wait_ready.recv_timeout(Duration::from_secs(5)).unwrap();
        let (progress, wait_progress) = mpsc::channel();
        let (resume_b, wait_b) = mpsc::channel();
        let second_path = directory.clone();
        let second = thread::spawn(move || {
            let result = Journal::open_inner(&second_path, "binding", 10, |step| {
                if step == LockStep::Acquired {
                    progress.send(true).unwrap();
                    wait_b.recv_timeout(Duration::from_secs(5)).unwrap();
                }
            });
            if result.is_err() {
                let _ = progress.send(false);
            }
            result
        });
        let second_locked = wait_progress.recv_timeout(Duration::from_secs(5)).unwrap();
        resume_a.send(()).unwrap();
        let a = first.join().unwrap();
        if second_locked {
            resume_b.send(()).unwrap();
        }
        let b = second.join().unwrap();
        let initialized = a.is_ok() || b.is_ok();
        drop(a);
        drop(b);
        assert!(initialized, "both cold-open contenders failed");
        Journal::open(&directory, "binding", 10).unwrap();
    }
    #[test]
    fn writer_drop_releases_owned_locks_with_duplicate_descriptors() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let writer = Journal::open(&directory, "binding", 10).unwrap();
        let _file_copy = writer._lock.try_clone().unwrap();
        let _directory_copy = writer._directory_lock.try_clone().unwrap();
        assert!(Journal::open(&directory, "binding", 10).is_err());
        drop(writer);
        let resumed = Journal::open(&directory, "binding", 10).unwrap();
        assert!(Journal::open(&directory, "binding", 10).is_err());
        drop(resumed);
        Journal::open(&directory, "binding", 10).unwrap();
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Retryable,
    Confirmed,
    Expired,
    Exhausted,
    Unresolved,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub signature: String,
    pub wire_base64: String,
    pub last_valid_block_height: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    #[serde(with = "crate::serde_keys::single")]
    pub address: Pubkey,
    pub request_base64: String,
    pub proof_hex: String,
    pub output_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_receipt: Option<crate::remote_prover::SigningReceipt>,
    pub attempts: u32,
    pub status: Status,
    pub attempt: Attempt,
    pub confirmed_slot: Option<u64>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub binding: String,
    pub records: BTreeMap<String, Record>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub l1_discovery_established: bool,
}
fn is_false(value: &bool) -> bool {
    !*value
}
pub struct Journal {
    _lock: File,
    _directory_lock: File,
    directory: PathBuf,
    snapshot: Snapshot,
    max_records: usize,
    poisoned: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum LockStep {
    Opened,
    Acquired,
}
impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self._lock.unlock();
        let _ = self._directory_lock.unlock();
    }
}
impl Journal {
    pub fn open(directory: &Path, binding: &str, max_records: usize) -> Result<Self> {
        Self::open_inner(directory, binding, max_records, |_| {})
    }
    fn open_inner(
        directory: &Path,
        binding: &str,
        max_records: usize,
        mut observe: impl FnMut(LockStep),
    ) -> Result<Self> {
        if !directory.exists() {
            fs::DirBuilder::new().mode(0o700).create(directory)?;
            let parent = directory
                .parent()
                .filter(|path| !path.as_os_str().is_empty());
            File::open(parent.unwrap_or(Path::new(".")))?.sync_all()?;
        }
        let metadata = fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
            "state directory must be private (0700), not a symlink"
        );
        let directory_lock = File::open(directory)?;
        directory_lock
            .try_lock()
            .context("journal directory already locked or locking unsupported")?;
        let lock_path = directory.join("writer.lock");
        let created = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path);
        let (lock, new) = match created {
            Ok(file) => (file, true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&lock_path)?,
                false,
            ),
            Err(error) => return Err(error.into()),
        };
        ensure!(lock.metadata()?.is_file(), "invalid journal lock");
        observe(LockStep::Opened);
        lock.try_lock()
            .context("journal already locked or locking unsupported")?;
        observe(LockStep::Acquired);
        let path = directory.join("public-state.json");
        let snapshot = if new {
            ensure!(
                !path.exists(),
                "snapshot exists without established lock; inspect state manually"
            );
            Snapshot {
                version: 1,
                binding: binding.to_owned(),
                records: BTreeMap::new(),
                l1_discovery_established: false,
            }
        } else {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)
                .context("established snapshot missing; refusing reset")?;
            ensure!(file.metadata()?.is_file(), "snapshot is not regular");
            let mut bytes = Vec::new();
            file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() as u64 <= MAX_BYTES, "journal too large");
            serde_json::from_slice(&bytes)
                .context("invalid established snapshot; refusing reset")?
        };
        ensure!(
            snapshot.version == 1
                && snapshot.binding == binding
                && snapshot.records.len() <= max_records,
            "incompatible journal binding or capacity"
        );
        let mut journal = Self {
            _lock: lock,
            _directory_lock: directory_lock,
            directory: directory.to_owned(),
            snapshot,
            max_records,
            poisoned: false,
        };
        if new {
            journal.change(|_| Ok(()))?;
        }
        Ok(journal)
    }
    pub fn inspect(directory: &Path, binding: &str, max_records: usize) -> Result<Snapshot> {
        let metadata = fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
            "state directory must be private (0700), not a symlink"
        );
        let directory_lock = File::open(directory)?;
        directory_lock
            .try_lock()
            .context("journal directory already locked")?;
        let open = |name: &str| -> Result<File> {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(directory.join(name))?;
            ensure!(file.metadata()?.is_file(), "journal input is not regular");
            Ok(file)
        };
        let lock = open("writer.lock")?;
        lock.try_lock().context("journal already locked")?;
        let mut bytes = Vec::new();
        open("public-state.json")?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= MAX_BYTES, "journal too large");
        let snapshot: Snapshot =
            serde_json::from_slice(&bytes).context("invalid established snapshot")?;
        ensure!(
            snapshot.version == 1
                && snapshot.binding == binding
                && snapshot.records.len() <= max_records,
            "incompatible journal binding or capacity"
        );
        if snapshot.l1_discovery_established {
            crate::discovery::DiscoveryState::inspect(directory, binding, max_records)?;
        }
        Ok(snapshot)
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn change(&mut self, update: impl FnOnce(&mut Snapshot) -> Result<()>) -> Result<()> {
        ensure!(!self.poisoned, "journal is poisoned; reopen and reconcile");
        let mut next = self.snapshot.clone();
        update(&mut next)?;
        ensure!(
            next.binding == self.snapshot.binding
                && next.version == 1
                && (!self.snapshot.l1_discovery_established || next.l1_discovery_established)
                && next.records.len() <= self.max_records,
            "journal policy violation"
        );
        let bytes = serde_json::to_vec(&next)?;
        ensure!(bytes.len() as u64 <= MAX_BYTES, "journal capacity exceeded");
        let result = (|| -> Result<()> {
            let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            temporary.persist(self.directory.join("public-state.json"))?;
            File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result?;
        self.snapshot = next;
        Ok(())
    }
}
