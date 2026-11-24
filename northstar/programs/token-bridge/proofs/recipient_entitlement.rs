#![allow(dead_code)]
#![feature(proc_macro_hygiene)]
#[path = "../src/entitlement.rs"]
pub(crate) mod entitlement;
#[path = "token_accounting.rs"]
mod counter;
pub(crate) use counter::{accounting, bridge_spec};
#[path = "vault_custody.rs"]
mod funds;
pub(crate) use funds::{custody, custody_spec};
#[path = "../../portal/proofs/token_authorization.rs"]
mod portal;
pub(crate) use portal::{auth_spec, withdrawal_auth};
#[path = "../../portal/proofs/payout_transcript.rs"]
mod transcript;
pub(crate) use transcript::{payout_wire, wire_spec};
extern crate self as northstar_portal;

pub mod entitlement_spec {
    use super::{accounting::*, custody::*, entitlement::*, payout_wire::Payout, wire_spec, withdrawal_auth::*};
    use vstd::prelude::*;

    verus! {
    pub open spec fn authority(signed: bool, call: Source, stored: Source) -> bool {
        signed && (call.owner@ =~= stored.owner@) && (call.registry@ =~= stored.registry@)
            && (call.mint@ =~= stored.mint@)
    }
    pub open spec fn same(a: &Intent, b: &Intent) -> bool {
        &&& a.bridge@ =~= b.bridge@
        &&& a.registry@ =~= b.registry@
        &&& a.source@ =~= b.source@
        &&& a.mint@ =~= b.mint@
        &&& a.destination@ =~= b.destination@
        &&& a.amount == b.amount && a.decimals == b.decimals
    }
    pub open spec fn projection(w: &Payout, i: &Intent) -> bool {
        &&& i.bridge@ =~= w.bridge_program@
        &&& i.registry@ =~= w.session_bridge@
        &&& i.source@ =~= w.er_token_account@
        &&& i.mint@ =~= w.mint@
        &&& i.destination@ =~= w.destination_token_account@
        &&& i.amount == w.amount && i.decimals == w.decimals
    }
    pub open spec fn issuance(signed: bool, owner: &[u8;32], stored: Source, call: Intent, r: Option<Intent>) -> bool {
        let actor = Source { owner, registry: &call.registry, mint: &call.mint };
        &&& (r is Some) == (call.amount > 0 && authority(signed, actor, stored))
        &&& match r { Some(intent) => same(&call, &intent), None => true }
    }
    pub open spec fn aligned(a: Seq<Intent>, b: Seq<Intent>) -> bool {
        a.len() == b.len() && forall|i:int| 0 <= i < a.len() ==> same(&a[i], &b[i])
    }
    pub open spec fn step(prev: Option<u64>, amount: u64, target: u64) -> bool {
        amount > 0 && match prev {
            Some(last) => target as int == last as int + amount as int,
            None => target >= amount,
        }
    }

    spec fn count(rows: Seq<Intent>, n: nat, needle: Intent) -> nat decreases n {
        if n == 0 { 0 } else {
            count(rows, (n-1) as nat, needle) + if same(&rows[n-1], &needle) { 1nat } else { 0nat }
        }
    }
    proof fn multiplicity(requests: Seq<Intent>, payouts: Seq<Intent>, n: nat, needle: Intent)
        requires aligned(requests, payouts), n <= requests.len(),
        ensures count(requests, n, needle) == count(payouts, n, needle), decreases n,
    {
        if n > 0 {
            assert(same(&requests[n-1], &payouts[n-1]));
            multiplicity(requests, payouts, (n-1) as nat, needle);
        }
    }
    fn every_occurrence(requests: &[Intent], payouts: &[Intent], needle: Intent) {
        let valid = match_occurrences(requests, payouts);
        if valid {
            proof { multiplicity(requests@, payouts@, requests@.len(), needle); }
            assert(count(requests@, requests@.len(), needle) == count(payouts@, payouts@.len(), needle));
        }
    }

    // Each successful step on one vault has a distinct, strictly greater (vault,target) ticket key.
    fn distinct_targets(targets: &[u64], amounts: &[u64], first: usize, last: usize)
        requires targets.len() == amounts.len(), first < last < targets.len(),
            forall|i:int| 0 < i < targets.len() ==> step(Some(targets@[i-1]), amounts@[i], targets@[i]),
    {
        let mut i = first;
        while i < last
            invariant targets.len() == amounts.len(), first <= i <= last < targets.len(),
                targets@[first as int] <= targets@[i as int],
                i > first ==> targets@[first as int] < targets@[i as int],
                forall|j:int| 0 < j < targets.len() ==> step(Some(targets@[j-1]), amounts@[j], targets@[j]),
            decreases last-i,
        {
            let next = i + 1;
            let valid = next_ticket(Some(targets[i]), amounts[next], targets[next]);
            assert(valid && targets@[next as int] > targets@[i as int]);
            i = next;
        }
        assert(targets@[first as int] < targets@[last as int]);
    }

    // The hash contract is explicit and local to this pair: the stored and presented
    // digests come from their respective v0 transcripts and do not collide here.
    // In particular, no input requires the two destinations, mints or amounts to agree.
    fn successful_payment(requests: &[Intent], payments: &[Intent], row: usize,
        signed: bool, owner: &[u8;32], holding: Source,
        planned: &Payout, actual: &Payout, plan_image: Ghost<Seq<u8>>, actual_image: Ghost<Seq<u8>>,
        stored: AuthorizationKey, presented: AuthorizationKey, was_consumed: bool,
        ledger: Ledger, before: Amounts, after: Amounts) -> (paid: bool)
        requires row < requests.len(), row < payments.len(), projection(planned, &payments@[row as int]),
            wire_spec::layout(planned, plan_image@), wire_spec::layout(actual, actual_image@),
            (stored.tuple_hash@ =~= presented.tuple_hash@) ==> (plan_image@ =~= actual_image@),
        ensures
            paid ==> (signed && (owner@ =~= holding.owner@) && !was_consumed
                && projection(actual, &requests@[row as int])
                && after.peer as int == before.peer as int + requests@[row as int].amount as int
                && after.vault as int == before.vault as int - requests@[row as int].amount as int),
            (aligned(requests@, payments@)
                && issuance(signed, owner, holding, requests@[row as int], Some(requests@[row as int]))
                && super::auth_spec::transition(was_consumed, true, stored, presented, true)
                && super::bridge_spec::withdrawal(ledger.withdrawn, ledger.deposited, actual.withdrawn, actual.amount, Ok(Some(actual.withdrawn)))
                && after.peer as int == before.peer as int + actual.amount as int
                && after.vault as int == before.vault as int - actual.amount as int) ==> paid,
    {
        let call = requests[row];
        let pay = from_payout(planned);
        let target = actual.withdrawn;
        if let Some(request) = issue(signed, owner, holding, call) {
            if match_occurrences(requests, payments) {
                assert(same(&requests@[row as int], &payments@[row as int]));
                assert(same(&request, &pay));
                let mut consumed = was_consumed;
                if let Ok(Some(next)) = withdrawal_target(ledger.withdrawn, ledger.deposited, target, actual.amount) {
                    if mark_consumed(&mut consumed, stored, presented) {
                        proof { wire_spec::payload_injective(planned, actual, plan_image@); }
                        if let Ok(expected) = prepare(Direction::Payout, ledger, before, actual.amount) {
                            if let Ok(post) = expected.confirm(after) {
                                assert(signed && (owner@ =~= holding.owner@) && same(&call, &pay));
                                assert(projection(actual, &call));
                                assert(!was_consumed && consumed && (stored.tuple_hash@ =~= presented.tuple_hash@));
                                assert(after.peer as int == before.peer as int + call.amount as int);
                                assert(after.vault as int == before.vault as int - call.amount as int);
                                assert(post.withdrawn == next && next == target);
                                let again = withdrawal_target(post.withdrawn, post.deposited, target, pay.amount);
                                assert(again == Ok::<Option<u64>, BridgeError>(None));
                                let reused = mark_consumed(&mut consumed, stored, presented);
                                assert(!reused);
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }
    fn reachable_and_distinct_recipients() {
        let owner = [1u8;32]; let registry = [2u8;32]; let mint = [3u8;32]; let recipient = [4u8;32];
        let holding = Source { owner: &owner, registry: &registry, mint: &mint };
        let a = Intent {bridge:[5u8;32], registry, source:[6u8;32], mint, destination:recipient, amount:7, decimals:6};
        let r = issue(true, &owner, holding, a);
        assert(r is Some && owner@[0] != recipient@[0]);
        let b = Intent {destination:owner, ..a};
        let yes = match_occurrences(&[a,a,b], &[a,a,b]);
        let no = match_occurrences(&[a,a,b], &[a,b,b]);
        assert(yes && !no);
        let k = AuthorizationKey {checkpoint:&registry, tuple_hash:&mint, bump:1};
        let mut consumed = false;
        let take = mark_consumed(&mut consumed,k,k); assert(take);
        let ledger = Ledger {deposited:20, withdrawn:3};
        let w = withdrawal_target(3,20,10,7); assert(w == Ok::<Option<u64>,BridgeError>(Some(10)));
        let w = Payout { bridge_program:&a.bridge, session_bridge:&a.registry, er_token_account:&a.source,
            mint:&a.mint, token_program:&owner, vault:&mint, vault_token_account:&registry,
            destination_token_account:&a.destination, amount:7, withdrawn:10, decimals:6 };
        let image = w.payload();
        let rows = [b, a];
        let paid = successful_payment(&rows, &rows, 1, true, &owner, holding, &w, &w, Ghost(image@), Ghost(image@),
            k, k, false, ledger, Amounts {vault:17, peer:1}, Amounts {vault:10, peer:8});
        assert(paid);
        let e = prepare(Direction::Payout,ledger,Amounts {vault:17, peer:1},7); assert(e is Ok);
        if let Ok(e) = e { let p = e.confirm(Amounts {vault:10, peer:8}); assert(p is Ok); }
    }
    }
}
