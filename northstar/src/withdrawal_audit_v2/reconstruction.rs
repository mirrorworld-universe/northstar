//! Offline reconstruction of one pre-transaction cut in the first isolated ER slot.
//! No live bank, checkpoint writes, simulated blockhashes, or admission hooks.
use {
    super::{audit, signed_transaction, AuditError, AuditPins, Unavailable, VerifiedStep},
    crate::{
        checkpoint::MAX_TRANSACTION_BYTES_V1,
        checkpoint_v2::CheckpointArtifactV2,
        continuity_v2::{Hash32, TransitionPayloadV2},
        EphemeralRuntime, DEFAULT_ER_SLOT_DURATION,
    },
    agave_snapshots::{
        snapshot_archive_info::{FullSnapshotArchiveInfo, SnapshotArchiveInfoGetter},
        snapshot_config::SnapshotConfig,
    },
    solana_accounts_db::accounts_db::AccountsDbConfig,
    solana_fee_structure::FeeStructure,
    solana_genesis_config::GenesisConfig,
    solana_leader_schedule::SlotLeader,
    solana_message::VersionedMessage,
    solana_runtime::{
        bank::Bank, bank_forks::BankForks, runtime_config::RuntimeConfig,
        snapshot_bank_utils::bank_from_snapshot_archives,
    },
    solana_sha256_hasher::Hasher,
    solana_svm::transaction_processor::ExecutionRecordingConfig,
    solana_svm_timings::ExecuteTimings,
    solana_transaction::versioned::VersionedTransaction,
    std::{
        collections::HashSet,
        fs::{self, File},
        io::{Read, Write},
        path::{Path, PathBuf},
        sync::Arc,
    },
};

pub const MAX_PREFIX_TRANSACTIONS: usize = 256;
const CUT_DOMAIN: &[u8] = b"northstar:v2:cpi-audit:first-er-slot:cut:v1";

/// Obtain these from a trusted source independent of the checkpoint producer. The archive
/// digest covers the compressed file, including serialized bank fields and the status cache.
#[derive(Debug, Clone, Copy)]
pub struct ReplayPins {
    pub genesis_hash: Hash32,
    pub archive_sha256: Hash32,
    pub archive_bytes: u64,
    pub parent_slot: u64,
    pub parent_bank_hash: Hash32,
    pub ordered_cut: Hash32,
}

/// Domain, little-endian u32 prefix count, then length-prefixed (u32 LE) canonical signed
/// transaction bytes, in order. The last frame is the target: it is *not* committed.
pub fn ordered_cut_digest(prefix: &[Vec<u8>], target: &[u8]) -> Result<Hash32, AuditError> {
    if prefix.len() > MAX_PREFIX_TRANSACTIONS {
        return Err(Unavailable("ordered prefix too long"));
    }
    let mut h = Hasher::default();
    h.hash(CUT_DOMAIN);
    h.hash(&(prefix.len() as u32).to_le_bytes());
    for bytes in prefix.iter().map(Vec::as_slice).chain([target]) {
        if bytes.is_empty() || bytes.len() > MAX_TRANSACTION_BYTES_V1 {
            return Err(Unavailable("transaction frame outside cut bounds"));
        }
        h.hash(&(bytes.len() as u32).to_le_bytes());
        h.hash(bytes);
    }
    Ok(h.result().to_bytes())
}

