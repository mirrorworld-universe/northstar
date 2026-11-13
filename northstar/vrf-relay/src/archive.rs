use {
    anyhow::{Context, Result, ensure},
    serde::{Deserialize, Serialize, de::DeserializeOwned},
    sha2::{Digest, Sha256},
    std::{
        collections::BTreeMap,
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
    },
};
const MAX_ENTRY_BYTES: u64 = 128 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveHead {
    pub count: u64,
    pub digest: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    version: u8,
    binding: String,
    id: String,
    previous: [u8; 32],
    data: serde_json::Value,
}
pub(crate) struct Archive {
    directory: PathBuf,
    owner: u32,
    binding: String,
    head: Option<ArchiveHead>,
    index: BTreeMap<String, ArchiveHead>,
}
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"northstar/vrf/retained-history/v1\0")
        .chain_update(bytes)
        .finalize()
        .into()
}
fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
impl Archive {
    pub fn open(directory: &Path, binding: &str, head: Option<ArchiveHead>) -> Result<Self> {
        let mut archive = Self {
            directory: directory.join("retained-history"),
            owner: fs::symlink_metadata(directory)?.uid(),
            binding: binding.into(),
            head: None,
            index: BTreeMap::new(),
        };
        if let Some(expected) = head {
            ensure!(expected.count > 0, "invalid retained-history head");
            let mut previous = [0; 32];
            for sequence in 1..=expected.count {
                let bytes = archive.read(sequence)?;
                let entry: Entry = serde_json::from_slice(&bytes)?;
                ensure!(
                    entry.version == 1
                        && entry.binding == binding
                        && valid_id(&entry.id)
                        && entry.previous == previous
                        && !archive.index.contains_key(&entry.id),
                    "retained history binding or chain differs"
                );
                previous = digest(&bytes);
                archive.index.insert(
                    entry.id,
                    ArchiveHead {
                        count: sequence,
                        digest: previous,
                    },
                );
            }
            ensure!(
                previous == expected.digest,
                "retained history digest differs"
            );
            archive.head = Some(expected);
        }
        Ok(archive)
    }
    fn check_directory(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.directory)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == self.owner
                && metadata.permissions().mode() & 0o077 == 0,
            "retained history must be a private owned directory"
        );
        Ok(())
    }
    fn path(&self, sequence: u64) -> PathBuf {
        self.directory.join(format!("{sequence:020}.json"))
    }
    fn read(&self, sequence: u64) -> Result<Vec<u8>> {
        self.check_directory()?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.path(sequence))
            .context("retained history entry unavailable; refusing reset")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == self.owner
                && metadata.nlink() == 1
                && metadata.permissions().mode() & 0o077 == 0,
            "invalid retained history entry"
        );
        let mut bytes = Vec::new();
        file.take(MAX_ENTRY_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_ENTRY_BYTES,
            "retained history entry exceeds bounds"
        );
        Ok(bytes)
    }
    pub fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.index.keys().map(String::as_str)
    }
    pub fn count(&self) -> u64 {
        self.head.as_ref().map_or(0, |head| head.count)
    }
    pub fn get<T: DeserializeOwned>(&self, id: &str) -> Result<Option<T>> {
        let Some(location) = self.index.get(id) else {
            return Ok(None);
        };
        let bytes = self.read(location.count)?;
        ensure!(
            digest(&bytes) == location.digest,
            "retained history entry changed"
        );
        let entry: Entry = serde_json::from_slice(&bytes)?;
        ensure!(
            entry.id == id && entry.binding == self.binding && entry.version == 1,
            "retained history identity differs"
        );
        Ok(Some(serde_json::from_value(entry.data)?))
    }
    pub fn stage(&self, id: &str, data: &impl Serialize) -> Result<ArchiveHead> {
        ensure!(
            valid_id(id) && !self.contains(id),
            "invalid or repeated retained history identity"
        );
        let sequence = self
            .count()
            .checked_add(1)
            .context("retained history sequence exhausted")?;
        let entry = Entry {
            version: 1,
            binding: self.binding.clone(),
            id: id.into(),
            previous: self.head.as_ref().map_or([0; 32], |head| head.digest),
            data: serde_json::to_value(data)?,
        };
        let bytes = serde_json::to_vec(&entry)?;
        ensure!(
            bytes.len() as u64 <= MAX_ENTRY_BYTES,
            "retained history entry exceeds bounds"
        );
        match fs::DirBuilder::new().mode(0o700).create(&self.directory) {
            Ok(()) => File::open(self.directory.parent().context("history parent missing")?)?
                .sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        self.check_directory()?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(self.path(sequence)) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    self.read(sequence)? == bytes,
                    "uncommitted history differs; reconciliation required"
                );
            }
            Err(error) => return Err(error.into()),
        }
        File::open(&self.directory)?.sync_all()?;
        Ok(ArchiveHead {
            count: sequence,
            digest: digest(&bytes),
        })
    }
    pub fn commit(&mut self, id: String, head: ArchiveHead) {
        self.index.insert(id, head.clone());
        self.head = Some(head);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_snapshot_publication_accepts_only_identical_history() {
        let directory = tempfile::tempdir().unwrap();
        let id = "01".repeat(32);
        let archive = Archive::open(directory.path(), "binding", None).unwrap();
        let expected = archive.stage(&id, &"original").unwrap();
        drop(archive);
        let mut reopened = Archive::open(directory.path(), "binding", None).unwrap();
        assert!(!reopened.contains(&id));
        assert!(reopened.stage(&id, &"changed").is_err());
        assert_eq!(reopened.stage(&id, &"original").unwrap(), expected);
        reopened.commit(id.clone(), expected.clone());
        assert_eq!(reopened.get::<String>(&id).unwrap().unwrap(), "original");
        drop(reopened);
        assert!(Archive::open(directory.path(), "other", Some(expected.clone())).is_err());
        let reopened = Archive::open(directory.path(), "binding", Some(expected.clone())).unwrap();
        fs::write(
            directory
                .path()
                .join("retained-history/00000000000000000001.json"),
            b"{}",
        )
        .unwrap();
        assert!(reopened.get::<String>(&id).is_err());
        assert!(Archive::open(directory.path(), "binding", Some(expected)).is_err());
    }
    #[test]
    fn history_rejects_symlinks_and_public_permissions() {
        for symlink in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let archive = Archive::open(directory.path(), "binding", None).unwrap();
            let id = "01".repeat(32);
            let head = archive.stage(&id, &"retained").unwrap();
            let path = directory
                .path()
                .join("retained-history/00000000000000000001.json");
            if symlink {
                let target = directory.path().join("original");
                fs::rename(&path, &target).unwrap();
                std::os::unix::fs::symlink(&target, &path).unwrap();
            } else {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            }
            assert!(Archive::open(directory.path(), "binding", Some(head)).is_err());
        }
    }
}
