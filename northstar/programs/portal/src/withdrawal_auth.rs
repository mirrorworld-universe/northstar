//! The state transition for one decoded Portal token withdrawal authorization.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct AuthorizationKey<'a> {
    pub(crate) checkpoint: &'a [u8; 32],
    pub(crate) tuple_hash: &'a [u8; 32],
    pub(crate) bump: u8,
}

#[cfg_attr(verus_keep_ghost, verus_spec(accepted =>
    ensures crate::auth_spec::transition(*old(consumed), *final(consumed), stored, requested, accepted)
))]
pub(crate) fn mark_consumed(
    consumed: &mut bool,
    stored: AuthorizationKey<'_>,
    requested: AuthorizationKey<'_>,
) -> bool {
    if *consumed
        || stored.bump != requested.bump
        || stored.checkpoint.as_slice() != requested.checkpoint.as_slice()
        || stored.tuple_hash.as_slice() != requested.tuple_hash.as_slice()
    {
        return false;
    }
    *consumed = true;
    true
}
