//! Prefix arithmetic and endpoint binding used at both v2 proposal and commit.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Head<'a> {
    pub count: u64,
    pub root: &'a [u8; 32],
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::prefix_spec::increment(count,r)))]
pub(crate) fn next_sequence(count: u64) -> Option<u64> {
    count.checked_add(1)
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == crate::prefix_spec::allowed(consumed,tip,start,end,steps)))]
pub(crate) fn permits(
    consumed: Head<'_>,
    tip: Head<'_>,
    start: Head<'_>,
    end: Head<'_>,
    steps: u64,
) -> bool {
    if start.count != consumed.count
        || start.root.as_slice() != consumed.root.as_slice()
        || end.count > tip.count
    {
        return false;
    }
    let Some(used) = end.count.checked_sub(start.count) else {
        return false;
    };
    used <= steps
        && (used != 0 || start.root.as_slice() == end.root.as_slice())
        && (end.count != tip.count || end.root.as_slice() == tip.root.as_slice())
}
