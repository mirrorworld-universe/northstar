#![allow(dead_code)]
// Recheck the implementation and specifications used by the four component suites.
#[path = "../src/liability.rs"]
mod liability;
#[path = "vault_custody.rs"]
mod funds;
use funds::{custody, custody_spec};
#[path = "delegated_credit.rs"]
mod credits;
use credits::{credit_spec, deposit_credit};
#[path = "token_accounting.rs"]
mod bridge;
use bridge::{accounting, bridge_spec};
#[path = "../../portal/proofs/token_authorization.rs"]
mod portal;
use portal::{auth_spec, withdrawal_auth};

pub mod liability_spec {
    use super::{accounting::*, custody::*, deposit_credit::*, liability::*, withdrawal_auth::*};
    use vstd::prelude::*;

    verus! {
    pub type Totals = (int, int, int, int); // before, after, authenticated credits, new unpaid claims
    pub open spec fn zero(f: Flow) -> bool { f.totals() == (0int, 0int, 0int, 0int) }
    pub open spec fn plus(v: Totals, e: Entry) -> Totals {
        match e {
            Entry::Before(n)  => (v.0 + n as int, v.1, v.2, v.3),
            Entry::After(n)   => (v.0, v.1 + n as int, v.2, v.3),
            Entry::Ingress(n) => (v.0, v.1, v.2 + n as int, v.3),
            Entry::Pending(n) => (v.0, v.1, v.2, v.3 + n as int),
        }
    }
    pub open spec fn add(f: Flow, entry: Entry, result: Option<Flow>) -> bool {
        let n = plus(f.totals(), entry);
        let fits = n.0 <= u128::MAX as int && n.1 <= u128::MAX as int
            && n.2 <= u128::MAX as int && n.3 <= u128::MAX as int;
        &&& (result is Some) == fits
        &&& match result { Some(x) => x.totals() == n, None => true }
    }
    pub open spec fn balanced(f: Flow) -> bool {
        let v = f.totals();
        v.0 + v.2 == v.1 + v.3 && v.0 + v.2 <= u128::MAX as int
    }
    pub open spec fn observation(planned: Ticket, seen: Option<(Ticket, bool)>, result: Option<bool>) -> bool {
        result == match seen {
            None => Some(false),
            Some((a, consumed)) => if (a.checkpoint@ =~= planned.checkpoint@) && (a.tuple_hash@ =~= planned.tuple_hash@)
                && a.bump == planned.bump { Some(consumed) } else { None },
        }
    }
    spec fn sum(rows: Seq<Entry>, k: nat) -> Totals decreases k {
        if k == 0 { (0int, 0int, 0int, 0int) } else { plus(sum(rows, (k-1) as nat), rows[k-1]) }
    }
    spec fn bounded(t: Totals, i: int) -> bool {
        0 <= t.0 && 0 <= t.1 && 0 <= t.2 && 0 <= t.3 && t.0+t.1+t.2+t.3 <= i * u64::MAX as int
    }
    fn fold_all_entries(entries: &[Entry]) {
        let mut flow = empty_flow();
        let mut i: usize = 0;
        while i < entries.len()
            invariant i <= entries.len(), flow.totals() == sum(entries@, i as nat), bounded(flow.totals(), i as int),
            decreases entries.len() - i,
        {
            let next = flow.record(entries[i]);
            assert(next is Some);
            if let Some(n) = next { flow = n; }
            i += 1;
        }
        let accepted = flow.conserved();
        let ghost s = sum(entries@, entries.len() as nat);
        assert(accepted == (s.0 + s.2 == s.1 + s.3));
    }

    // A complete logical cut: one spendable copy per account, one uncredited
    // delta per receipt, one unpaid claim per withdrawal, not per representation.
    struct Claims { h: int, u: int, p: int }
    spec fn safe(c: Claims, l: Ledger, v: u64) -> bool {
        &&& 0 <= c.h && 0 <= c.u && 0 <= c.p
        &&& c.h + c.u + c.p == l.deposited as int - l.withdrawn as int
        &&& c.h + c.u + c.p <= v as int
    }
    spec fn amount(s: Seq<u64>, k: nat) -> nat decreases k {
        if k == 0 { 0 } else { amount(s, (k-1) as nat) + s[k-1] as nat }
    }
    proof fn replace_one(s: Seq<u64>, k: nat, idx: int, value: u64)
        requires k <= s.len(), 0 <= idx < s.len(),
        ensures amount(s.update(idx, value), k) as int == amount(s, k) as int
            + (if idx < k { value as int - s[idx] as int } else { 0int }),
        decreases k,
    {
        if k > 0 { replace_one(s, (k-1) as nat, idx, value); }
    }
    fn genesis(v: u64) {
        let l = empty();
        assert(safe(Claims { h: 0, u: 0, p: 0 }, l, v));
    }

    fn funded_deposit(l: Ledger, b: Amounts, a: Amounts, n: u64, receipt: u64, holding: u64, deferred: bool, c: Ghost<Claims>)
        requires safe(c@, l, b.vault), holding as int <= c@.h,
    {
        if let Ok(expect) = prepare(Direction::Deposit, l, b, n) {
            if let Ok(post) = expect.confirm(a) {
                if let Some(book) = record(receipt, holding, deferred, n) {
                    match book.immediate {
                        Some(h) => assert(safe(Claims { h: c@.h - holding as int + h as int, u: c@.u, p: c@.p }, post, a.vault)),
                        None => assert(safe(Claims { h: c@.h, u: c@.u + book.receipt as int - receipt as int, p: c@.p }, post, a.vault)),
                    }
                }
            }
        }
    }

    fn consume_deferred(l: Ledger, vault: u64, origin: u64, cursor: u64, available: u64, target: u64, holding: u64, c: Ghost<Claims>)
        requires safe(c@, l, vault), holding as int <= c@.h, available as int - cursor as int <= c@.u,
    {
        if let Ok(credit) = advance(origin, cursor, available, target, holding) {
            let ghost other_receipts = c@.u - (available as int - cursor as int);
            assert(safe(Claims { h: c@.h - holding as int + credit.holding as int,
                u: other_receipts + available as int - credit.cursor as int, p: c@.p }, l, vault));
        }
    }

    fn accept_er_transaction(f: Flow, l: Ledger, vault: u64, c: Ghost<Claims>)
        requires safe(c@, l, vault), 0 <= f.totals().0 <= c@.h, 0 <= f.totals().2 <= c@.u,
            0 <= f.totals().1, 0 <= f.totals().3,
    {
        let ok = f.conserved();
        let ghost t = f.totals();
        if ok {
            // Untouched balances cancel, even in transactions mixing multiple transfers and withdrawals.
            assert(safe(Claims { h: c@.h - t.0 + t.1, u: c@.u - t.2, p: c@.p + t.3 }, l, vault));
        }
    }
    fn direct_payout(l: Ledger, b: Amounts, a: Amounts, n: u64, holding: u64, c: Ghost<Claims>)
        requires safe(c@, l, b.vault), holding as int <= c@.h,
    {
        if let Some(h) = retire(holding, n) {
            if let Ok(expect) = prepare(Direction::Payout, l, b, n) {
                if let Ok(post) = expect.confirm(a) {
                    assert(safe(Claims { h: c@.h - holding as int + h as int, u: c@.u, p: c@.p }, post, a.vault));
                }
            }
        }
    }

    // idx identifies the pending logical effect whose tuple is being consumed. Retirement
    // replaces just that entry with zero; all other pending claims remain outstanding.
    fn settle_pending(l: Ledger, b: Amounts, a: Amounts, n: u64, target: u64, used: bool,
        stored: AuthorizationKey, requested: AuthorizationKey, c: Ghost<Claims>, tickets: Ghost<Seq<u64>>, idx: Ghost<int>)
        requires safe(c@, l, b.vault), 0 <= idx@ < tickets@.len(), tickets@[idx@] == n,
            amount(tickets@, tickets@.len()) == c@.p,
    {
        if let Ok(Some(next)) = withdrawal_target(l.withdrawn, l.deposited, target, n) {
            let mut bit = used;
            if mark_consumed(&mut bit, stored, requested) {
                if let Ok(expect) = prepare(Direction::Payout, l, b, n) {
                    if let Ok(post) = expect.confirm(a) {
                        proof { replace_one(tickets@, tickets@.len(), idx@, 0); }
                        let ghost unpaid = amount(tickets@.update(idx@, 0), tickets@.len());
                        assert(bit && !used && post.withdrawn == next);
                        let planned = Ticket { checkpoint: requested.checkpoint, tuple_hash: requested.tuple_hash, bump: requested.bump };
                        let actual = Ticket { checkpoint: stored.checkpoint, tuple_hash: stored.tuple_hash, bump: stored.bump };
                        let posted = reached(post.withdrawn, target);
                        let retired = paid_authorization(planned, Some((actual, bit)));
                        assert(posted && retired == Some(true));
                        assert(safe(Claims { h: c@.h, u: c@.u, p: unpaid as int }, post, a.vault));
                        let again = mark_consumed(&mut bit, stored, requested);
                        assert(!again);
                        let retry = withdrawal_target(post.withdrawn, post.deposited, target, n);
                        assert(retry == Ok::<Option<u64>, BridgeError>(None));
                    }
                }
            }
        }
    }
    proof fn handoff_or_donation(c: Claims, l: Ledger, v: u64, next_v: u64, old_copy: u64, new_copy: u64)
        requires safe(c, l, v), v <= next_v, old_copy as int <= c.h, old_copy == new_copy,
        ensures safe(Claims { h: c.h - old_copy as int + new_copy as int, u: c.u, p: c.p }, l, next_v),
    {}

    fn outstanding_until_paid(posted: u64, target: u64, plan: Ticket, seen: Option<(Ticket, bool)>) {
        let enough = reached(posted, target);
        let receipt = paid_authorization(plan, seen);
        if enough && receipt == Some(true) {
            assert(posted >= target && seen is Some && seen.unwrap().1);
            assert(seen.unwrap().0.checkpoint@ =~= plan.checkpoint@);
            assert(seen.unwrap().0.tuple_hash@ =~= plan.tuple_hash@);
            assert(seen.unwrap().0.bump == plan.bump);
        }
    }
    fn reachable_gate() {
        let entries = [Entry::Before(12), Entry::After(9), Entry::Pending(3)];
        let f = empty_flow();
        let a = f.record(entries[0]); assert(a is Some);
        if let Some(a) = a {
            let b = a.record(entries[1]); assert(b is Some);
            if let Some(b) = b {
                let incomplete = b.conserved(); assert(!incomplete);
                let d = b.record(entries[2]); assert(d is Some);
                if let Some(d) = d { let pass = d.conserved(); assert(pass); }
            }
        }
    }
    }
}
