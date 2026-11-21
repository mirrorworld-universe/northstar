//! Accounting for one session bridge and mint at a time. Used by the V2 journal.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub struct Flow {
    before: u128,
    after: u128,
    ingress: u128,
    pending: u128,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub enum Entry {
    Before(u64),
    After(u64),
    Ingress(u64),
    Pending(u64),
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::liability_spec::zero(r)))]
pub fn empty_flow() -> Flow {
    Flow {
        before: 0,
        after: 0,
        ingress: 0,
        pending: 0,
    }
}

impl Flow {
    #[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::liability_spec::add(self, entry, r)))]
    pub fn record(mut self, entry: Entry) -> Option<Self> {
        let (total, amount) = match entry {
            Entry::Before(n) => (&mut self.before, n),
            Entry::After(n) => (&mut self.after, n),
            Entry::Ingress(n) => (&mut self.ingress, n),
            Entry::Pending(n) => (&mut self.pending, n),
        };
        *total = total.checked_add(u128::from(amount))?;
        Some(self)
    }

    #[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == crate::liability_spec::balanced(self)))]
    pub fn conserved(self) -> bool {
        match (
            self.before.checked_add(self.ingress),
            self.after.checked_add(self.pending),
        ) {
            (Some(available), Some(claims)) => available == claims,
            _ => false,
        }
    }
}

#[cfg(verus_keep_ghost)]
verus! {
impl Flow {
    pub closed spec fn totals(self) -> (int, int, int, int) {
        (self.before as int, self.after as int, self.ingress as int, self.pending as int)
    }
}
}
