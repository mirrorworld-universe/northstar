//! Atomic snapshots for local checkpoint lineage and explicit restart recovery.
//! Callers must authenticate the supplied L1 head against a finalized Portal view.

use {
    crate::{
        checkpoint_v2::{build_checkpoint_artifact_v2, MAX_CHECKPOINT_TRANSITIONS_V2},
        continuity_v2::{hash_parts, CommittedStateV2, RecordedTransitionV2, TransitionPayloadV2},
    },
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_zk_types::continuity::SessionIdentityV2,
    std::{
        fs::{self, File, OpenOptions},
        io::{self, Read, Write},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
        path::{Path, PathBuf},
    },
};

const FORMAT_VERSION: u16 = 3;
const MAX_CAPSULE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SETTLEMENT_PLAN_BYTES: usize = 1024 * 1024;
const CAPSULE_FILE: &str = "lineage-v2.borsh";
const MARKER_FILE: &str = "established-v2";
const LOCK_FILE: &str = "lineage-v2.lock";

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct L1CheckpointHeadV2 {
    pub checkpoint: [u8; 32],
    pub er_slot: u64,
    pub state_root: [u8; 32],
    pub checkpoint_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SealedCheckpointV2 {
    pub checkpoint: [u8; 32],
    pub er_slot: u64,
    pub artifact: Vec<u8>,
    pub artifact_hash: [u8; 32],
    pub checkpoint_hash: [u8; 32],
    pub base: CommittedStateV2,
    pub transitions: Vec<RecordedTransitionV2>,
    pub settlement_plan: Option<Vec<u8>>,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
struct LineageCapsuleV2 {
    version: u16,
    sequence: u64,
    accepted_state: CommittedStateV2,
    accepted_head: Option<L1CheckpointHeadV2>,
    unsealed: Vec<RecordedTransitionV2>,
    sealed: Option<SealedCheckpointV2>,
    processed_transactions: std::collections::BTreeMap<[u8; 32], (u64, Vec<u8>)>,
}

impl LineageCapsuleV2 {
    fn working_state(&self) -> io::Result<CommittedStateV2> {
        if self.version != FORMAT_VERSION {
            return Err(invalid("unsupported lineage store version"));
        }
        if self.processed_transactions.len() > 65_536
            || self
                .processed_transactions
                .iter()
                .any(|(hash, (slot, wire))| {
                    *slot == 0
                        || wire.is_empty()
                        || wire.len() > 1_232
                        || hash_parts(&[wire]) != *hash
                })
        {
            return Err(invalid("invalid retained transaction cache"));
        }
        let mut state = self.accepted_state.clone();
        if let Some(sealed) = &self.sealed {
            if !self.unsealed.is_empty()
                || sealed.transitions.is_empty()
                || sealed.transitions.len() > MAX_CHECKPOINT_TRANSITIONS_V2
                || sealed.artifact.is_empty()
                || hash_parts(&[&sealed.artifact]) != sealed.artifact_hash
            {
                return Err(invalid("invalid sealed checkpoint capsule"));
            }
            let artifact: crate::checkpoint_v2::CheckpointArtifactV2 =
                borsh::from_slice(&sealed.artifact)?;
            artifact.validate().map_err(invalid)?;
            if artifact.binding_hash().map_err(invalid)? != sealed.checkpoint_hash
                || artifact.checkpoint.er_slot != sealed.er_slot
                || artifact
                    .pages
                    .first()
                    .is_none_or(|page| page.pre_state != sealed.base)
                || artifact.pages.len() != sealed.transitions.len()
                || artifact
                    .pages
                    .iter()
                    .zip(&sealed.transitions)
                    .any(|(page, transition)| page.transition != *transition)
            {
                return Err(invalid("sealed artifact differs from retained transitions"));
            }
            if let Some(plan) = &sealed.settlement_plan {
                decode_settlement_plan(plan, sealed.er_slot)?;
            }
            let mut result = sealed.base.clone();
            for transition in &sealed.transitions {
                transition.apply(&mut result).map_err(invalid)?;
            }
            if sealed.accepted {
                if result != state {
                    return Err(invalid("accepted checkpoint state differs"));
                }
            } else {
                if sealed.base != state {
                    return Err(invalid("sealed checkpoint base differs"));
                }
                state = result;
            }
        }
        if self.unsealed.len() > MAX_CHECKPOINT_TRANSITIONS_V2 {
            return Err(invalid("checkpoint transition limit exceeded"));
        }
        for transition in &self.unsealed {
            if self
                .accepted_head
                .as_ref()
                .is_some_and(|head| transition.er_slot <= head.er_slot)
            {
                return Err(invalid(
                    "checkpoint transition does not advance the accepted ER slot",
                ));
            }
            transition.apply(&mut state).map_err(invalid)?;
        }
        if let Some(last) = self.unsealed.last() {
            build_checkpoint_artifact_v2(&self.accepted_state, last.er_slot, &self.unsealed)
                .map_err(invalid)?;
        }
        if let Some(head) = &self.accepted_head {
            if self.accepted_state.root().map_err(invalid)? != head.state_root {
                return Err(invalid("accepted lineage head differs from its snapshot"));
            }
        } else if self.accepted_state
            != CommittedStateV2::new(*self.accepted_state.identity()).map_err(invalid)?
        {
            return Err(invalid("nonempty lineage state has no accepted L1 head"));
        }
        Ok(state)
    }
}

fn decode_settlement_plan(
    bytes: &[u8],
    er_slot: u64,
) -> io::Result<crate::settlement::SettlementPlan> {
    if bytes.is_empty() || bytes.len() > MAX_SETTLEMENT_PLAN_BYTES {
        return Err(invalid("settlement plan exceeds its encoding bound"));
    }
    let durable: crate::DurableSettlementPlan = borsh::from_slice(bytes)?;
    let plan = crate::settlement::SettlementPlan::from(durable);
    if plan.er_slot != er_slot || plan.recomputed_checksum() != plan.checksum {
        return Err(invalid("settlement plan checksum or slot differs"));
    }
    Ok(plan)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupModeV2 {
    InitializeNew,
    Resume,
}

pub struct CheckpointLineageStoreV2 {
    directory: PathBuf,
    capsule: LineageCapsuleV2,
    poisoned: bool,
    _lock: File,
}

impl CheckpointLineageStoreV2 {
    pub fn open(
        directory: &Path,
        identity: SessionIdentityV2,
        onchain_head: Option<L1CheckpointHeadV2>,
        mode: StartupModeV2,
    ) -> io::Result<Self> {
        let initial = CommittedStateV2::new(identity).map_err(invalid)?;
        match fs::symlink_metadata(directory) {
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                return Err(invalid("lineage directory is not a regular directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if mode == StartupModeV2::Resume {
                    return Err(error);
                }
                fs::DirBuilder::new().mode(0o700).create(directory)?;
                let parent = directory
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                File::open(parent)?.sync_all()?;
            }
            Err(error) => return Err(error),
        }
        let lock = private_options()
            .read(true)
            .write(true)
            .create(true)
            .open(directory.join(LOCK_FILE))?;
        if !lock.metadata()?.is_file() {
            return Err(invalid("lineage lock is not a regular file"));
        }
        lock.try_lock()
            .map_err(|error| io::Error::other(format!("lineage store is locked: {error}")))?;
        let marker = directory.join(MARKER_FILE);
        let snapshot = directory.join(CAPSULE_FILE);
        let established = path_exists(&marker)?;
        let has_snapshot = path_exists(&snapshot)?;
        if mode == StartupModeV2::InitializeNew
            && (established || has_snapshot || onchain_head.is_some())
        {
            return Err(invalid(
                "new lineage initialization cannot replace established state",
            ));
        }
        if mode == StartupModeV2::Resume && (!established || !has_snapshot) {
            return Err(invalid("established lineage state is missing"));
        }
        let capsule = if established && has_snapshot {
            let identity_hash = identity.commitment(hash_parts).map_err(invalid)?;
            if read_regular(&marker, 32)? != identity_hash {
                return Err(invalid("lineage identity differs"));
            }
            let bytes = read_regular(&snapshot, MAX_CAPSULE_BYTES)?;
            let (checksum, payload) = bytes
                .split_at_checked(32)
                .ok_or_else(|| invalid("truncated lineage capsule"))?;
            if hash_parts(&[payload]) != checksum {
                return Err(invalid("lineage checksum differs"));
            }
            let capsule = LineageCapsuleV2::try_from_slice(payload).map_err(invalid)?;
            if capsule.accepted_state.identity() != &identity {
                return Err(invalid("lineage session differs"));
            }
            if borsh::to_vec(&capsule).map_err(invalid)? != payload {
                return Err(invalid("noncanonical lineage capsule"));
            }
            capsule.working_state()?;
            capsule
        } else if established || has_snapshot || onchain_head.is_some() {
            return Err(invalid("established lineage state is missing"));
        } else {
            LineageCapsuleV2 {
                version: FORMAT_VERSION,
                sequence: 0,
                accepted_state: initial,
                accepted_head: None,
                unsealed: vec![],
                sealed: None,
                processed_transactions: std::collections::BTreeMap::new(),
            }
        };
        let mut store = Self {
            directory: directory.to_path_buf(),
            capsule,
            poisoned: false,
            _lock: lock,
        };
        if !established {
            store.install(&store.capsule.clone())?;
            let mut file = private_options()
                .write(true)
                .create_new(true)
                .open(&marker)?;
            file.write_all(&identity.commitment(hash_parts).map_err(invalid)?)?;
            file.sync_all()?;
            File::open(directory)?.sync_all()?;
        }
        if store.capsule.accepted_head != onchain_head {
            match onchain_head {
                Some(head) => store.record_accepted(head)?,
                None => return Err(invalid("established L1 checkpoint head is missing")),
            }
        }
        Ok(store)
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn pending_payout_events(
        &self,
    ) -> io::Result<(
        Vec<crate::settlement::WithdrawalPayoutEvent>,
        Vec<crate::settlement::TokenWithdrawalPayoutEvent>,
    )> {
        use crate::continuity_v2::SettlementEffectV2;
        let mut sol = Vec::new();
        let mut tokens = Vec::new();
        for transition in self.pending_transitions() {
            let wire = match &transition.payload {
                TransitionPayloadV2::Execution { transaction, .. }
                | TransitionPayloadV2::TokenIngress { transaction, .. } => transaction,
                TransitionPayloadV2::Ingress { .. } => continue,
            };
            let transaction: solana_transaction::versioned::VersionedTransaction =
                bincode::deserialize(wire).map_err(invalid)?;
            let signature = transaction
                .signatures
                .first()
                .copied()
                .ok_or_else(|| invalid("retained transaction has no signature"))?;
            for effect in &transition.settlement_effects {
                match borsh::from_slice::<SettlementEffectV2>(effect)? {
                    SettlementEffectV2::SolWithdrawal {
                        source,
                        recipient,
                        amount,
                        cumulative_withdrawn,
                    } => {
                        sol.push(crate::settlement::WithdrawalPayoutEvent {
                            er_source: solana_pubkey::Pubkey::new_from_array(source),
                            l1_recipient: solana_pubkey::Pubkey::new_from_array(recipient),
                            lamports: amount,
                            cumulative_withdrawn,
                            signature,
                            er_slot: transition.er_slot,
                        });
                    }
                    SettlementEffectV2::TokenWithdrawal {
                        bridge,
                        session_bridge,
                        account,
                        destination,
                        amount,
                        decimals,
                        ..
                    } => {
                        tokens.push(crate::settlement::TokenWithdrawalPayoutEvent {
                            bridge_program: solana_pubkey::Pubkey::new_from_array(bridge),
                            session_bridge: solana_pubkey::Pubkey::new_from_array(session_bridge),
                            er_token_account: solana_pubkey::Pubkey::new_from_array(account),
                            l1_destination_token_account: solana_pubkey::Pubkey::new_from_array(
                                destination,
                            ),
                            amount,
                            decimals,
                            signature,
                            er_slot: transition.er_slot,
                        });
                    }
                }
            }
        }
        Ok((sol, tokens))
    }

    pub fn accepted_state(&self) -> &CommittedStateV2 {
        &self.capsule.accepted_state
    }

    pub fn working_state(&self) -> io::Result<CommittedStateV2> {
        self.capsule.working_state()
    }
    pub fn sealed(&self) -> Option<&SealedCheckpointV2> {
        self.capsule.sealed.as_ref()
    }
    pub fn unsealed(&self) -> &[RecordedTransitionV2] {
        &self.capsule.unsealed
    }
    pub fn pending_transitions(&self) -> &[RecordedTransitionV2] {
        self.capsule
            .sealed
            .as_ref()
            .map_or(self.capsule.unsealed.as_slice(), |sealed| {
                sealed.transitions.as_slice()
            })
    }
    pub fn accepted_head(&self) -> Option<&L1CheckpointHeadV2> {
        self.capsule.accepted_head.as_ref()
    }
    pub fn processed_transactions(&self) -> impl Iterator<Item = (u64, &[u8])> {
        self.capsule
            .processed_transactions
            .values()
            .map(|(slot, wire)| (*slot, wire.as_slice()))
    }

    pub fn append(&mut self, transition: RecordedTransitionV2) -> io::Result<()> {
        self.append_batch(vec![transition])
    }

    pub fn append_batch(&mut self, transitions: Vec<RecordedTransitionV2>) -> io::Result<()> {
        if transitions.is_empty() {
            return Ok(());
        }
        if self.capsule.sealed.is_some()
            || self
                .capsule
                .unsealed
                .len()
                .checked_add(transitions.len())
                .is_none_or(|length| length > MAX_CHECKPOINT_TRANSITIONS_V2)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "checkpoint is sealed or full",
            ));
        }
        let mut state = self.working_state()?;
        let mut next = self.capsule.clone();
        for transition in transitions {
            transition.apply(&mut state).map_err(invalid)?;
            if let TransitionPayloadV2::Execution { transaction, .. }
            | TransitionPayloadV2::TokenIngress { transaction, .. } = &transition.payload
            {
                let hash = hash_parts(&[transaction]);
                if next.processed_transactions.contains_key(&hash) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "transaction already committed",
                    ));
                }
                if next.processed_transactions.len() >= 65_536 {
                    return Err(io::Error::other(
                        "processed transaction retention limit reached",
                    ));
                }
                next.processed_transactions
                    .insert(hash, (transition.er_slot, transaction.clone()));
            }
            next.unsealed.push(transition);
        }
        self.commit(next)
    }

    pub fn seal(
        &mut self,
        checkpoint: [u8; 32],
        er_slot: u64,
        artifact: Vec<u8>,
    ) -> io::Result<()> {
        if self.capsule.sealed.is_some()
            || self.capsule.unsealed.is_empty()
            || artifact.is_empty()
            || checkpoint == [0; 32]
            || er_slot == 0
            || self
                .capsule
                .accepted_head
                .as_ref()
                .is_some_and(|head| er_slot <= head.er_slot)
        {
            return Err(invalid("checkpoint cannot be sealed"));
        }
        let decoded: crate::checkpoint_v2::CheckpointArtifactV2 = borsh::from_slice(&artifact)?;
        decoded.validate().map_err(invalid)?;
        if decoded.checkpoint.er_slot != er_slot
            || decoded.checkpoint.session.to_bytes()
                != self.capsule.accepted_state.identity().session
        {
            return Err(invalid("sealed artifact identity differs"));
        }
        let mut next = self.capsule.clone();
        next.sealed = Some(SealedCheckpointV2 {
            checkpoint,
            er_slot,
            checkpoint_hash: decoded.binding_hash().map_err(invalid)?,
            artifact_hash: hash_parts(&[&artifact]),
            artifact,
            base: next.accepted_state.clone(),
            transitions: std::mem::take(&mut next.unsealed),
            settlement_plan: None,
            accepted: false,
        });
        self.commit(next)
    }

    pub fn bind_settlement_plan(
        &mut self,
        plan: &crate::settlement::SettlementPlan,
    ) -> io::Result<()> {
        let sealed = self
            .capsule
            .sealed
            .as_ref()
            .ok_or_else(|| invalid("settlement plan has no sealed checkpoint"))?;
        if plan.has_unsupported_changes() || plan.er_slot != sealed.er_slot {
            return Err(invalid(
                "settlement plan differs from the sealed checkpoint",
            ));
        }
        let bytes = borsh::to_vec(&crate::DurableSettlementPlan::from(plan))?;
        decode_settlement_plan(&bytes, sealed.er_slot)?;
        if let Some(existing) = &sealed.settlement_plan {
            if existing != &bytes {
                return Err(invalid("established settlement plan changed"));
            }
            return Ok(());
        }
        let mut next = self.capsule.clone();
        next.sealed
            .as_mut()
            .ok_or_else(|| invalid("sealed checkpoint missing"))?
            .settlement_plan = Some(bytes);
        self.commit(next)
    }

    pub fn settlement_plan(&self) -> io::Result<Option<crate::settlement::SettlementPlan>> {
        self.working_state()?;
        self.capsule
            .sealed
            .as_ref()
            .and_then(|sealed| {
                sealed
                    .settlement_plan
                    .as_ref()
                    .map(|bytes| decode_settlement_plan(bytes, sealed.er_slot))
            })
            .transpose()
    }

    pub fn record_accepted(&mut self, head: L1CheckpointHeadV2) -> io::Result<()> {
        if self.capsule.accepted_head.as_ref() == Some(&head) {
            return Ok(());
        }
        let state = self.working_state()?;
        let sealed = self
            .capsule
            .sealed
            .as_ref()
            .ok_or_else(|| invalid("accepted checkpoint has no retained artifact"))?;
        if sealed.accepted
            || sealed.checkpoint != head.checkpoint
            || sealed.er_slot != head.er_slot
            || sealed.checkpoint_hash != head.checkpoint_hash
            || state.root().map_err(invalid)? != head.state_root
        {
            return Err(invalid(
                "L1 checkpoint head does not match the retained artifact",
            ));
        }
        let mut next = self.capsule.clone();
        next.accepted_state = state;
        next.accepted_head = Some(head);
        next.sealed
            .as_mut()
            .ok_or_else(|| invalid("sealed checkpoint missing"))?
            .accepted = true;
        self.commit(next)
    }

    pub fn record_settled(&mut self, head: &L1CheckpointHeadV2) -> io::Result<()> {
        if self.capsule.accepted_head.as_ref() != Some(head)
            || !self
                .capsule
                .sealed
                .as_ref()
                .is_some_and(|sealed| sealed.accepted)
        {
            return Err(invalid("settlement does not match the accepted checkpoint"));
        }
        let mut next = self.capsule.clone();
        next.sealed = None;
        self.commit(next)
    }

    fn commit(&mut self, mut next: LineageCapsuleV2) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other("lineage persistence is unavailable"));
        }
        next.sequence = self
            .capsule
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid("lineage sequence exhausted"))?;
        next.working_state()?;
        if let Err(error) = self.install(&next) {
            self.poisoned = true;
            return Err(error);
        }
        self.capsule = next;
        Ok(())
    }

    fn install(&self, next: &LineageCapsuleV2) -> io::Result<()> {
        let payload = borsh::to_vec(next).map_err(invalid)?;
        if u64::try_from(payload.len())
            .ok()
            .is_none_or(|length| length > MAX_CAPSULE_BYTES - 32)
        {
            return Err(invalid("lineage capsule exceeds its limit"));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&hash_parts(&[&payload]))?;
        temporary.write_all(&payload)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.directory.join(CAPSULE_FILE))
            .map_err(|error| error.error)?;
        File::open(&self.directory)?.sync_all()
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options
}

fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            Err(invalid("lineage input is not a regular file"))
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_regular(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = private_options().read(true).open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > limit {
        return Err(invalid("lineage input is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())
        .ok()
        .is_none_or(|length| length > limit)
    {
        return Err(invalid("lineage input exceeds its limit"));
    }
    Ok(bytes)
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
#[path = "lineage_store_v2_tests.rs"]
mod tests;
