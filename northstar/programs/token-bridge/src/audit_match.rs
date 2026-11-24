//! Host-only comparison of independently populated, canonically framed evidence.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub struct Transcript<'a> {
    pub execution: &'a [u8],
    pub writes: &'a [u8],
    pub effects: &'a [u8],
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, PartialEq, Eq)]
pub enum Comparison {
    Execution,
    Writes,
    Effects,
    NoCpiWithdrawal,
    Match,
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (a@ =~= b@)))]
fn equal(a: &[u8], b: &[u8]) -> bool {
    a == b
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::audit_spec::comparison(record, observed, cpi_count, r)))]
pub fn compare(record: Transcript<'_>, observed: Transcript<'_>, cpi_count: usize) -> Comparison {
    if !equal(record.execution, observed.execution) {
        Comparison::Execution
    } else if !equal(record.writes, observed.writes) {
        Comparison::Writes
    } else if !equal(record.effects, observed.effects) {
        Comparison::Effects
    } else if cpi_count == 0 {
        Comparison::NoCpiWithdrawal
    } else {
        Comparison::Match
    }
}
