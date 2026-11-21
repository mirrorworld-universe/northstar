#![allow(dead_code)]
#[path = "token_accounting.rs"]
mod bridge;
#[path = "../src/payout_schedule.rs"]
mod payout_schedule;
use bridge::{accounting, bridge_spec};

pub mod schedule_spec {
    use super::{accounting::*, payout_schedule::*};
    use vstd::prelude::*;

    verus! {
    pub open spec fn reservation(before: u64, after: u64, cap: u64, amount: u64, r: Option<u64>) -> bool {
        let next = before as int + amount as int;
        if amount > 0 && next <= cap as int {
            r == Some(after) && after as int == next && after > before
        } else {
            r is None && after == before
        }
    }

    spec fn total(s: Seq<u64>, k: nat) -> int decreases k {
        if k == 0 { 0 } else { total(s, (k-1) as nat) + s[k-1] as int }
    }

    spec fn funded(base: u64, cap: u64, events: Seq<u64>) -> bool {
        &&& base as int + total(events, events.len()) <= cap as int
        &&& forall|i: int| 0 <= i < events.len() ==> #[trigger] events[i] > 0
    }

    proof fn prefix_order(s: Seq<u64>, k: nat, end: nat)
        requires k <= end <= s.len(),
        ensures total(s, k) <= total(s, end),
        decreases end-k,
    {
        if k < end { prefix_order(s, k, (end-1) as nat); }
    }

    fn allocate_and_post(base: u64, cap: u64, events: &[u64]) -> (last: u64)
        requires funded(base, cap, events@),
        ensures last as int == base as int + total(events@, events@.len()),
    {
        let mut planned = base;
        let mut posted = base;
        let mut i = 0usize;
        while i < events.len()
            invariant funded(base, cap, events@), i <= events.len(),
                planned == posted, planned as int == base as int + total(events@, i as nat),
            decreases events.len()-i,
        {
            proof { prefix_order(events@, (i+1) as nat, events@.len()); }
            let r = reserve(&mut planned, cap, events[i]);
            assert(r is Some);
            if let Some(target) = r {
                let verdict = withdrawal_target(posted, cap, target, events[i]);
                assert(verdict == Ok::<Option<u64>, BridgeError>(Some(target)));
                posted = target;
            }
            i += 1;
        }
        posted
    }

    fn at_any_frontier(prior: u64, cap: u64, amount: u64, posted: u64) {
        let mut cursor = prior;
        if let Some(target) = reserve(&mut cursor, cap, amount) {
            let r = withdrawal_target(posted, cap, target, amount);
            if posted < prior || (prior < posted && posted < target) { assert(r is Err); }
            if posted == prior { assert(r == Ok::<Option<u64>, BridgeError>(Some(target))); }
            if target <= posted { assert(r == Ok::<Option<u64>, BridgeError>(None)); }
        }
    }

    // Replay the original plan from any paid prefix, not a new plan based on the partly paid counter.
    fn split_plan(base: u64, cap: u64, events: &[u64], cut: usize)
        requires cut <= events.len(), funded(base, cap, events@),
    {
        let mut planned = base;
        let mut frontier = base;
        let mut i = 0usize;
        while i < events.len()
            invariant funded(base, cap, events@), cut <= events.len(), i <= events.len(),
                planned as int == base as int + total(events@, i as nat),
                frontier as int == base as int + total(events@, (if i < cut { i } else { cut }) as nat),
            decreases events.len()-i,
        {
            proof { prefix_order(events@, (i+1) as nat, events@.len()); }
            let r = reserve(&mut planned, cap, events[i]);
            assert(r is Some);
            if i < cut { frontier = planned; }
            i += 1;
        }
        let mut posted = frontier;
        let mut target = base;
        let mut j = 0usize;
        while j < events.len()
            invariant funded(base, cap, events@), cut <= events.len(), j <= events.len(),
                target as int == base as int + total(events@, j as nat),
                posted as int == base as int + total(events@, (if j < cut { cut } else { j }) as nat),
            decreases events.len()-j,
        {
            proof {
                prefix_order(events@, (j+1) as nat, events@.len());
                if j < cut { prefix_order(events@, (j+1) as nat, cut as nat); }
            }
            let r = reserve(&mut target, cap, events[j]);
            assert(r is Some);
            let verdict = withdrawal_target(posted, cap, target, events[j]);
            if j < cut {
                assert(verdict == Ok::<Option<u64>, BridgeError>(None));
            } else {
                assert(verdict == Ok::<Option<u64>, BridgeError>(Some(target)));
                posted = target;
            }
            j += 1;
        }
        assert(posted == planned);
    }

    fn reachable_cut() {
        let events = [2u64, 3, 1];
        assert(total(events@, 3) == 6) by { reveal_with_fuel(total, 4); }
        let end = allocate_and_post(4, 10, &events);
        assert(end == 10);
        split_plan(4, 10, &events, 2);
        let mut last = u64::MAX;
        let overflow = reserve(&mut last, u64::MAX, 1);
        assert(overflow is None && last == u64::MAX);
    }
    }
}
