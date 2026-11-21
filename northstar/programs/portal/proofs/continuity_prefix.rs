#![allow(dead_code)]
#[path = "../src/ingress_prefix.rs"]
mod ingress_prefix;

mod prefix_spec {
    use super::ingress_prefix::*;
    use vstd::prelude::*;

    verus! {
    pub(super) open spec fn increment(count: u64, r: Option<u64>) -> bool {
        match r { Some(k) => k as int == count as int + 1, None => count == u64::MAX }
    }

    pub(super) open spec fn allowed(c: Head, t: Head, a: Head, b: Head, steps: u64) -> bool {
        &&& c.count == a.count && (c.root@ =~= a.root@)
        &&& a.count <= b.count <= t.count && b.count as int - a.count as int <= steps as int
        &&& a.count == b.count ==> (a.root@ =~= b.root@)
        &&& b.count == t.count ==> (b.root@ =~= t.root@)
    }

    fn cannot_apply_advancing_prefix_twice(c: Head, t: Head, a: Head, b: Head, steps: u64) {
        if permits(c, t, a, b, steps) && b.count > a.count {
            let again = permits(b, t, a, b, steps);
            assert(!again);
        }
    }

    fn adjacent_budgets_add(c: Head, t: Head, a: Head, b: Head, d: Head, first: u64, second: u64) {
        if permits(c, t, a, b, first) && permits(b, t, b, d, second) {
            assert(c.count <= b.count <= d.count <= t.count);
            assert(d.count as int - c.count as int <= first as int + second as int);
        }
    }

    fn reachable_prefixes(root: &[u8; 32], later: &[u8; 32]) {
        let c = Head { count: 9, root };
        let t = Head { count: 12, root: later };
        let ok = permits(c, t, c, t, 3);
        assert(ok);
        let noop = permits(c, t, c, c, 0);
        assert(noop);
        let exhausted = next_sequence(u64::MAX);
        assert(exhausted is None);
    }
    }
}
