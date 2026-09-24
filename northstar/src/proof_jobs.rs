//! Durable, bounded inputs/results for the opt-in proof coordinator.
use {
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_zk_types::{Sp1Groth16ProofV1, SP1_GROTH16_PROOF_V1_LEN},
    solana_sha256_hasher::hashv,
    std::{
        fs::{self, File, OpenOptions},
        io::{self, Read, Write},
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
        sync::Mutex,
    },
};

pub const MAX_PROOF_WITNESS_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PENDING_PROOF_JOBS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ProofJobBinding {
    pub portal: [u8; 32],
    pub session: [u8; 32],
    pub checkpoint: [u8; 32],
    pub challenge: [u8; 32],
    pub er_slot: u64,
    pub step: u32,
    pub public_inputs: [u8; 256],
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
struct Manifest {
    version: u8,
    binding: ProofJobBinding,
    witness_hash: [u8; 32],
    witness_len: u32,
}

pub struct ProofJob {
    pub id: [u8; 32],
    pub binding: ProofJobBinding,
    pub witness: Vec<u8>,
    pub proof: Option<[u8; SP1_GROTH16_PROOF_V1_LEN]>,
}

pub struct ProofJobStore {
    directory: PathBuf,
    _lease: File,
    writes: Mutex<()>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn name(id: &[u8; 32]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_bounded(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > maximum as u64 {
        return Err(invalid("proof artifact type or size"));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(invalid("proof artifact size"));
    }
    Ok(bytes)
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

impl ProofJobStore {
    pub fn open(directory: &Path) -> io::Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid("proof job directory must be private"));
        }
        let lease_path = directory.join(".lock");
        if fs::symlink_metadata(&lease_path).is_ok_and(|metadata| !metadata.is_file()) {
            return Err(invalid("proof job lease must be a regular file"));
        }
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lease_path)?;
        if lease.metadata()?.uid() != metadata.uid() {
            return Err(invalid("proof job directory owner mismatch"));
        }
        lease.try_lock().map_err(io::Error::other)?;
        let store = Self {
            directory: directory.to_owned(),
            _lease: lease,
            writes: Mutex::new(()),
        };
        // Only the lease holder may remove unpublished staging directories after restart.
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with(".stage-") {
                if !entry.file_type()?.is_dir() {
                    return Err(invalid("proof staging entry must be a directory"));
                }
                fs::remove_dir_all(entry.path())?;
            }
        }
        store.ids()?;
        Ok(store)
    }

    pub fn ids(&self) -> io::Result<Vec<[u8; 32]>> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if entry.file_name() == ".lock"
                || entry.file_name().to_string_lossy().starts_with(".stage-")
            {
                continue;
            }
            let text = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid("proof job name"))?;
            if text.len() != 64
                || !text
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || !entry.file_type()?.is_dir()
            {
                return Err(invalid("proof job entry"));
            }
            let mut id = [0; 32];
            for (index, byte) in id.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                    .map_err(|_| invalid("proof job name"))?;
            }
            ids.push(id);
            if ids.len() > MAX_PENDING_PROOF_JOBS {
                return Err(invalid("proof job capacity exceeded"));
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    pub fn enqueue(&self, binding: ProofJobBinding, witness: &[u8]) -> io::Result<[u8; 32]> {
        let _write = self
            .writes
            .lock()
            .map_err(|_| invalid("proof store lock poisoned"))?;
        if witness.is_empty() || witness.len() > MAX_PROOF_WITNESS_BYTES {
            return Err(invalid("proof witness size"));
        }
        let manifest = Manifest {
            version: 1,
            binding,
            witness_hash: hashv(&[witness]).to_bytes(),
            witness_len: witness.len() as u32,
        };
        let encoded = borsh::to_vec(&manifest)?;
        let id = hashv(&[b"northstar-proof-job-v1", &encoded]).to_bytes();
        let destination = self.directory.join(name(&id));
        if destination.exists() {
            self.load(&id)?;
            return Ok(id);
        }
        if self.ids()?.len() == MAX_PENDING_PROOF_JOBS {
            return Err(invalid("proof job capacity exceeded"));
        }
        let staging = tempfile::Builder::new()
            .prefix(".stage-")
            .tempdir_in(&self.directory)?;
        write_synced(&staging.path().join("manifest"), &encoded)?;
        write_synced(&staging.path().join("witness"), witness)?;
        File::open(staging.path())?.sync_all()?;
        fs::rename(staging.path(), destination)?;
        File::open(&self.directory)?.sync_all()?;
        Ok(id)
    }

    pub fn load(&self, id: &[u8; 32]) -> io::Result<ProofJob> {
        let directory = self.directory.join(name(id));
        if !fs::symlink_metadata(&directory)?.is_dir() {
            return Err(invalid("proof job must be a directory"));
        }
        let encoded = read_bounded(&directory.join("manifest"), 1024)?;
        if hashv(&[b"northstar-proof-job-v1", &encoded]).to_bytes() != *id {
            return Err(invalid("proof job binding mismatch"));
        }
        let manifest = Manifest::try_from_slice(&encoded)?;
        if manifest.version != 1
            || manifest.witness_len == 0
            || manifest.witness_len as usize > MAX_PROOF_WITNESS_BYTES
        {
            return Err(invalid("proof job manifest"));
        }
        let witness = read_bounded(&directory.join("witness"), MAX_PROOF_WITNESS_BYTES)?;
        if witness.len() != manifest.witness_len as usize
            || hashv(&[&witness]).to_bytes() != manifest.witness_hash
        {
            return Err(invalid("proof witness binding mismatch"));
        }
        let proof = match read_bounded(&directory.join("proof"), SP1_GROTH16_PROOF_V1_LEN + 32) {
            Ok(bytes) => {
                if bytes.len() != SP1_GROTH16_PROOF_V1_LEN + 32
                    || hashv(&[id, &bytes[..SP1_GROTH16_PROOF_V1_LEN]]).as_ref()
                        != &bytes[SP1_GROTH16_PROOF_V1_LEN..]
                {
                    return Err(invalid("proof result checksum mismatch"));
                }
                let proof: [u8; SP1_GROTH16_PROOF_V1_LEN] =
                    bytes[..SP1_GROTH16_PROOF_V1_LEN].try_into().unwrap();
                Sp1Groth16ProofV1::from_bytes(&proof).map_err(|_| invalid("proof envelope"))?;
                Some(proof)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(ProofJob {
            id: *id,
            binding: manifest.binding,
            witness,
            proof,
        })
    }

    /// Persist only results already verified by the configured prover. Storage checks do not
    /// replace cryptographic verification or reconciliation with authenticated L1 state.
    pub fn complete(
        &self,
        id: &[u8; 32],
        proof: &[u8],
        public_inputs: &[u8; 256],
    ) -> io::Result<()> {
        let _write = self
            .writes
            .lock()
            .map_err(|_| invalid("proof store lock poisoned"))?;
        let job = self.load(id)?;
        if &job.binding.public_inputs != public_inputs {
            return Err(invalid("proof public inputs mismatch"));
        }
        Sp1Groth16ProofV1::from_bytes(proof).map_err(|_| invalid("proof envelope"))?;
        let mut bytes = proof.to_vec();
        bytes.extend_from_slice(hashv(&[id, proof]).as_ref());
        let directory = self.directory.join(name(id));
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(directory.join("proof")) {
            Ok(_) => (),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                if read_bounded(&directory.join("proof"), bytes.len())? != bytes {
                    return Err(invalid("proof result is immutable"));
                }
            }
            Err(error) => return Err(error.error),
        }
        File::open(directory)?.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    fn binding() -> ProofJobBinding {
        ProofJobBinding {
            portal: [1; 32],
            session: [2; 32],
            checkpoint: [3; 32],
            challenge: [4; 32],
            er_slot: 5,
            step: 6,
            public_inputs: *include_bytes!(
                "../zkvm-replay/evidence/sp1-v6.8.0/northstar-sp1-public-inputs.bin"
            ),
        }
    }

    fn proof() -> Vec<u8> {
        include_bytes!("../zkvm-replay/evidence/sp1-v6.8.0/northstar-sp1-groth16-onchain.bin")
            .to_vec()
    }

    #[test]
    fn restart_preserves_queued_and_completed_jobs() {
        let directory = private_directory();
        let store = ProofJobStore::open(directory.path()).unwrap();
        let id = store.enqueue(binding(), b"witness").unwrap();
        assert_eq!(store.enqueue(binding(), b"witness").unwrap(), id);
        assert!(store.load(&id).unwrap().proof.is_none());
        drop(store);
        let store = ProofJobStore::open(directory.path()).unwrap();
        store
            .complete(&id, &proof(), &binding().public_inputs)
            .unwrap();
        store
            .complete(&id, &proof(), &binding().public_inputs)
            .unwrap();
        drop(store);
        let store = ProofJobStore::open(directory.path()).unwrap();
        assert_eq!(store.ids().unwrap(), vec![id]);
        let job = store.load(&id).unwrap();
        assert_eq!(job.binding, binding());
        assert_eq!(job.witness, b"witness");
        assert_eq!(job.proof.unwrap().as_slice(), proof());
    }

    #[test]
    fn exclusive_owner_and_private_directory_are_required() {
        let directory = private_directory();
        let store = ProofJobStore::open(directory.path()).unwrap();
        assert!(ProofJobStore::open(directory.path()).is_err());
        drop(store);
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ProofJobStore::open(directory.path()).is_err());
    }

    #[test]
    fn interrupted_publication_is_not_a_job() {
        let directory = private_directory();
        fs::create_dir(directory.path().join(".stage-incomplete")).unwrap();
        let store = ProofJobStore::open(directory.path()).unwrap();
        assert!(store.ids().unwrap().is_empty());
        assert!(!directory.path().join(".stage-incomplete").exists());
    }

    #[test]
    fn changed_witness_or_result_is_rejected_after_restart() {
        for artifact in ["witness", "proof", "manifest"] {
            let directory = private_directory();
            let store = ProofJobStore::open(directory.path()).unwrap();
            let id = store.enqueue(binding(), b"witness").unwrap();
            store
                .complete(&id, &proof(), &binding().public_inputs)
                .unwrap();
            drop(store);
            let path = directory.path().join(name(&id)).join(artifact);
            let mut bytes = fs::read(&path).unwrap();
            bytes[0] ^= 1;
            fs::write(path, bytes).unwrap();
            let store = ProofJobStore::open(directory.path()).unwrap();
            assert!(store.load(&id).is_err(), "{artifact}");
        }
    }

    #[test]
    fn completed_results_are_immutable() {
        let directory = private_directory();
        let store = ProofJobStore::open(directory.path()).unwrap();
        let id = store.enqueue(binding(), b"witness").unwrap();
        store
            .complete(&id, &proof(), &binding().public_inputs)
            .unwrap();
        let mut changed = proof();
        changed[355] ^= 1;
        assert!(store
            .complete(&id, &changed, &binding().public_inputs)
            .is_err());
        assert_eq!(store.load(&id).unwrap().proof.unwrap().as_slice(), proof());
    }

    #[test]
    fn capacity_is_bounded_without_replacing_existing_jobs() {
        let directory = private_directory();
        let store = ProofJobStore::open(directory.path()).unwrap();
        for index in 0..MAX_PENDING_PROOF_JOBS {
            let mut binding = binding();
            binding.challenge[0] = index as u8;
            store.enqueue(binding, b"witness").unwrap();
        }
        let mut additional = binding();
        additional.challenge[0] = MAX_PENDING_PROOF_JOBS as u8;
        assert!(store.enqueue(additional, b"witness").is_err());
        assert_eq!(store.ids().unwrap().len(), MAX_PENDING_PROOF_JOBS);
    }

    #[test]
    fn staging_symlinks_are_rejected_without_touching_other_directories() {
        let directory = private_directory();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("marker"), b"preserve").unwrap();
        std::os::unix::fs::symlink(other.path(), directory.path().join(".stage-link")).unwrap();
        assert!(ProofJobStore::open(directory.path()).is_err());
        assert_eq!(fs::read(other.path().join("marker")).unwrap(), b"preserve");
    }

    #[test]
    fn concurrent_enqueue_is_idempotent() {
        let directory = private_directory();
        let store = std::sync::Arc::new(ProofJobStore::open(directory.path()).unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.enqueue(binding(), b"witness").unwrap()
                })
            })
            .collect();
        let ids: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(ids.iter().all(|id| *id == ids[0]));
        assert_eq!(store.ids().unwrap(), vec![ids[0]]);
    }

    #[test]
    fn context_binding_and_witness_limits_are_enforced() {
        let directory = private_directory();
        let store = ProofJobStore::open(directory.path()).unwrap();
        assert!(store.enqueue(binding(), &[]).is_err());
        assert!(store
            .enqueue(binding(), &vec![0; MAX_PROOF_WITNESS_BYTES + 1])
            .is_err());
        let first = store.enqueue(binding(), b"witness").unwrap();
        let mut changed = binding();
        changed.step += 1;
        let second = store.enqueue(changed, b"witness").unwrap();
        assert_ne!(first, second);
        assert!(store.complete(&first, &proof(), &[1; 256]).is_err());
        assert!(store.load(&first).unwrap().proof.is_none());
        assert!(store
            .complete(
                &first,
                &[0; SP1_GROTH16_PROOF_V1_LEN],
                &binding().public_inputs
            )
            .is_err());
    }
}