fn private_archive(
    source: &Path,
    dir: &Path,
    pins: &ReplayPins,
) -> Result<FullSnapshotArchiveInfo, AuditError> {
    let name = source
        .file_name()
        .ok_or(Unavailable("archive filename missing"))?;
    let info = FullSnapshotArchiveInfo::new_from_path(dir.join(name))
        .map_err(|_| Unavailable("expected a named Agave full snapshot archive"))?;
    if info.slot() != pins.parent_slot {
        return Err(Unavailable("full archive slot does not match parent pin"));
    }
    let mut input = File::open(source).map_err(|_| Unavailable("cannot open full archive"))?;
    let meta = input
        .metadata()
        .map_err(|_| Unavailable("archive metadata unavailable"))?;
    if !meta.is_file() || meta.len() != pins.archive_bytes || pins.archive_bytes == 0 {
        return Err(Unavailable("full archive size does not match pin"));
    }
    // The loader reopens by path. Hash exactly the private copy that it will open, not a
    // possibly changing source file. Limit copying even if the source grows after stat.
    let mut private =
        File::create_new(info.path()).map_err(|_| Unavailable("cannot stage archive"))?;
    let mut remaining = pins.archive_bytes;
    let mut h = Hasher::default();
    let mut buf = vec![0; 64 * 1024];
    while remaining != 0 {
        let n = remaining.min(buf.len() as u64) as usize;
        input
            .read_exact(&mut buf[..n])
            .map_err(|_| Unavailable("incomplete full archive"))?;
        private
            .write_all(&buf[..n])
            .map_err(|_| Unavailable("cannot stage archive"))?;
        h.hash(&buf[..n]);
        remaining -= n as u64;
    }
    if input.read(&mut buf[..1]).ok() != Some(0) || h.result().to_bytes() != pins.archive_sha256 {
        return Err(Unavailable("full archive digest does not match pin"));
    }
    Ok(info)
}

fn restore(
    genesis: &GenesisConfig,
    archive: &FullSnapshotArchiveInfo,
    workspace: &Path,
) -> Result<Bank, AuditError> {
    let directory = |name: &str| -> Result<PathBuf, AuditError> {
        let path = workspace.join(name);
        fs::create_dir(&path).map_err(|_| Unavailable("cannot create restore directory"))?;
        Ok(path)
    };
    let accounts = directory("accounts")?;
    let config = SnapshotConfig {
        bank_snapshots_dir: directory("bank")?,
        use_direct_io: false,
        use_registered_io_uring_buffers: false,
        ..SnapshotConfig::new_load_only()
    };
    // Upstream also uses assertions for inconsistent snapshot state. Never substitute a
    // partially restored bank or a load-slot limit when full verification fails.
    std::panic::catch_unwind(|| {
        bank_from_snapshot_archives(
            &[accounts],
            archive,
            None,
            &config,
            genesis,
            &RuntimeConfig::default(),
            None,
            None,
            None,
            true,
            AccountsDbConfig {
                skip_initial_hash_calc: false,
                verify_index: true,
                ..Default::default()
            },
            None,
            Arc::default(),
        )
    })
    .map_err(|_| Unavailable("full snapshot verification failed"))?
    .map_err(|_| Unavailable("full snapshot restore failed"))
}

