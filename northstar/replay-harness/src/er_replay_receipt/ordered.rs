//! Ordered, success-only replay for one ER working slot. V1 carries signed packets,
//! not precomputed state changes, execution results, or roots.
use {
    super::{
        Digest, Error, ExecutionMode, MAX_WIRE, Observation, execute_success, successful_batch_root,
    },
    solana_runtime::{bank::Bank, bank_forks::BankForks},
};

const TAG: &[u8; 8] = b"NSERPK01";
// ContinuityV2 seals a checkpoint at 16 transitions. Do not use the larger
// generic receipt-codec bound to admit more packets to this profile.
pub const MAX_PACKETS: usize = 16;
const HEADER_BYTES: usize = TAG.len() + 8 + 2;
const MAX_BYTES: usize = HEADER_BYTES + MAX_PACKETS * (2 + MAX_WIRE);

/// `tag | slot:u64le | count:u16le | (length:u16le | signed packet)*`.
/// Validates the container only; SVM admission is part of replay.
pub fn encode(slot: u64, packets: &[&[u8]]) -> Result<Vec<u8>, Error> {
    if !(1..=MAX_PACKETS).contains(&packets.len()) {
        return Err(Error::Batch);
    }
    let mut body = Vec::from(TAG.as_slice());
    body.extend_from_slice(&slot.to_le_bytes());
    body.extend_from_slice(&(packets.len() as u16).to_le_bytes());
    for (i, packet) in packets.iter().enumerate() {
        if !(1..=MAX_WIRE).contains(&packet.len()) || packets[..i].contains(packet) {
            return Err(Error::Batch);
        }
        body.extend_from_slice(&(packet.len() as u16).to_le_bytes());
        body.extend_from_slice(packet);
    }
    Ok(body)
}

fn take<'a, const N: usize>(input: &mut &'a [u8]) -> Result<&'a [u8; N], Error> {
    let (head, rest) = input.split_first_chunk::<N>().ok_or(Error::Batch)?;
    *input = rest;
    Ok(head)
}

fn decode(slot: u64, mut input: &[u8]) -> Result<Vec<&[u8]>, Error> {
    if input.len() > MAX_BYTES || take::<8>(&mut input)? != TAG {
        return Err(Error::Batch);
    }
    if u64::from_le_bytes(*take(&mut input)?) != slot {
        return Err(Error::Slot);
    }
    let count = usize::from(u16::from_le_bytes(*take(&mut input)?));
    if !(1..=MAX_PACKETS).contains(&count) {
        return Err(Error::Batch);
    }
    let mut packets = Vec::with_capacity(count);
    for _ in 0..count {
        let len = usize::from(u16::from_le_bytes(*take(&mut input)?));
        if !(1..=MAX_WIRE).contains(&len) {
            return Err(Error::Batch);
        }
        let (packet, rest) = input.split_at_checked(len).ok_or(Error::Batch)?;
        if packets.contains(&packet) {
            return Err(Error::Batch);
        }
        packets.push(packet);
        input = rest;
    }
    if !input.is_empty() {
        return Err(Error::Batch);
    }
    Ok(packets)
}

#[derive(Debug, PartialEq, Eq)]
pub struct BatchObservation {
    pub transactions: Vec<Observation>,
    pub receipts_root: Digest,
}

/// Consume a replica's **independently materialized disposable** bank. Its entire
/// AccountsDb, status cache, program cache and ancestry must be private to this
/// replay; an ER child of a live bank shares backing storage and is not suitable.
/// The caller is responsible for authenticating bootstrap state and slot context.
/// A later packet can read an earlier packet's committed effects. No partial
/// observation or reusable bank is returned, even if the final packet fails.
pub fn replay_on_disposable_bank(
    scratch: Bank,
    expected_slot: u64,
    body: &[u8],
) -> Result<BatchObservation, Error> {
    let packets = decode(expected_slot, body)?;
    if scratch.slot() != expected_slot || scratch.freeze_started() {
        return Err(Error::Slot);
    }
    if scratch
        .accounts()
        .accounts_db
        .has_accounts_update_notifier()
    {
        return Err(Error::Batch);
    }
    let forks = BankForks::new_rw_arc_ephemeral(scratch);
    let bank = forks.read().unwrap().working_bank();
    let mut transactions = Vec::with_capacity(packets.len());
    let mut entries = Vec::with_capacity(packets.len());
    for packet in packets {
        let obs = execute_success(&bank, expected_slot, packet, ExecutionMode::AdvanceScratch)?;
        entries.push(obs.receipt);
        transactions.push(obs);
    }
    Ok(BatchObservation {
        receipts_root: successful_batch_root(&entries)?,
        transactions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_container_is_canonical_bounded_and_ordered() {
        let a: &[u8] = b"first";
        let b: &[u8] = b"second";
        let body = encode(42, &[a, b]).unwrap();
        assert_eq!(decode(42, &body).unwrap(), [a, b]);
        assert_ne!(body, encode(42, &[b, a]).unwrap());
        assert_eq!(decode(43, &body), Err(Error::Slot));
        for end in 0..body.len() {
            assert!(decode(42, &body[..end]).is_err());
        }
        let mut changed = body.clone();
        changed.push(0);
        assert_eq!(decode(42, &changed), Err(Error::Batch));
        changed = body.clone();
        changed[0] ^= 1;
        assert_eq!(decode(42, &changed), Err(Error::Batch));
        for n in [0u16, 1, (MAX_PACKETS + 1) as u16] {
            changed = body.clone();
            changed[16..18].copy_from_slice(&n.to_le_bytes());
            assert_eq!(decode(42, &changed), Err(Error::Batch));
        }
        assert_eq!(encode(42, &[]), Err(Error::Batch));
        assert_eq!(encode(42, &[&[]]), Err(Error::Batch));
        assert_eq!(encode(42, &[&[0; MAX_WIRE + 1]]), Err(Error::Batch));
        assert_eq!(encode(42, &[a, a]), Err(Error::Batch));
        let mut duplicate = encode(42, &[b"a", b"b"]).unwrap();
        *duplicate.last_mut().unwrap() = b'a';
        assert_eq!(decode(42, &duplicate), Err(Error::Batch));
        let rows: Vec<_> = (0..MAX_PACKETS).map(|n| vec![n as u8; MAX_WIRE]).collect();
        let packets: Vec<_> = rows.iter().map(Vec::as_slice).collect();
        let max = encode(42, &packets).unwrap();
        assert_eq!(max.len(), MAX_BYTES);
        assert_eq!(decode(42, &max).unwrap(), packets);
        assert_eq!(decode(42, &[0; MAX_BYTES + 1]), Err(Error::Batch));
    }
}
