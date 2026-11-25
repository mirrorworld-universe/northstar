//! Owner authority at withdrawal entry; exact economic identity at V2 plan binding.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

type Key = [u8; 32];

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub owner: &'a Key,
    pub registry: &'a Key,
    pub mint: &'a Key,
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == crate::entitlement_spec::authority(signed, actor, holding)))]
pub fn authorized(signed: bool, actor: Source<'_>, holding: Source<'_>) -> bool {
    signed
        && actor.owner.as_slice() == holding.owner.as_slice()
        && actor.registry.as_slice() == holding.registry.as_slice()
        && actor.mint.as_slice() == holding.mint.as_slice()
}

#[cfg(not(target_os = "solana"))]
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
#[cfg_attr(not(verus_keep_ghost), derive(Debug, Eq, PartialEq, Ord, PartialOrd))]
pub struct Intent {
    pub bridge: Key,
    pub registry: Key,
    pub source: Key,
    pub mint: Key,
    pub destination: Key,
    pub amount: u64,
    pub decimals: u8,
}

#[cfg(not(target_os = "solana"))]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::entitlement_spec::issuance(signed, principal, holding, call, r)))]
pub fn issue(signed: bool, principal: &Key, holding: Source<'_>, call: Intent) -> Option<Intent> {
    let actor = Source {
        owner: principal,
        registry: &call.registry,
        mint: &call.mint,
    };
    if call.amount == 0 || !authorized(signed, actor, holding) {
        return None;
    }
    Some(call)
}

#[cfg(not(target_os = "solana"))]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::entitlement_spec::projection(wire, &r)))]
pub fn from_payout(wire: &northstar_portal::payout_wire::Payout<'_>) -> Intent {
    Intent {
        bridge: *wire.bridge_program,
        registry: *wire.session_bridge,
        source: *wire.er_token_account,
        mint: *wire.mint,
        destination: *wire.destination_token_account,
        amount: wire.amount,
        decimals: wire.decimals,
    }
}

#[cfg(not(target_os = "solana"))]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == crate::entitlement_spec::same(a, b)))]
fn same_intent(a: &Intent, b: &Intent) -> bool {
    a.bridge.as_slice() == b.bridge.as_slice()
        && a.registry.as_slice() == b.registry.as_slice()
        && a.source.as_slice() == b.source.as_slice()
        && a.mint.as_slice() == b.mint.as_slice()
        && a.destination.as_slice() == b.destination.as_slice()
        && a.amount == b.amount
        && a.decimals == b.decimals
}

// Both inputs are permutations of their original lists. Sorting is needed for completeness,
// not soundness: a successful comparison pairs every occurrence, even for equal requests.
#[cfg(not(target_os = "solana"))]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == crate::entitlement_spec::aligned(requests@, payments@)))]
pub fn match_occurrences(requests: &[Intent], payments: &[Intent]) -> bool {
    if requests.len() != payments.len() {
        return false;
    }
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(invariant i <= requests.len(), requests.len() == payments.len(),
        forall|j:int| 0 <= j < i ==> crate::entitlement_spec::same(&requests@[j], &payments@[j]),
        decreases requests.len() - i))]
    while i < requests.len() {
        if !same_intent(&requests[i], &payments[i]) {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(not(target_os = "solana"))]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == crate::entitlement_spec::step(previous, amount, target)))]
pub fn next_ticket(previous: Option<u64>, amount: u64, target: u64) -> bool {
    if amount == 0 || target < amount {
        return false;
    }
    match previous {
        None => true,
        Some(last) => target - amount == last,
    }
}
