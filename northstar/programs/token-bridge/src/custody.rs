//! Custody is the SPL token amount, not lamports or the bridge's deposit counter.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Ledger {
    pub deposited: u64,
    pub withdrawn: u64,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Amounts {
    pub vault: u64,
    pub peer: u64,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Deposit,
    Payout,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum CustodyError {
    Overflow,
    Insufficient,
    Movement,
}
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) struct Expected {
    ledger: Ledger,
    amounts: Amounts,
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r.deposited == 0 && r.withdrawn == 0))]
pub(crate) fn empty() -> Ledger {
    Ledger {
        deposited: 0,
        withdrawn: 0,
    }
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::custody_spec::posting(direction, old, before, amount, r)))]
pub(crate) fn prepare(
    direction: Direction,
    old: Ledger,
    before: Amounts,
    amount: u64,
) -> Result<Expected, CustodyError> {
    let (ledger, amounts) = match direction {
        Direction::Deposit => {
            let deposited = old
                .deposited
                .checked_add(amount)
                .ok_or(CustodyError::Overflow)?;
            let vault = before
                .vault
                .checked_add(amount)
                .ok_or(CustodyError::Overflow)?;
            let peer = before
                .peer
                .checked_sub(amount)
                .ok_or(CustodyError::Insufficient)?;
            (
                Ledger {
                    deposited,
                    withdrawn: old.withdrawn,
                },
                Amounts { vault, peer },
            )
        }
        Direction::Payout => {
            let withdrawn = old
                .withdrawn
                .checked_add(amount)
                .ok_or(CustodyError::Insufficient)?;
            if withdrawn > old.deposited {
                return Err(CustodyError::Insufficient);
            }
            let vault = before
                .vault
                .checked_sub(amount)
                .ok_or(CustodyError::Insufficient)?;
            let peer = before
                .peer
                .checked_add(amount)
                .ok_or(CustodyError::Overflow)?;
            (
                Ledger {
                    deposited: old.deposited,
                    withdrawn,
                },
                Amounts { vault, peer },
            )
        }
    };
    Ok(Expected { ledger, amounts })
}

impl Expected {
    #[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::custody_spec::observed(self, actual, r)))]
    pub(crate) fn confirm(self, actual: Amounts) -> Result<Ledger, CustodyError> {
        if self.amounts.vault != actual.vault || self.amounts.peer != actual.peer {
            return Err(CustodyError::Movement);
        }
        Ok(self.ledger)
    }
}

// These projections exist only for the specification; Rust callers cannot take
// counters out of an Expected value without confirming the post-CPI observation.
#[cfg(verus_keep_ghost)]
verus! {
impl Expected {
    pub(crate) closed spec fn ledger(self) -> Ledger { self.ledger }
    pub(crate) closed spec fn amounts(self) -> Amounts { self.amounts }
}
}
