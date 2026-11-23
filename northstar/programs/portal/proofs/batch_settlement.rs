#![allow(dead_code)]
#[path = "../src/lamport_batch.rs"]
mod lamport_batch;

mod batch_spec {
    use super::lamport_batch::{self, *};
    use vstd::prelude::*;

    verus! {
    pub(super) open spec fn row(t: Tally, e: Row, r: Result<Tally, BatchError>) -> bool {
        let current = t.current as int + e.current as int;
        let target = t.target as int + e.target as int;
        if e.target < e.floor { r == Err(BatchError::Rent) }
        else if current > u128::MAX as int || target > u128::MAX as int { r == Err(BatchError::Overflow) }
        else { match r {
            Ok(n) => n.current as int == current && n.target as int == target
                && n.unchanged == (t.unchanged && e.current == e.target),
            Err(_) => false,
        } }
    }

    pub(super) open spec fn effect(t: Tally, v: Option<u64>, r: Result<Effect, BatchError>) -> bool {
        let surplus = t.current as int - t.target as int;
        if t.unchanged && v is None { r == Ok(Effect::Noop) }
        else if surplus < 0 || (v is None && surplus != 0) { r == Err(BatchError::Unbalanced) }
        else { match v {
            None => r == Ok(Effect::Write(None)),
            Some(v) => if v as int + surplus > u64::MAX as int { r == Err(BatchError::Overflow) }
                else { match r {
                    Ok(Effect::Write(Some(n))) => n as int + t.target as int == v as int + t.current as int,
                    _ => false,
                } },
        } }
    }

    pub(super) open spec fn decision(a: &[u8; 32], p: &[u8; 32], n: &[u8; 32], r: Guard) -> bool {
        r == (if a@ =~= n@ { Guard::Retry } else if a@ =~= p@ { Guard::Apply } else { Guard::Mismatch })
    }

    spec fn sum(s: Seq<Row>, k: nat, target: bool) -> int decreases k {
        if k == 0 { 0 } else {
            sum(s, (k - 1) as nat, target) + if target { s[k-1].target as int } else { s[k-1].current as int }
        }
    }

    // The instruction accepts one to seven distinct accounts. This induction includes
    // the empty tally and shows the u128 accumulator cannot overflow on legal batches.
    fn bounded_batch(rows: &[Row], v: Option<u64>)
        requires rows.len() <= 7, forall|j: int| 0 <= j < rows.len() ==> #[trigger] rows@[j].target >= rows@[j].floor,
    {
        let mut t = empty();
        let mut i: usize = 0;
        while i < rows.len()
            invariant
                i <= rows.len() <= 7,
                t.current as int == sum(rows@, i as nat, false), t.target as int == sum(rows@, i as nat, true),
                t.current as int <= i as int * u64::MAX as int, t.target as int <= i as int * u64::MAX as int,
                t.unchanged == (forall|j: int| 0 <= j < i ==> #[trigger] rows@[j].current == rows@[j].target),
                t.unchanged ==> t.current == t.target,
                forall|j: int| 0 <= j < rows.len() ==> #[trigger] rows@[j].target >= rows@[j].floor,
            decreases rows.len() - i,
        {
            let n = lamport_batch::add(t, rows[i]);
            assert(n is Ok);
            if let Ok(n) = n { t = n; }
            i += 1;
        }
        let r = finish(t, v);
        if let Ok(Effect::Write(Some(n))) = r {
            assert(v is Some);
            assert(n as int + sum(rows@, i as nat, true) == v.unwrap() as int + sum(rows@, i as nat, false));
        }
        if r matches Ok(Effect::Noop) { assert(v is None && sum(rows@, i as nat, true) == sum(rows@, i as nat, false)); }
        if r matches Ok(Effect::Write(None)) { assert(sum(rows@, i as nat, true) == sum(rows@, i as nat, false)); }
    }

    fn guarded_replay(p: &[u8; 32], n: &[u8; 32]) {
        let r = guard(n, p, n);
        assert(r == Guard::Retry);
    }

    fn reachable_surplus() {
        let r = lamport_batch::add(empty(), Row { current: 12, target: 10, floor: 10 });
        assert(r is Ok);
        if let Ok(t) = r {
            let e = finish(t, Some(5));
            assert(e == Ok::<Effect, BatchError>(Effect::Write(Some(7))));
        }
    }
    }
}
