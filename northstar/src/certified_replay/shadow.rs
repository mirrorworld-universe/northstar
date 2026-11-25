//! A single replay-only observer of the existing, gasless ER. It uses the
//! source's actual completed tick, not a certified epoch's blockhash schedule.
mod capture;
mod storage;

pub use capture::FrozenSlotCapture;
use {
    super::{invalid, ReplayBootstrap},
    crate::{continuity_v2::hash_parts, slot_advancer::SlotAdvancer, EphemeralRollupSettings},
    northstar_replay_harness::er_replay_receipt::slot_chain::{
        replay_recorded_slot, BankPin, SlotChain,
    },
    solana_hash::Hash,
    solana_leader_schedule::SlotLeader,
    solana_runtime::bank::Bank,
    std::{io, path::Path, sync::Arc},
    storage::Store,
};

const MAGIC: &[u8; 8] = b"NSERSH02";
const MAX_SLOTS: usize = 256;
const MAX_BODY: usize = 18 + 16 * (2 + 1232);
const FIXED: usize = 8 + 32 * 4 + 4;
const MAX_IMAGE: usize = 40 + MAX_SLOTS * (FIXED + MAX_BODY);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowCursor {
    pub slot: u64,
    /// Save separately from the observer directory to detect rollback on resume.
    pub digest: [u8; 32],
}

/// An explicit observation for one physical slot. Missing input must not be
/// submitted as `None`: `None` denotes a known empty slot with a frozen bank pin.
pub struct RecordedSlot<'a> {
    pub expected: BankPin,
    pub completed_blockhash: [u8; 32],
    /// V2 ordered signed-packet container from the source, unmodified.
    pub packets: Option<&'a [u8]>,
}

pub struct ShadowAnchor {
    binding: [u8; 32],
    bank: Arc<Bank>,
    pin: BankPin,
}

impl ReplayBootstrap {
    /// Supply a newly materialized frozen bank with private ancestry and caches,
    /// never a fork of the serving validator. The ER bank pin is independent of
    /// the L1 state opening. No committee roster or signing key is used here.
    pub fn shadow_anchor(
        &self,
        private_bank: Bank,
        expected: BankPin,
        max_age: u32,
    ) -> io::Result<ShadowAnchor> {
        let pins = self.pins();
        if expected.slot != pins.checkpoint.er_slot
            || !(1..=12_000).contains(&max_age)
            || private_bank.max_processing_age() != max_age as usize
            || private_bank.fee_structure() != &EphemeralRollupSettings::zero_fee_structure()
            || private_bank
                .accounts()
                .accounts_db
                .has_accounts_update_notifier()
            || SlotChain::measure(&private_bank).map_err(io::Error::other)? != expected
        {
            return Err(invalid(
                "shadow bootstrap differs from the pinned gasless ER",
            ));
        }
        let images = [pins.portal, pins.coordinator, pins.verifier, pins.consumer]
            .map(|image| (image.address.to_bytes(), image.programdata_sha256));
        let inputs = borsh::to_vec(&(
            &pins.identity,
            &pins.checkpoint,
            pins.finalized_l1_slot_floor,
            pins.minimum_challenge_slots,
            images,
            expected.bank_hash,
            expected.history_hash,
            max_age,
        ))?;
        Ok(ShadowAnchor {
            binding: hash_parts(&[b"northstar/er-shadow/start/v2\0", &inputs]),
            bank: Arc::new(private_bank),
            pin: expected,
        })
    }
}

impl ShadowAnchor {
    fn initial(&self) -> ShadowCursor {
        ShadowCursor {
            slot: self.pin.slot,
            digest: self.binding,
        }
    }
    fn replay(&mut self, input: &RecordedSlot<'_>) -> io::Result<[u8; 32]> {
        if self.pin.slot.checked_add(1) != Some(input.expected.slot)
            || input
                .packets
                .is_some_and(|b| b.is_empty() || b.len() > MAX_BODY)
            || [
                input.expected.bank_hash,
                input.expected.history_hash,
                input.completed_blockhash,
            ]
            .contains(&[0; 32])
        {
            return Err(invalid("shadow input is not the next complete slot"));
        }
        let mut child = Bank::new_from_parent_ephemeral(
            Arc::clone(&self.bank),
            SlotLeader::default(),
            input.expected.slot,
        );
        child.configure_er(self.bank.fee_structure(), self.bank.max_processing_age());
        SlotAdvancer::carry_forward_modified_accounts(&self.bank, &child);
        let (bank, batch) = replay_recorded_slot(
            child,
            self.pin,
            input.expected,
            Hash::new_from_array(input.completed_blockhash),
            input.packets,
        )
        .map_err(io::Error::other)?;
        let root = batch.map_or([0; 32], |b| b.receipts_root);
        if (root == [0; 32]) != input.packets.is_none() {
            return Err(invalid("shadow receipt differs"));
        }
        self.pin = input.expected;
        self.bank = bank;
        Ok(root)
    }
}

