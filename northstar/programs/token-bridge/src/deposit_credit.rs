//! Deposits credit the ER immediately or through a cursor, never both at deposit time.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Deposit {
    pub receipt: u64,
    pub immediate: Option<u64>,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Credit {
    pub cursor: u64,
    pub holding: u64,
    // Only V2 binds each individual credit to an ingress record.
    #[cfg_attr(
        not(any(feature = "checkpoint-continuity-v2", verus_keep_ghost)),
        allow(dead_code)
    )]
    pub delta: u64,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum CreditError {
    Cursor,
    Range,
    Overflow,
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::credit_spec::deposit(old_receipt, holding, delegated, amount, r)))]
pub(crate) fn record(
    old_receipt: u64,
    holding: u64,
    delegated: bool,
    amount: u64,
) -> Option<Deposit> {
    let receipt = old_receipt.checked_add(amount)?;
    let immediate = if delegated {
        None
    } else {
        Some(holding.checked_add(amount)?)
    };
    Some(Deposit { receipt, immediate })
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::credit_spec::debit(holding, amount, r)))]
pub(crate) fn retire(holding: u64, amount: u64) -> Option<u64> {
    holding.checked_sub(amount)
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::credit_spec::claim(origin, cursor, available, target, holding, r)))]
pub(crate) fn advance(
    origin: u64,
    cursor: u64,
    available: u64,
    target: u64,
    holding: u64,
) -> Result<Credit, CreditError> {
    if cursor < origin {
        return Err(CreditError::Cursor);
    }
    if target > available || target <= cursor {
        return Err(CreditError::Range);
    }
    let delta = target - cursor;
    let holding = holding.checked_add(delta).ok_or(CreditError::Overflow)?;
    Ok(Credit {
        cursor: target,
        holding,
        delta,
    })
}
