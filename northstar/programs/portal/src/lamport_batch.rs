//! Streaming tally for settlement of independently authenticated, distinct delegated accounts.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Tally {
    pub current: u128,
    pub target: u128,
    pub unchanged: bool,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Row {
    pub current: u64,
    pub target: u64,
    pub floor: u64,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum BatchError {
    Rent,
    Overflow,
    Unbalanced,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum Effect {
    Noop,
    Write(Option<u64>),
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum Guard {
    Retry,
    Apply,
    Mismatch,
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r.current==0 && r.target==0 && r.unchanged))]
pub(crate) fn empty() -> Tally {
    Tally {
        current: 0,
        target: 0,
        unchanged: true,
    }
}
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::batch_spec::row(t,e,r)))]
pub(crate) fn add(t: Tally, e: Row) -> Result<Tally, BatchError> {
    if e.target < e.floor {
        return Err(BatchError::Rent);
    }
    let current = t
        .current
        .checked_add(e.current as u128)
        .ok_or(BatchError::Overflow)?;
    let target = t
        .target
        .checked_add(e.target as u128)
        .ok_or(BatchError::Overflow)?;
    Ok(Tally {
        current,
        target,
        unchanged: t.unchanged && e.current == e.target,
    })
}
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::batch_spec::effect(t,vault,r)))]
pub(crate) fn finish(t: Tally, vault: Option<u64>) -> Result<Effect, BatchError> {
    if t.unchanged && vault.is_none() {
        return Ok(Effect::Noop);
    }
    let surplus = t
        .current
        .checked_sub(t.target)
        .ok_or(BatchError::Unbalanced)?;
    match vault {
        Some(v) => {
            if surplus > u64::MAX as u128 {
                return Err(BatchError::Overflow);
            }
            let next = v.checked_add(surplus as u64).ok_or(BatchError::Overflow)?;
            Ok(Effect::Write(Some(next)))
        }
        None if surplus == 0 => Ok(Effect::Write(None)),
        None => Err(BatchError::Unbalanced),
    }
}
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::batch_spec::decision(actual,previous,next,r)))]
pub(crate) fn guard(actual: &[u8; 32], previous: &[u8; 32], next: &[u8; 32]) -> Guard {
    if actual.as_slice() == next.as_slice() {
        Guard::Retry
    } else if actual.as_slice() == previous.as_slice() {
        Guard::Apply
    } else {
        Guard::Mismatch
    }
}
