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
    history: SlotHashes,
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
        let (pin, history) = observe(full_bank)?;
        let epoch = RecentHashEpoch::new(start_id, expected.slot, max_age).ok_or(Error::Context)?;
        if pin != expected {
            return Err(Error::Context);
        }
        Ok(Self {
            pin,
            history,
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
        let parent = scratch.parent().ok_or(Error::Context)?;
        let slot = scratch.slot();
        if self.pin.slot.checked_add(1) != Some(slot)
            || scratch.parent_slot() != self.pin.slot
            || scratch.parent_hash().to_bytes() != self.pin.bank_hash
            || Self::measure(&parent)? != self.pin
            || scratch.freeze_started()
            || scratch.clock().slot != slot
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
        let (rows, _) = history(&scratch)?;
        let retain = self.history.len().min(MAX_ENTRIES - 1);
        if rows.slot_hashes().get(1..) != Some(&self.history[..retain]) {
            return Err(Error::Context);
        }
        if self.first {
            if !scratch.install_initial_replay_queue(&self.epoch) {
                return Err(Error::Context);
            }
        } else if !scratch.matches_certified_replay_queue(&self.epoch, slot) {
            return Err(Error::Context);
        }
        let forks = BankForks::new_rw_arc_ephemeral(scratch);
        let bank = forks.read().unwrap().working_bank();
        let batch = body
            .map(|bytes| execute_batch(&bank, slot, bytes))
            .transpose()?;
        let count = batch.as_ref().map_or(0, |b| b.transactions.len() as u64);
        if parent.transaction_count().checked_add(count) != Some(bank.transaction_count())
            || (count == 0 && bank.signature_count() != 0)
        {
            return Err(Error::Context);
        }
        let last = self.epoch.completed_slot_hash(slot).ok_or(Error::Context)?;
        // Bank registers a recent hash only on the final tick. Do not use the
        // new bank hash (which depends on this tick) as the transaction hash.
        for _ in 0..bank.ticks_per_slot() {
            bank.register_tick_for_test(&last);
        }
        if !bank.is_complete()
            || !bank.matches_certified_replay_queue(
                &self.epoch,
                slot.checked_add(1).ok_or(Error::Context)?,
            )
        {
            return Err(Error::Context);
        }
        bank.freeze_and_verify_bank_hash()
            .map_err(|_| Error::Context)?;
        let (pin, final_history) = observe(&bank)?;
        if final_history != rows {
            return Err(Error::Context);
        }
        Ok(ReplayedSlot {
            next: Self {
                pin,
                history: final_history,
                epoch: self.epoch,
                first: false,
            },
            frozen: bank,
            batch,
        })
    }
}