/// A bounded pilot window, not a validator, vote log or signing authority.
pub struct ShadowReplica {
    state: ShadowAnchor,
    store: Store,
    image: Vec<u8>,
    slots: usize,
    tip: ShadowCursor,
    stopped: bool,
}

fn header(binding: &[u8; 32]) -> Vec<u8> {
    [MAGIC.as_slice(), binding].concat()
}
fn advance(tip: ShadowCursor, slot: u64, record: &[u8]) -> ShadowCursor {
    ShadowCursor {
        slot,
        digest: hash_parts(&[b"northstar/er-shadow/slot/v2\0", &tip.digest, record]),
    }
}
fn take<const N: usize>(input: &mut &[u8]) -> io::Result<[u8; N]> {
    let (a, b) = input
        .split_first_chunk()
        .ok_or_else(|| invalid("incomplete shadow record"))?;
    *input = b;
    Ok(*a)
}

impl ShadowReplica {
    /// Creates only a new child of an existing private directory. Never adopts
    /// an existing store on creation, and never creates one on resume.
    pub fn create(dir: &Path, state: ShadowAnchor) -> io::Result<Self> {
        let tip = state.initial();
        let image = header(&state.binding);
        let store = Store::open(dir, true)?;
        store.save(&image)?;
        Ok(Self {
            state,
            store,
            image,
            slots: 0,
            tip,
            stopped: false,
        })
    }

    /// Replay from a fresh independently reproduced anchor. An earlier floor is
    /// accepted only when it is on the exact retained prefix. No truncation/reset.
    pub fn resume(dir: &Path, mut state: ShadowAnchor, floor: ShadowCursor) -> io::Result<Self> {
        let store = Store::open(dir, false)?;
        let image = store.read()?;
        let mut input = image
            .strip_prefix(header(&state.binding).as_slice())
            .ok_or_else(|| invalid("shadow store binding differs"))?;
        let mut tip = state.initial();
        let mut slots: usize = 0;
        let mut seen = tip == floor;
        while !input.is_empty() {
            if slots >= MAX_SLOTS {
                return Err(invalid("shadow window exceeds bound"));
            }
            let start = input;
            let slot = u64::from_le_bytes(take(&mut input)?);
            let expected = BankPin {
                slot,
                bank_hash: take(&mut input)?,
                history_hash: take(&mut input)?,
            };
            let completed_blockhash = take(&mut input)?;
            let receipt: [u8; 32] = take(&mut input)?;
            let size = u32::from_le_bytes(take(&mut input)?) as usize;
            if size > MAX_BODY {
                return Err(invalid("shadow packet container exceeds bound"));
            }
            let (body, remaining) = input
                .split_at_checked(size)
                .ok_or_else(|| invalid("incomplete shadow packet container"))?;
            input = remaining;
            let item = RecordedSlot {
                expected,
                completed_blockhash,
                packets: (!body.is_empty()).then_some(body),
            };
            if state.replay(&item)? != receipt {
                return Err(invalid("retained shadow receipt differs"));
            }
            let end = start.len().saturating_sub(input.len());
            tip = advance(tip, slot, &start[..end]);
            seen |= tip == floor;
            slots = slots.saturating_add(1);
        }
        if !seen {
            return Err(invalid(
                "retained shadow prefix does not cover recovery floor",
            ));
        }
        Ok(Self {
            state,
            store,
            image,
            slots,
            tip,
            stopped: false,
        })
    }

    pub fn cursor(&self) -> ShadowCursor {
        self.tip
    }

    /// A failure may leave caches touched on the disposable fork. Never use
    /// that fork again; resume from the last published cursor and a fresh anchor.
    pub fn observe(&mut self, item: RecordedSlot<'_>) -> io::Result<ShadowCursor> {
        if self.stopped || self.slots >= MAX_SLOTS {
            return Err(invalid("shadow replica stopped or window full"));
        }
        self.stopped = true;
        let root = self.state.replay(&item)?;
        let body = item.packets.unwrap_or_default();
        let index = self.image.len();
        self.image.extend(item.expected.slot.to_le_bytes());
        self.image.extend(item.expected.bank_hash);
        self.image.extend(item.expected.history_hash);
        self.image.extend(item.completed_blockhash);
        self.image.extend(root);
        self.image.extend((body.len() as u32).to_le_bytes());
        self.image.extend(body);
        self.store.save(&self.image)?;
        self.tip = advance(self.tip, item.expected.slot, &self.image[index..]);
        self.slots = self.slots.saturating_add(1);
        self.stopped = false;
        Ok(self.tip)
    }
}
