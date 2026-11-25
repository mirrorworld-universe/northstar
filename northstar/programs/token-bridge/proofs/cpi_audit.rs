#![allow(dead_code)]
#[path = "../src/audit_match.rs"]
mod audit_match;

pub mod audit_spec {
    use super::audit_match::*;
    use vstd::prelude::*;
    verus! {
    pub open spec fn comparison(a: Transcript, b: Transcript, n: usize, r: Comparison) -> bool {
        let e = a.execution@ =~= b.execution@;
        let w = a.writes@ =~= b.writes@;
        let f = a.effects@ =~= b.effects@;
        &&& (r is Execution) == !e
        &&& (r is Writes) == (e && !w)
        &&& (r is Effects) == (e && w && !f)
        &&& (r is NoCpiWithdrawal) == (e && w && f && n == 0)
        &&& (r is Match) == (e && w && f && n > 0)
    }
    // A positive verdict determines the byte at every position, in every component.
    fn matched_bytes(a: Transcript, b: Transcript, n: usize, i: usize) {
        let r = compare(a,b,n);
        if let Comparison::Match = r {
            assert(n > 0);
            assert(a.execution.len() == b.execution.len() && a.writes.len() == b.writes.len() && a.effects.len() == b.effects.len());
            if i < a.execution.len() {assert(a.execution@[i as int] == b.execution@[i as int]);}
            if i < a.writes.len() {assert(a.writes@[i as int] == b.writes@[i as int]);}
            if i < a.effects.len() {assert(a.effects@[i as int] == b.effects@[i as int]);}
        }
    }
    fn reachable_verdicts() {
        let a = [1u8,2]; let b = [2u8,1]; let short = [1u8];
        let x = Transcript { execution:&a, writes:&b, effects:&short };
        let yes=compare(x,x,2); let no=compare(x,x,0);
        let wrong=compare(x,Transcript{execution:&b,..x},2);
        let changed=compare(x,Transcript{writes:&a,..x},2);
        let extra=compare(x,Transcript{effects:&a,..x},2);
        assert(yes is Match && no is NoCpiWithdrawal && wrong is Execution && changed is Writes && extra is Effects);
    }
    }
}
