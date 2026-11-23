#![allow(dead_code)]
#[path = "../src/deposit_credit.rs"]
pub(crate) mod deposit_credit;

pub(crate) mod credit_spec {
    use super::deposit_credit::*;
    use vstd::prelude::*;

    verus! {
    pub(crate) open spec fn deposit(receipt: u64, holding: u64, delegated: bool, n: u64, r: Option<Deposit>) -> bool {
        let next = receipt as int + n as int;
        let credited = holding as int + n as int;
        &&& (r is Some) == (next <= u64::MAX as int && (delegated || credited <= u64::MAX as int))
        &&& match r {
            None => true,
            Some(v) => v.receipt as int == next && match v.immediate {
                Some(value) => !delegated && value as int == credited,
                None => delegated,
            },
        }
    }

    pub(crate) open spec fn claim(origin: u64, cursor: u64, available: u64, target: u64, holding: u64,
        r: Result<Credit, CreditError>) -> bool {
        let delta = target as int - cursor as int;
        &&& (r is Ok) == (origin <= cursor < target <= available && holding as int + delta <= u64::MAX as int)
        &&& match r {
            Ok(x) => x.cursor == target && x.delta as int == delta && x.holding as int == holding as int + delta,
            Err(e) => e == (if cursor < origin { CreditError::Cursor }
                else if target > available || target <= cursor { CreditError::Range } else { CreditError::Overflow }),
        }
    }

    pub(crate) open spec fn debit(holding: u64, n: u64, r: Option<u64>) -> bool {
        match r { Some(left) => left as int + n as int == holding as int, None => n > holding }
    }

    fn two_requests_share_one_budget(holding: u64, a: u64, b: u64) {
        if let Some(first) = retire(holding, a) {
            if let Some(last) = retire(first, b) {
                assert(last as int + a as int + b as int == holding as int);
            }
        }
    }

    fn partition_independent(origin: u64, cursor: u64, available: u64, mid: u64, target: u64, holding: u64) {
        if let Ok(first) = advance(origin, cursor, available, mid, holding) {
            if let Ok(last) = advance(origin, first.cursor, available, target, first.holding) {
                let whole = advance(origin, cursor, available, target, holding);
                assert(whole is Ok);
                if let Ok(w) = whole {
                    assert(w.holding == last.holding && w.cursor == last.cursor);
                    assert(w.delta as int == first.delta as int + last.delta as int);
                }
                let retry = advance(origin, last.cursor, available, mid, last.holding);
                assert(retry is Err);
            }
        }
    }

    fn immediate_and_deferred_agree(receipt: u64, holding: u64, n: u64) {
        if let Some(i) = record(receipt, holding, false, n) {
            if n > 0 {
                let deferred = record(receipt, holding, true, n);
                assert(deferred is Some);
                if let Some(d) = deferred {
                    let credit = advance(receipt, receipt, d.receipt, d.receipt, holding);
                    assert(credit is Ok);
                    if let Ok(c) = credit { assert(i.immediate == Some(c.holding) && c.delta == n); }
                }
            }
        }
    }

    fn reachable_credit() {
        let d = record(10, 20, true, 3);
        assert(d is Some);
        let c = advance(10, 10, 13, 13, 20);
        assert(c is Ok);
        if let Ok(c) = c { assert(c.holding == 23 && c.delta == 3); }
    }
    }
}
