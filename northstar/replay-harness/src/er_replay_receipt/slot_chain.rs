//! Shadow progression over a replica's independently materialized full ER banks.
//! Neither Portal's opening nor a transaction recent blockhash is an Agave bank hash.
#[cfg(test)]
mod tests;

use {
    super::{
        Error,
        ordered::{BatchObservation, execute_batch},
    },
    solana_account::ReadableAccount,
    solana_accounts_db::blockhash_queue::certified_er::RecentHashEpoch,
    solana_hash::Hash,
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    solana_sdk_ids::sysvar,
    solana_sha256_hasher::hashv,
    solana_slot_hashes::{MAX_ENTRIES, SIZE, SlotHashes},
    std::sync::Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BankPin {
    pub slot: u64,
    pub bank_hash: [u8; 32],
    pub history_hash: [u8; 32],
}

pub struct SlotChain {
    pin: BankPin,
    epoch: RecentHashEpoch,
    first: bool,
}

pub struct ReplayedSlot {
    pub next: SlotChain,
    /// Frozen, from this successful replay only. Never share its backing store
    /// with the active ER, another replica, or another speculative proposal.
    pub frozen: Arc<Bank>,
    pub batch: Option<BatchObservation>,
}

fn history(bank: &Bank) -> Result<(SlotHashes, [u8; 32]), Error> {
    let account = bank
        .get_account(&sysvar::slot_hashes::id())
        .ok_or(Error::Context)?;
    let bytes = account.data();
    if account.executable()
        || account.owner() != &sysvar::id()
        || account.lamports() == 0
        || bytes.len() > SIZE
    {
        return Err(Error::Context);
    }
    let count = u64::from_le_bytes(*bytes.first_chunk().ok_or(Error::Context)?);
    if !(1..=MAX_ENTRIES as u64).contains(&count) {
        return Err(Error::Context);
    }
    let rows: SlotHashes = wincode::deserialize(bytes).map_err(|_| Error::Context)?;
    if rows.first().map(|r| (r.slot, r.hash)) != Some((bank.parent_slot(), bank.parent_hash()))
        || bank.parent_slot() >= bank.slot()
        || rows.windows(2).any(|w| w[0].slot <= w[1].slot)
        || rows.iter().any(|r| r.hash == Hash::default())
    {
        return Err(Error::Context);
    }
    // Programs can read the raw sysvar including padding, not just decoded rows.
    let digest = hashv(&[
        b"northstar/er-cert/slot-hashes/v2\0",
        &(bytes.len() as u32).to_le_bytes(),
        bytes,
    ]);
    Ok((rows, digest.to_bytes()))
}

fn observe(bank: &Bank) -> Result<(BankPin, SlotHashes), Error> {
    if !bank.is_frozen()
        || !bank.is_complete()
        || bank.clock().slot != bank.slot()
        || bank.hash() == Hash::default()
    {
        return Err(Error::Context);
    }
    let (history, history_hash) = history(bank)?;
    Ok((
        BankPin {
            slot: bank.slot(),
            bank_hash: bank.hash().to_bytes(),
            history_hash,
        },
        history,
    ))
}

impl SlotChain {
    /// Read-only evidence for epoch enrollment. A measurement alone is not approval.
    pub fn measure(full_bank: &Bank) -> Result<BankPin, Error> {
        Ok(observe(full_bank)?.0)
    }

    /// The starting bank belongs to the previous profile. Only its child uses
    /// hashes derived from the new starting head, avoiding a circular anchor.
    pub fn enroll(
        expected: BankPin,
        start_id: [u8; 32],
        max_age: u32,
        full_bank: &Bank,
    ) -> Result<Self, Error> {
        let pin = Self::measure(full_bank)?;
        let epoch = RecentHashEpoch::new(start_id, expected.slot, max_age).ok_or(Error::Context)?;
        if pin != expected {
            return Err(Error::Context);
        }
        Ok(Self {
            pin,
            epoch,
            first: true,
        })
    }

    pub fn pin(&self) -> BankPin {
        self.pin
    }

    /// `scratch` must be an independently replayed full, disposable ER child
    /// with private ancestry and caches. A partial account snapshot has no such
    /// parent. Only successful completion returns a frozen bank and a new pin.
    pub fn replay(&self, scratch: Bank, packets: &[u8]) -> Result<ReplayedSlot, Error> {
        self.step(scratch, Some(packets))
    }

    /// No packet input is omitted by this operation: the slot must be empty on
    /// the replica's independent input stream. Do not infer that from RPC silence.
    pub fn replay_empty(&self, scratch: Bank) -> Result<ReplayedSlot, Error> {
        self.step(scratch, None)
    }

    fn step(&self, mut scratch: Bank, body: Option<&[u8]>) -> Result<ReplayedSlot, Error> {
        let slot = scratch.slot();
        let rows = check_child(&scratch, self.pin)?;
        if self.first {
            if !scratch.install_initial_replay_queue(&self.epoch) {
                return Err(Error::Context);
            }
        } else if !scratch.matches_certified_replay_queue(&self.epoch, slot) {
            return Err(Error::Context);
        }
        let last = self.epoch.completed_slot_hash(slot).ok_or(Error::Context)?;
        let (bank, batch) = finish_slot(scratch, last, body)?;
        if !bank
            .matches_certified_replay_queue(&self.epoch, slot.checked_add(1).ok_or(Error::Context)?)
        {
            return Err(Error::Context);
        }
        let (pin, final_history) = observe(&bank)?;
        if final_history != rows {
            return Err(Error::Context);
        }
        Ok(ReplayedSlot {
            next: Self {
                pin,
                epoch: self.epoch,
                first: false,
            },
            frozen: bank,
            batch,
        })
    }
}

fn check_child(scratch: &Bank, expected: BankPin) -> Result<SlotHashes, Error> {
    let parent = scratch.parent().ok_or(Error::Context)?;
    let (actual, old) = observe(&parent)?;
    if actual != expected
        || expected.slot.checked_add(1) != Some(scratch.slot())
        || scratch.parent_slot() != expected.slot
        || scratch.parent_hash().to_bytes() != expected.bank_hash
        || scratch.freeze_started()
        || scratch.clock().slot != scratch.slot()
        || scratch.transaction_count() != parent.transaction_count()
        || scratch.signature_count() != 0
        || scratch
            .accounts()
            .accounts_db
            .has_accounts_update_notifier()
        || scratch.tick_height() != parent.max_tick_height()
        || !(1..=1024).contains(&scratch.ticks_per_slot())
        || scratch.tick_height().checked_add(scratch.ticks_per_slot())
            != Some(scratch.max_tick_height())
    {
        return Err(Error::Context);
    }
    let (rows, _) = history(scratch)?;
    if rows.slot_hashes().get(1..) != Some(&old[..old.len().min(MAX_ENTRIES - 1)]) {
        return Err(Error::Context);
    }
    Ok(rows)
}

fn finish_slot(
    scratch: Bank,
    final_tick: Hash,
    body: Option<&[u8]>,
) -> Result<(Arc<Bank>, Option<BatchObservation>), Error> {
    let initial = scratch.transaction_count();
    let forks = BankForks::new_rw_arc_ephemeral(scratch);
    let bank = forks.read().unwrap().working_bank();
    let batch = body
        .map(|bytes| execute_batch(&bank, bank.slot(), bytes))
        .transpose()?;
    let count = batch.as_ref().map_or(0, |b| b.transactions.len() as u64);
    if initial.checked_add(count) != Some(bank.transaction_count())
        || (count == 0 && bank.signature_count() != 0)
    {
        return Err(Error::Context);
    }
    // Only the block-boundary tick registers a hash. Other ticks advance height.
    for _ in 0..bank.ticks_per_slot() {
        bank.register_tick_for_test(&final_tick);
    }
    if !bank.is_complete() || bank.last_blockhash() != final_tick {
        return Err(Error::Context);
    }
    bank.freeze_and_verify_bank_hash()
        .map_err(|_| Error::Context)?;
    Ok((bank, batch))
}

/// Shadow the existing ER tick history on a private full-bank replica. No epoch
/// schedule, new transaction hash, vote, or certificate is installed or inferred.
/// `None` means explicitly observed empty, not missing input.
pub fn replay_recorded_slot(
    scratch: Bank,
    parent: BankPin,
    expected: BankPin,
    final_tick: Hash,
    body: Option<&[u8]>,
) -> Result<(Arc<Bank>, Option<BatchObservation>), Error> {
    if expected.slot != scratch.slot() || final_tick == Hash::default() {
        return Err(Error::Context);
    }
    let rows = check_child(&scratch, parent)?;
    let (bank, batch) = finish_slot(scratch, final_tick, body)?;
    let (actual, after) = observe(&bank)?;
    if actual != expected || after != rows {
        return Err(Error::Context);
    }
    Ok((bank, batch))
}
