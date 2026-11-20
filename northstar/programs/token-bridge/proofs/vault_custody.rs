#![allow(dead_code)]
#[path = "../src/custody.rs"]
mod custody;

mod custody_spec {
    use super::custody::*;
    use vstd::prelude::*;

    verus! {
    pub(super) open spec fn net(l: Ledger) -> int { l.deposited as int - l.withdrawn as int }
    pub(super) open spec fn backed(l: Ledger, tokens: u64) -> bool { 0 <= net(l) <= tokens as int }

    pub(super) open spec fn posting(d: Direction, l: Ledger, b: Amounts, n: u64, r: Result<Expected, CustodyError>) -> bool {
        let change: int = match d { Direction::Deposit => n as int, Direction::Payout => -(n as int) };
        let deposited = l.deposited as int + if d is Deposit { n as int } else { 0 };
        let withdrawn = l.withdrawn as int + if d is Payout { n as int } else { 0 };
        let vault = b.vault as int + change;
        let peer = b.peer as int - change;
        &&& (r is Ok) == (deposited <= u64::MAX as int && withdrawn <= u64::MAX as int
            && (d is Deposit || withdrawn <= deposited) && 0 <= vault <= u64::MAX as int && 0 <= peer <= u64::MAX as int)
        &&& match r {
            Err(_) => true,
            Ok(e) => {
                &&& e.ledger().deposited as int == deposited && e.ledger().withdrawn as int == withdrawn
                &&& e.amounts().vault as int == vault && e.amounts().peer as int == peer
            },
        }
    }

    pub(super) open spec fn observed(e: Expected, a: Amounts, r: Result<Ledger, CustodyError>) -> bool {
        r == (if a == e.amounts() { Ok(e.ledger()) } else { Err(CustodyError::Movement) })
    }

    fn initial_backing(tokens: u64) {
        let l = empty();
        assert(backed(l, tokens));
    }

    // Induction step. The two post-CPI token amounts are independent, arbitrary inputs.
    fn backing_step(d: Direction, l: Ledger, b: Amounts, a: Amounts, n: u64) {
        if let Ok(e) = prepare(d, l, b, n) {
            if let Ok(new) = e.confirm(a) {
                assert(a.vault as int + a.peer as int == b.vault as int + b.peer as int);
                assert(a.vault as int - net(new) == b.vault as int - net(l));
                assert(backed(l, b.vault) ==> backed(new, a.vault));
                assert(l.withdrawn <= new.withdrawn && l.deposited <= new.deposited);
                assert(n == 0 ==> (new == l && a == b));
                assert((n > 0 && b.vault == b.peer) ==> a.vault != a.peer);
            }
        }
    }

    fn cannot_post_one_sided(d: Direction, l: Ledger, b: Amounts, a: Amounts, n: u64)
        requires n > 0, b.vault == a.vault || b.peer == a.peer,
    {
        if let Ok(e) = prepare(d, l, b, n) {
            let posted = e.confirm(a);
            assert(posted == Err::<Ledger, CustodyError>(CustodyError::Movement));
        }
    }

    fn donation_cannot_reduce_backing(l: Ledger, before: u64, after: u64) {
        assert((backed(l, before) && before <= after) ==> backed(l, after));
    }

    fn round_trip_with_surplus() {
        let l = empty();
        let b = Amounts { vault: 5, peer: 19 };
        let a = Amounts { vault: 24, peer: 0 };
        let first = prepare(Direction::Deposit, l, b, 19);
        assert(first is Ok);
        if let Ok(e) = first {
            let dep = e.confirm(a);
            assert(dep is Ok);
            if let Ok(dep) = dep {
                let last = prepare(Direction::Payout, dep, a, 19);
                assert(last is Ok);
                if let Ok(e) = last {
                    let end = e.confirm(b);
                    assert(end is Ok);
                    if let Ok(new) = end { assert(net(new) == 0 && new.withdrawn == 19); }
                }
            }
        }
    }
    }
}
