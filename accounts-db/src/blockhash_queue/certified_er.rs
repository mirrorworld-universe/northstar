//! Sonic: Transaction recent blockhashes for a new certified ER epoch. These
//! are public validity tokens, not randomness, bank hashes, or `SlotHashes`.
use {
    super::{BlockhashQueue, HashInfo},
    solana_fee_calculator::FeeCalculator,
    solana_hash::Hash,
    solana_sha256_hasher::hashv,
};

/// Resource bound only. The actual maximum age is part of the pinned epoch policy.
pub const MAX_AGE: u32 = 12_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecentHashEpoch {
    anchor_id: [u8; 32],
    anchor_slot: u64,
    max_age: u32,
}

impl RecentHashEpoch {
    /// `anchor_id` is the authenticated certificate epoch's **starting head ID**,
    /// not a leader-chosen seed or the changing ID of the latest certificate.
    pub fn new(anchor_id: [u8; 32], anchor_slot: u64, max_age: u32) -> Option<Self> {
        if anchor_id == [0; 32]
            || !(1..u64::MAX).contains(&anchor_slot)
            || !(1..=MAX_AGE).contains(&max_age)
        {
            return None;
        }
        Some(Self {
            anchor_id,
            anchor_slot,
            max_age,
        })
    }

    pub fn max_age(&self) -> usize {
        self.max_age as usize
    }

    pub fn completed_slot_hash(&self, slot: u64) -> Option<Hash> {
        (self.anchor_slot..u64::MAX)
            .contains(&slot)
            .then(|| self.hash_at(slot))
    }

    fn hash_at(&self, slot: u64) -> Hash {
        hashv(&[
            b"northstar/er-cert/recent-blockhash/v1\0",
            &self.anchor_id,
            &self.anchor_slot.to_le_bytes(),
            &self.max_age.to_le_bytes(),
            &slot.to_le_bytes(),
        ])
    }

    fn window(&self, working_slot: u64) -> Option<std::ops::RangeInclusive<u64>> {
        let newest = working_slot.checked_sub(1)?;
        (newest >= self.anchor_slot).then(|| {
            newest
                .saturating_sub(u64::from(self.max_age))
                .max(self.anchor_slot)..=newest
        })
    }

    /// Construct only the retained suffix, including age exactly `max_age`.
    /// No inherited L1 or pre-epoch ER hash, wall clock, or replay from slot zero.
    pub fn queue_for_working_slot(&self, working_slot: u64) -> Option<BlockhashQueue> {
        let window = self.window(working_slot)?;
        let newest = *window.end();
        let mut queue = BlockhashQueue::new(self.max_age());
        for slot in window {
            let info = HashInfo {
                fee_calculator: FeeCalculator::new(0),
                hash_index: slot.checked_sub(self.anchor_slot)?,
                timestamp: 0,
            };
            if queue.hashes.insert(self.hash_at(slot), info).is_some() {
                return None;
            }
        }
        queue.last_hash = Some(self.hash_at(newest));
        queue.last_hash_index = newest.checked_sub(self.anchor_slot)?;
        queue.refresh_durable_nonce();
        Some(queue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_vector_separates_epoch_slot_and_policy() {
        let anchor: [u8; 32] = std::array::from_fn(|n| n as u8);
        let start = 0x0102_0304_0506_0708;
        let slot = start + 10;
        let epoch = RecentHashEpoch::new(anchor, start, 6000).unwrap();
        let hash = epoch.completed_slot_hash(slot).unwrap();
        assert_eq!(
            hash.to_bytes(),
            [
                0x1b, 0xbb, 0xc8, 0x8c, 0x61, 0x9a, 0x89, 0xe6, 0xec, 0x1b, 0xf2, 0x2b, 0xe6, 0xbb,
                0x60, 0x3a, 0x81, 0xc7, 0xa7, 0x8f, 0x0e, 0xd0, 0xea, 0xcf, 0xd6, 0xf9, 0x19, 0xf0,
                0x5d, 0xad, 0xc4, 0x35
            ]
        );
        for other in [
            RecentHashEpoch::new([1; 32], start, 6000),
            RecentHashEpoch::new(anchor, start + 1, 6000),
            RecentHashEpoch::new(anchor, start, 5999),
        ] {
            assert_ne!(other.unwrap().completed_slot_hash(slot), Some(hash));
        }
        assert_ne!(epoch.completed_slot_hash(slot + 1), Some(hash));
        assert_eq!(epoch.completed_slot_hash(start - 1), None);
        assert_eq!(epoch.completed_slot_hash(u64::MAX), None);
        for invalid in [
            RecentHashEpoch::new([0; 32], start, 1),
            RecentHashEpoch::new(anchor, 0, 1),
            RecentHashEpoch::new(anchor, u64::MAX, 1),
            RecentHashEpoch::new(anchor, start, 0),
            RecentHashEpoch::new(anchor, start, MAX_AGE + 1),
        ] {
            assert!(invalid.is_none());
        }
    }

    #[test]
    fn restart_reconstructs_exact_age_window_without_legacy_or_future_hashes() {
        let epoch = RecentHashEpoch::new([7; 32], 11, 2).unwrap();
        assert!(epoch.queue_for_working_slot(0).is_none());
        assert!(epoch.queue_for_working_slot(11).is_none());
        let first = epoch.queue_for_working_slot(12).unwrap();
        assert_eq!(first.hashes.len(), 1);
        assert_eq!(first.last_hash(), epoch.completed_slot_hash(11).unwrap());
        for working in [15, 10_000, u64::MAX] {
            let queue = epoch.queue_for_working_slot(working).unwrap();
            assert_eq!(queue, epoch.queue_for_working_slot(working).unwrap());
            assert_eq!(queue.hashes.len(), 3);
            let newest = working - 1;
            for age in 0..=2 {
                let hash = epoch.completed_slot_hash(newest - age).unwrap();
                assert_eq!(queue.get_hash_age(&hash), Some(age));
                assert_eq!(queue.get_lamports_per_signature(&hash), Some(0));
            }
            assert_eq!(
                queue.last_hash(),
                epoch.completed_slot_hash(newest).unwrap()
            );
            assert!(queue.hashes.values().all(|entry| entry.timestamp == 0));
            assert_eq!(
                queue.get_hash_age(&epoch.completed_slot_hash(newest - 3).unwrap()),
                None
            );
            if let Some(future) = epoch.completed_slot_hash(working) {
                assert_eq!(queue.get_hash_age(&future), None);
            }
            assert_eq!(queue.get_hash_age(&Hash::default()), None);
        }
    }
}
