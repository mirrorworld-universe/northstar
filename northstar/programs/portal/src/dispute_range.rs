//! Arithmetic shared by the challenge response, bisection, proof and timeout handlers.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Range {
    pub lo: u64,
    pub hi: u64,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum Response {
    Step,
    Mid(u64),
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum RangeError {
    Invalid,
    Overflow,
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::range_spec::response(range,r)))]
pub(crate) fn respond(range: Range) -> Option<Response> {
    let width = range.hi.checked_sub(range.lo)?;
    match width {
        0 => None,
        1 => Some(Response::Step),
        _ => Some(Response::Mid(range.lo + width / 2)),
    }
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (range.hi as int == range.lo as int + 1)))]
pub(crate) fn isolated(range: Range) -> bool {
    matches!(respond(range), Some(Response::Step))
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::range_spec::selection(range,mid,upper,round,r)))]
pub(crate) fn select_half(
    range: Range,
    mid: u64,
    upper: bool,
    round: u32,
) -> Result<(Range, u32), RangeError> {
    if mid <= range.lo || mid >= range.hi {
        return Err(RangeError::Invalid);
    }
    let round = round.checked_add(1).ok_or(RangeError::Overflow)?;
    let next = if upper {
        Range {
            lo: mid,
            hi: range.hi,
        }
    } else {
        Range {
            lo: range.lo,
            hi: mid,
        }
    };
    Ok((next, round))
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::range_spec::deadline(now,hard,window,r)))]
pub(crate) fn next_deadline(now: u64, hard: u64, window: u64) -> Option<u64> {
    let end = now.checked_add(window)?;
    Some(if end < hard { end } else { hard })
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (now < turn && now < hard)))]
pub(crate) fn live(now: u64, turn: u64, hard: u64) -> bool {
    now < turn && now < hard
}
