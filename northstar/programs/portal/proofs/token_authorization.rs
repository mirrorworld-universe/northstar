#![allow(dead_code)]
#[path = "../src/withdrawal_auth.rs"]
pub(crate) mod withdrawal_auth;

pub(crate) mod auth_spec {
    use super::withdrawal_auth::*;
    use vstd::prelude::*;

    verus! {
    pub(crate) open spec fn transition(before: bool, after: bool, stored: AuthorizationKey,
        requested: AuthorizationKey, accepted: bool) -> bool {
        let bound = (stored.checkpoint@ =~= requested.checkpoint@)
            && (stored.tuple_hash@ =~= requested.tuple_hash@) && stored.bump == requested.bump;
        &&& accepted == (!before && bound)
        &&& after == (before || accepted)
    }

    // A match can always consume a fresh authorization. No second request, even one
    // with different key material, can consume the resulting authorization again.
    fn matching_once_for_every_key(key: AuthorizationKey, retry: AuthorizationKey) {
        let mut bit = false;
        let first = mark_consumed(&mut bit, key, key);
        assert(first && bit);
        let second = mark_consumed(&mut bit, key, retry);
        assert(!second && bit);
    }
    }
}