/// Restore a private full snapshot, commit each prefix transaction, freeze, compare the
/// resulting bank with the externally pinned cut, and only then run the read-only auditor.
/// `scratch` must be an auditor-controlled local directory with space for a full restore.
/// Only the default-duration, zero-fee, single ER root slot without external writes or ticks
/// is supported. A missing input or unsupported history yields no independent verdict.
pub fn audit_from_full_snapshot(
    genesis: &GenesisConfig,
    full_archive: &Path,
    scratch: &Path,
    prefix: &[Vec<u8>],
    replay: &ReplayPins,
    artifact: &CheckpointArtifactV2,
    pins: &AuditPins,
) -> Result<VerifiedStep, AuditError> {
    let page = artifact
        .pages
        .get(pins.step as usize)
        .ok_or(Unavailable("step absent"))?;
    let TransitionPayloadV2::Execution { transaction, .. } = &page.transition.payload else {
        return Err(Unavailable("not an execution step"));
    };
    if page.index != pins.step
        || page.transition.er_slot != pins.bank_slot
        || page.pre_state.identity().l1_genesis != replay.genesis_hash
        || genesis.hash().to_bytes() != replay.genesis_hash
    {
        return Err(Unavailable("genesis or selected cut does not match pins"));
    }
    if ordered_cut_digest(prefix, transaction)? != replay.ordered_cut {
        return Err(Unavailable(
            "ordered prefix or target does not match cut pin",
        ));
    }
    let target = signed_transaction(transaction).map_err(Unavailable)?;
    let mut seen = HashSet::from([target.signatures[0]]);
    let txs = prefix
        .iter()
        .map(|bytes| {
            let tx = signed_transaction(bytes).map_err(Unavailable)?;
            if !matches!(
                tx.message,
                VersionedMessage::Legacy(_) | VersionedMessage::V0(_)
            ) {
                return Err(Unavailable("unsupported prefix message version"));
            }
            if crate::ephemeral_tx_client::EphemeralTransactionClient::batch_uses_upgradeable_loader(std::slice::from_ref(&tx)) {
                return Err(Unavailable("prefix requires an ER bank rotation"));
            }
            if !seen.insert(tx.signatures[0]) {
                return Err(Unavailable(
                    "prefix repeats a signature or includes the target",
                ));
            }
            Ok(tx)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let work = tempfile::tempdir_in(scratch)
        .map_err(|_| Unavailable("private restore directory unavailable"))?;
    let archive = private_archive(full_archive, work.path(), replay)?;
    let parent = restore(genesis, &archive, work.path())?;
    if !parent.is_frozen()
        || parent.slot() != replay.parent_slot
        || parent.hash().to_bytes() != replay.parent_bank_hash
    {
        return Err(Unavailable("restored parent does not match bank pin"));
    }
    if EphemeralRuntime::er_slot_for(&parent) != pins.bank_slot
        || pins
            .bank_slot
            .checked_add(1)
            .and_then(|n| n.checked_mul(parent.ticks_per_slot()))
            .is_none()
    {
        return Err(Unavailable("cut outside supported first ER slot"));
    }
    let mut child = Bank::new_from_parent_ephemeral_isolated(
        Arc::new(parent),
        SlotLeader::default(),
        pins.bank_slot,
    );
    child.configure_er(
        &FeeStructure {
            lamports_per_signature: 0,
            lamports_per_write_lock: 0,
            compute_fee_bins: vec![],
        },
        crate::er_recent_blockhash_max_age_for_slot_duration(DEFAULT_ER_SLOT_DURATION),
    );
    EphemeralRuntime::prepare_initial_working_bank(&child);
    let forks = BankForks::new_rw_arc_ephemeral(child);
    let bank = forks.read().unwrap().root_bank();
    let recent = |tx: &VersionedTransaction| {
        bank.get_hash_age(tx.message.recent_blockhash())
            .is_some_and(|age| age <= crate::DEFAULT_ER_TRANSACTION_MAX_AGE as u64)
    };
    for tx in txs {
        if !recent(&tx) {
            return Err(Unavailable(
                "prefix requires an unavailable recent blockhash",
            ));
        }
        let batch = bank
            .prepare_entry_batch([tx])
            .map_err(|_| Unavailable("cannot prepare prefix transaction"))?;
        let (results, _) = bank.load_execute_and_commit_transactions(
            &batch,
            ExecutionRecordingConfig::new_single_setting(false),
            &mut ExecuteTimings::default(),
            None,
        );
        if !matches!(results.as_slice(), [Ok(c)] if c.status.is_ok() && c.fee_details.total_fee() == 0)
        {
            return Err(Unavailable(
                "prefix transaction did not commit successfully at zero fee",
            ));
        }
    }
    if !recent(&target) {
        return Err(Unavailable(
            "target requires an unavailable recent blockhash",
        ));
    }
    if bank.has_signature(&target.signatures[0]) {
        return Err(Unavailable("target already present at reconstructed cut"));
    }
    bank.freeze();
    // audit() checks the independent cut hash before reading the checkpoint transcript.
    audit(Some(&bank), artifact, pins)
}
