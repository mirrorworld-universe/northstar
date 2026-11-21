//! Assign one positive V2 withdrawal effect a cumulative target in its own vault.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::schedule_spec::reservation(*old(last), *final(last), deposited, amount, r)))]
pub fn reserve(last: &mut u64, deposited: u64, amount: u64) -> Option<u64> {
    if amount == 0 {
        return None;
    }
    let target = last.checked_add(amount)?;
    if target > deposited {
        return None;
    }
    *last = target;
    Some(target)
}
