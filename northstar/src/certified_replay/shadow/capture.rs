use {
    super::{invalid, RecordedSlot},
    northstar_replay_harness::er_replay_receipt::{
        ordered::{encode, MAX_PACKETS},
        slot_chain::{BankPin, SlotChain},
    },
    solana_runtime::bank::Bank,
    solana_transaction_status::{ConfirmedBlock, TransactionWithStatusMeta},
    std::io,
};

/// Capture at local freeze, before the ER parent is disconnected. The block's
/// transaction list alone cannot establish coverage: ER history omits empty slots.
pub struct FrozenSlotCapture {
    pin: BankPin,
    tick: [u8; 32],
    packets: Option<Vec<u8>>,
}

impl FrozenSlotCapture {
    pub fn from_bank(bank: &Bank, block: Option<&ConfirmedBlock>) -> io::Result<Self> {
        let pin = SlotChain::measure(bank).map_err(io::Error::other)?;
        let parent = bank
            .parent()
            .ok_or_else(|| invalid("shadow capture has no frozen parent"))?;
        let count = bank
            .transaction_count()
            .checked_sub(parent.transaction_count())
            .ok_or_else(|| invalid("shadow transaction count regressed"))?;
        if !parent.is_complete()
            || !parent.is_frozen()
            || parent.slot().checked_add(1) != Some(bank.slot())
            || bank.parent_hash() != parent.hash()
            || count > MAX_PACKETS as u64
        {
            return Err(invalid("invalid or oversized shadow capture span"));
        }
        let mut wires = Vec::new();
        let mut signatures = 0u64;
        if let Some(block) = block {
            if block.parent_slot != parent.slot()
                || block.previous_blockhash != parent.hash().to_string()
                || block.block_height != Some(bank.block_height())
                || block.blockhash != bank.last_blockhash().to_string()
                || block.transactions.len() as u64 != count
            {
                return Err(invalid("shadow block differs from the frozen bank"));
            }
            for item in &block.transactions {
                let TransactionWithStatusMeta::Complete(tx) = item else {
                    return Err(invalid("missing shadow transaction metadata"));
                };
                let first = tx
                    .transaction
                    .signatures
                    .first()
                    .ok_or_else(|| invalid("missing shadow transaction signature"))?;
                if tx.meta.status.is_err()
                    || bank.get_signature_status_slot(first) != Some((bank.slot(), Ok(())))
                    || bincode::serialized_size(&tx.transaction).map_err(io::Error::other)? > 1232
                {
                    return Err(invalid("unsupported shadow transaction"));
                }
                signatures = signatures
                    .checked_add(tx.transaction.signatures.len() as u64)
                    .ok_or_else(|| invalid("shadow signature count overflow"))?;
                wires.push(bincode::serialize(&tx.transaction).map_err(io::Error::other)?);
            }
        }
        if wires.len() as u64 != count || signatures != bank.signature_count() {
            return Err(invalid("incomplete shadow slot history"));
        }
        let packets = if wires.is_empty() {
            None
        } else {
            Some(
                encode(
                    bank.slot(),
                    &wires.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                )
                .map_err(io::Error::other)?,
            )
        };
        Ok(Self {
            pin,
            tick: bank.last_blockhash().to_bytes(),
            packets,
        })
    }

    pub fn input(&self) -> RecordedSlot<'_> {
        RecordedSlot {
            expected: self.pin,
            completed_blockhash: self.tick,
            packets: self.packets.as_deref(),
        }
    }
}
