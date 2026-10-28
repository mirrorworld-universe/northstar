use {
    anyhow::{anyhow, bail, Context, Result},
    serde::{Deserialize, Serialize},
    std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
};

pub const DEADLINE_MS: u64 = 120_000;
pub const MAINNET_RPC_URL: &str = "https://rpc.mainnet.succinct.xyz";

pub fn endpoint(mode: &str, rpc_url: Option<&str>) -> Result<(String, bool)> {
    match (mode, rpc_url) {
        ("auction", None) => Ok((MAINNET_RPC_URL.to_string(), false)),
        ("local-hosted", Some(url)) => {
            let address: std::net::SocketAddr = url
                .strip_prefix("http://")
                .context("local-hosted endpoint must use http:// with a numeric loopback address")?
                .parse()?;
            if !address.ip().is_loopback() || address.port() == 0 {
                bail!("local-hosted endpoint must be loopback with a nonzero port");
            }
            Ok((url.to_string(), true))
        }
        _ => bail!("use auction with no RPC override, or local-hosted with a loopback RPC URL"),
    }
}

pub fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

pub fn remaining(started_ms: u64, now_ms: u64) -> Result<Duration> {
    let elapsed = now_ms
        .checked_sub(started_ms)
        .ok_or_else(|| anyhow!("network request clock moved backwards"))?;
    if elapsed >= DEADLINE_MS {
        bail!("network request deadline expired; no automatic resubmission");
    }
    Ok(Duration::from_millis(DEADLINE_MS - elapsed))
}

pub fn private_directory(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!("network state directory must be absolute");
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("network state directory must be private and not a symlink");
    }
    Ok(())
}

pub fn private_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("network file must be private and regular");
    }
    Ok(file)
}

pub fn read_private_key(path: &Path) -> Result<String> {
    if !path.is_absolute() {
        bail!("network private key path must be absolute");
    }
    let mut encoded = String::new();
    private_file(path)?.take(129).read_to_string(&mut encoded)?;
    let key = encoded.trim().strip_prefix("0x").unwrap_or(encoded.trim());
    if encoded.len() > 128 || key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid network private key file");
    }
    Ok(key.to_string())
}

pub fn positive_limit(name: &str, value: &str, maximum: u64) -> Result<u64> {
    let parsed = value
        .parse::<u64>()
        .with_context(|| format!("invalid {name}"))?;
    if parsed == 0 || parsed > maximum {
        bail!("{name} must be in 1..={maximum}");
    }
    Ok(parsed)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub witness_sha256: String,
    pub program_vkey_hash: String,
    pub requester: String,
    pub started_ms: u64,
    pub request_id: Option<String>,
    pub completed_ms: Option<u64>,
}

impl Request {
    pub fn submitted_id(&self) -> Result<&str> {
        self.request_id
            .as_deref()
            .context("network submission outcome unknown; reconcile request before retrying")
    }

    pub fn validate(&self, witness: &str, program: &str, requester: &str) -> Result<()> {
        if self.witness_sha256 != witness
            || self.program_vkey_hash != program
            || self.requester != requester
        {
            bail!("network request binding mismatch");
        }
        if let Some(completed) = self.completed_ms {
            let elapsed = completed.checked_sub(self.started_ms);
            if !matches!(elapsed, Some(1..=DEADLINE_MS)) || self.request_id.is_none() {
                bail!("invalid completed network request");
            }
        }
        Ok(())
    }
}

pub struct Journal {
    pub directory: PathBuf,
    _lock: File,
}

impl Journal {
    pub fn open(directory: PathBuf) -> Result<Self> {
        private_directory(&directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("lock"))?;
        lock.try_lock()
            .context("network request already in progress")?;
        File::open(directory.parent().context("network state parent missing")?)?.sync_all()?;
        Ok(Self {
            directory,
            _lock: lock,
        })
    }

