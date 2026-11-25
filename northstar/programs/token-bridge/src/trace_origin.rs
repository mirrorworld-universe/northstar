//! Replay checks on transaction-wide account indices, not application addresses.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Exact Borsh framing of StartWithdrawal. Other instructions do not issue a request.
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::origin_spec::opening(data@, r)))]
pub fn withdrawal(data: &[u8]) -> Option<(u64, u8)> {
    if data.len() != 10 || data[0] != 7 {
        return None;
    }
    let amount = u64::from(data[1])
        | (u64::from(data[2]) << 8)
        | (u64::from(data[3]) << 16)
        | (u64::from(data[4]) << 24)
        | (u64::from(data[5]) << 32)
        | (u64::from(data[6]) << 40)
        | (u64::from(data[7]) << 48)
        | (u64::from(data[8]) << 56);
    Some((amount, data[9]))
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub struct Addresses<'a> {
    pub actor: &'a [u8; 32],
    pub program: &'a [u8; 32],
    pub registry: &'a [u8; 32],
    pub source: &'a [u8; 32],
    pub destination: &'a [u8; 32],
    pub mint: &'a [u8; 32],
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::origin_spec::issued(data@, origin, required, who, a, holding, r)))]
pub fn request(
    data: &[u8],
    origin: Origin,
    required: u8,
    who: u8,
    a: Addresses<'_>,
    holding: crate::entitlement::Source<'_>,
) -> Option<crate::entitlement::Intent> {
    let (amount, decimals) = withdrawal(data)?;
    crate::entitlement::issue(
        permits(origin, required, who),
        a.actor,
        holding,
        crate::entitlement::Intent {
            bridge: *a.program,
            registry: *a.registry,
            source: *a.source,
            mint: *a.mint,
            destination: *a.destination,
            amount,
            decimals,
        },
    )
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == parent@.contains(key)))]
fn listed(parent: &[u8], key: u8) -> bool {
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(invariant i <= parent.len(), forall|k:int| 0 <= k < i ==> parent@[k] != key, decreases parent.len()-i))]
    while i < parent.len() {
        if parent[i] == key {
            return true;
        }
        i += 1;
    }
    false
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == crate::origin_spec::subset(child@, parent@)))]
pub fn visible(child: &[u8], parent: &[u8]) -> bool {
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(invariant i <= child.len(), forall|k:int| 0 <= k < i ==> parent@.contains(child@[k]), decreases child.len()-i))]
    while i < child.len() {
        if !listed(parent, child[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// Count of open frames to retain before pushing an inner frame at `height`.
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::origin_spec::parent(open, height, r)))]
pub fn retain_parent(open: usize, height: u32) -> Option<usize> {
    let keep = height.checked_sub(1)? as usize;
    if keep == 0 || keep > open {
        return None;
    }
    Some(keep)
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub enum Origin {
    Message,
    Invocation,
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == crate::origin_spec::authority(origin, required, index)))]
pub fn permits(origin: Origin, required: u8, index: u8) -> bool {
    match origin {
        Origin::Message => index < required,
        // The successful, structurally checked invocation attests its own effective signer.
        Origin::Invocation => true,
    }
}
