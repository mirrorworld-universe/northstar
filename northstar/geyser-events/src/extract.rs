use {
    crate::{
        config::Commitment,
        event::{ProgramEvent, Source},
    },
    agave_geyser_plugin_interface::geyser_plugin_interface::ReplicaTransactionInfoV4,
    base64::{Engine, prelude::BASE64_STANDARD},
    solana_pubkey::Pubkey,
    std::{collections::HashSet, str::FromStr},
};

// Anchor's EVENT_IX_TAG_LE, not the event discriminator carried in the payload.
const EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

#[derive(Default)]
struct Frame {
    program: Pubkey,
    events: Vec<(usize, Pubkey, String)>,
    inner_indices: Vec<usize>,
}

pub fn extract(
    info: &ReplicaTransactionInfoV4<'_>,
    slot: u64,
    watched: &HashSet<Pubkey>,
) -> Vec<ProgramEvent> {
    if info.is_vote || info.transaction_status_meta.status.is_err() || watched.is_empty() {
        return Vec::new();
    }
    let keys: Vec<_> = info
        .transaction
        .message
        .static_account_keys()
        .iter()
        .chain(&info.transaction_status_meta.loaded_addresses.writable)
        .chain(&info.transaction_status_meta.loaded_addresses.readonly)
        .copied()
        .collect();
    if !keys.iter().any(|key| watched.contains(key)) {
        return Vec::new();
    }
    let inner: Vec<_> = info
        .transaction_status_meta
        .inner_instructions
        .unwrap_or_default()
        .iter()
        .flat_map(|group| group.instructions)
        .collect();
    let mut stack: Vec<Frame> = Vec::new();
    let mut log_events = Vec::new();
    let mut failed_inner = HashSet::new();
    let mut inner_cursor = 0;
    let mut ordinal = 0usize;

    for line in info
        .transaction_status_meta
        .log_messages
        .unwrap_or_default()
    {
        if let Some((program, depth)) = invocation(line) {
            let inner_index = if depth > 1 {
                let index = inner_cursor;
                inner_cursor += 1;
                inner
                    .get(index)
                    .and_then(|instruction| {
                        keys.get(instruction.instruction.program_id_index as usize)
                    })
                    .filter(|key| **key == program)
                    .map(|_| index)
            } else {
                None
            };
            if depth != stack.len() + 1 {
                stack.clear();
                if depth != 1 {
                    continue;
                }
            }
            stack.push(Frame {
                program,
                inner_indices: inner_index.into_iter().collect(),
                ..Frame::default()
            });
        } else if let Some((program, success)) = completion(line) {
            if stack.last().is_none_or(|frame| frame.program != program) {
                stack.clear();
                continue;
            }
            let mut frame = stack.pop().unwrap();
            if !success {
                failed_inner.extend(frame.inner_indices);
                continue;
            }
            if let Some(parent) = stack.last_mut() {
                parent.events.append(&mut frame.events);
                parent.inner_indices.append(&mut frame.inner_indices);
            } else {
                log_events.append(&mut frame.events);
            }
        } else if let Some(encoded) = line.strip_prefix("Program data: ") {
            // Count every payload, even for unwatched programs or rolled-back calls.
            // Subscription changes must not change another program's event IDs.
            let index = ordinal;
            ordinal += 1;
            if let Some(frame) = stack.last_mut()
                && watched.contains(&frame.program)
                && let Ok(data) = BASE64_STANDARD.decode(encoded)
            {
                frame
                    .events
                    .push((index, frame.program, BASE64_STANDARD.encode(data)));
            }
        }
    }
    let make_event = |index, program: Pubkey, data, source| ProgramEvent {
        program_id: program.to_string(),
        slot,
        block_time: None,
        signature: info.signature.to_string(),
        tx_index: info.index,
        log_index: index,
        source,
        commitment: Commitment::Confirmed,
        data,
    };
    let mut events: Vec<_> = log_events
        .into_iter()
        .map(|(index, program, data)| make_event(index, program, data, Source::Log))
        .collect();
    for (index, instruction) in inner.iter().enumerate() {
        let Some(data) = instruction.instruction.data.strip_prefix(&EVENT_IX_TAG) else {
            continue;
        };
        let event_index = ordinal;
        ordinal += 1;
        let Some(program) = keys.get(instruction.instruction.program_id_index as usize) else {
            continue;
        };
        if data.len() >= 8 && watched.contains(program) && !failed_inner.contains(&index) {
            events.push(make_event(
                event_index,
                *program,
                BASE64_STANDARD.encode(data),
                Source::Cpi,
            ));
        }
    }
    events.sort_unstable_by_key(|event| event.log_index);
    events
}

fn invocation(line: &str) -> Option<(Pubkey, usize)> {
    let (program, depth) = line.strip_prefix("Program ")?.split_once(" invoke [")?;
    Some((
        Pubkey::from_str(program).ok()?,
        depth.strip_suffix(']')?.parse().ok()?,
    ))
}

fn completion(line: &str) -> Option<(Pubkey, bool)> {
    let line = line.strip_prefix("Program ")?;
    if let Some(program) = line.strip_suffix(" success") {
        Some((program.parse().ok()?, true))
    } else {
        let (program, _) = line.split_once(" failed: ")?;
        Some((program.parse().ok()?, false))
    }
}

#[cfg(test)]
mod tests;