    pub fn load(&self) -> Result<Option<Request>> {
        let path = self.directory.join("request.json");
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let mut bytes = Vec::new();
                private_file(&path)?.take(4097).read_to_end(&mut bytes)?;
                if bytes.len() > 4096 {
                    bail!("network request journal too large");
                }
                Ok(Some(serde_json::from_slice(&bytes)?))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn store(&self, request: &Request) -> Result<()> {
        let temporary = self.directory.join("request.tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(request)?)?;
        file.sync_all()?;
        fs::rename(temporary, self.directory.join("request.json"))?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            witness_sha256: "witness".into(),
            program_vkey_hash: "program".into(),
            requester: "requester".into(),
            started_ms: 1_000,
            request_id: None,
            completed_ms: None,
        }
    }

    #[test]
    fn local_hosted_endpoint_cannot_select_public_services() {
        assert_eq!(
            endpoint("auction", None).unwrap(),
            (MAINNET_RPC_URL.into(), false)
        );
        assert!(
            endpoint("local-hosted", Some("http://127.0.0.1:50051"))
                .unwrap()
                .1
        );
        assert!(
            endpoint("local-hosted", Some("http://[::1]:50051"))
                .unwrap()
                .1
        );
        for url in [
            "https://rpc.mainnet.succinct.xyz",
            "http://54.201.147.177:50051",
            "http://0.0.0.0:50051",
            "http://127.0.0.1:0",
            "http://user@127.0.0.1:50051",
            "http://127.0.0.1:50051/path",
        ] {
            assert!(endpoint("local-hosted", Some(url)).is_err(), "{url}");
        }
        assert!(endpoint("auction", Some("http://127.0.0.1:50051")).is_err());
        assert!(endpoint("local-hosted", None).is_err());
    }

    #[test]
    fn deadline_never_restarts_on_retry() {
        assert_eq!(
            remaining(1000, 1001).unwrap(),
            Duration::from_millis(119_999)
        );
        assert!(remaining(1000, 121_000).is_err());
        assert!(remaining(1000, 999).is_err());
    }

    #[test]
    fn limits_reject_zero_overflow_and_invalid_values() {
        for value in ["0", "-1", "1.5", "18446744073709551616", "1001"] {
            assert!(positive_limit("limit", value, 1000).is_err());
        }
        assert_eq!(positive_limit("limit", "1000", 1000).unwrap(), 1000);
    }

    #[test]
    fn private_key_permissions_and_type_are_checked() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("key");
        fs::write(&path, format!("0x{}\n", "12".repeat(32))).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_private_key(&path).unwrap(), "12".repeat(32));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private_key(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_private_key(&link).is_err());
        fs::write(&path, "not a key").unwrap();
        assert!(read_private_key(&path).is_err());
        assert!(read_private_key(Path::new("relative")).is_err());
    }

    #[test]
    fn submission_intent_and_request_id_survive_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("job");
        let journal = Journal::open(path.clone()).unwrap();
        assert!(journal.load().unwrap().is_none());
        let mut state = request();
        journal.store(&state).unwrap();
        drop(journal);
        let journal = Journal::open(path.clone()).unwrap();
        let loaded = journal.load().unwrap().unwrap();
        assert!(loaded.submitted_id().is_err());
        assert_eq!(loaded.started_ms, state.started_ms);
        state.request_id = Some("request-id".into());
        journal.store(&state).unwrap();
        drop(journal);
        assert_eq!(
            Journal::open(path)
                .unwrap()
                .load()
                .unwrap()
                .unwrap()
                .request_id,
            state.request_id
        );
    }

    #[test]
    fn concurrent_requests_are_rejected_and_lock_releases() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("job");
        let journal = Journal::open(path.clone()).unwrap();
        assert!(Journal::open(path.clone()).is_err());
        drop(journal);
        assert!(Journal::open(path).is_ok());
    }

    #[test]
    fn request_binding_and_completion_are_checked() {
        let mut state = request();
        state.validate("witness", "program", "requester").unwrap();
        for (witness, program, requester) in [
            ("other", "program", "requester"),
            ("witness", "other", "requester"),
            ("witness", "program", "other"),
        ] {
            assert!(state.validate(witness, program, requester).is_err());
        }
        state.completed_ms = Some(1001);
        assert!(state.validate("witness", "program", "requester").is_err());
        state.request_id = Some("id".into());
        state.validate("witness", "program", "requester").unwrap();
        for completed in [999, 1000, 121_001] {
            state.completed_ms = Some(completed);
            assert!(state.validate("witness", "program", "requester").is_err());
        }
    }

    #[test]
    fn public_state_directory_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Journal::open(directory.path().to_path_buf()).is_err());
    }
}
