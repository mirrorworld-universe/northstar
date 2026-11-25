#![allow(dead_code)]
#![feature(proc_macro_hygiene)]
#[path = "../src/trace_origin.rs"]
mod trace_origin;
#[allow(unused_attributes)]
#[path = "recipient_entitlement.rs"]
mod matched;
use matched::{auth_spec, bridge_spec, custody_spec, entitlement, entitlement_spec, payout_wire, wire_spec};
extern crate self as northstar_portal;

pub mod origin_spec {
    use super::{trace_origin::*, entitlement::*, entitlement_spec};
    use vstd::prelude::*;
    verus! {
    pub open spec fn word(b: Seq<u8>) -> u64 {
        (b[1] as u64) | ((b[2] as u64) << 8u64) | ((b[3] as u64) << 16u64) | ((b[4] as u64) << 24u64)
        | ((b[5] as u64) << 32u64) | ((b[6] as u64) << 40u64) | ((b[7] as u64) << 48u64) | ((b[8] as u64) << 56u64)
    }
    pub open spec fn opening(b: Seq<u8>, r: Option<(u64,u8)>) -> bool {
        r == if b.len()==10 && b[0]==7 { Some((word(b),b[9])) } else { None }
    }
    pub open spec fn fields(b:Seq<u8>, a:Addresses, i:Intent) -> bool {
        &&& i.bridge@ =~= a.program@ && i.registry@ =~= a.registry@ && i.source@ =~= a.source@
        &&& i.mint@ =~= a.mint@ && i.destination@ =~= a.destination@
        &&& i.amount == word(b) && i.decimals == b[9]
    }
    pub open spec fn issued(b:Seq<u8>, origin:Origin, required:u8, who:u8, a:Addresses, h:Source, r:Option<Intent>) -> bool {
        &&& (r is Some) == (b.len()==10 && b[0]==7 && word(b)>0 && authority(origin,required,who)
            && (a.actor@ =~= h.owner@) && (a.registry@ =~= h.registry@) && (a.mint@ =~= h.mint@))
        &&& r matches Some(i) ==> fields(b,a,i)
    }
    // Any matched occurrence preserves the decoded call, not merely the sum of payouts.
    fn retained_row(data:&[u8], origin:Origin, required:u8, who:u8, a:Addresses, h:Source,
        calls:&[Intent], effects:&[Intent], row:usize) -> (accepted:bool)
        requires row < calls.len(), row < effects.len(),
        ensures accepted ==> (fields(data@, a, effects@[row as int]) && (a.actor@ =~= h.owner@)
                && effects@[row as int].amount > 0 && (origin is Message ==> who < required)),
            (entitlement_spec::aligned(calls@,effects@) && issued(data@,origin,required,who,a,h,Some(calls@[row as int]))) ==> accepted,
    {
        if let Some(r) = request(data, origin, required, who, a, h) {
            let paired = match_occurrences(&[r], &[calls[row]]) && match_occurrences(calls,effects);
            if paired {assert(entitlement_spec::same(&calls@[row as int],&effects@[row as int]));}
            paired
        } else {false}
    }
    pub open spec fn subset(child: Seq<u8>, parent: Seq<u8>) -> bool {
        forall|i:int| 0 <= i < child.len() ==> parent.contains(child[i])
    }
    pub open spec fn parent(open: usize, height:u32, result: Option<usize>) -> bool {
        &&& (result is Some) == (2 <= height && height as int <= open as int + 1)
        &&& result matches Some(n) ==> n as int + 1 == height && 1 <= n <= open
    }
    pub open spec fn authority(origin: Origin, required: u8, index:u8) -> bool {
        origin is Invocation || (index as int) < required as int
    }
    proof fn transitive(a: Seq<u8>, b: Seq<u8>, c: Seq<u8>)
        requires subset(a,b), subset(b,c), ensures subset(a,c), {
        assert forall|i:int| 0 <= i < a.len() implies c.contains(a[i]) by {
            let j = choose|j:int| 0 <= j < b.len() && b[j] == a[i];
            assert(c.contains(b[j]));
        }
    }
    pub(crate) proof fn inherited(frames: Seq<Seq<u8>>, i:nat)
        requires i < frames.len(), forall|j:int| 0 < j <= i ==> #[trigger] subset(frames[j], frames[j-1]),
        ensures subset(frames[i as int], frames[0]), decreases i, {
        if i > 0 {
            inherited(frames, (i-1) as nat);
            transitive(frames[i as int], frames[i-1], frames[0]);
        }
    }
    fn siblings_and_signers() {
        let outer = [0u8,3,6]; let child = [6u8,3,6]; let absent = [2u8];
        let a = visible(&child,&outer); let b = visible(&absent,&outer);
        assert(a && !b);
        let down = retain_parent(1,2); let sibling = retain_parent(3,2); let gap = retain_parent(1,3);
        assert(down == Some(1usize) && sibling == Some(1usize) && gap is None);
        let signed = permits(Origin::Message,1,0); let unsigned = permits(Origin::Message,1,1);
        let cpi = permits(Origin::Invocation,1,1); assert(signed && !unsigned && cpi);
        let b=[7u8,3,0,0,0,0,0,0,0,6]; let owner=[1u8;32]; let registry=[2u8;32]; let mint=[3u8;32];
        let recipient=[4u8;32]; let key=[5u8;32];
        let a=Addresses{actor:&owner, program:&key, registry:&registry, source:&owner, destination:&recipient, mint:&mint};
        let h=Source{owner:&owner,registry:&registry,mint:&mint};
        assert((0u64 << 8u64) == 0 && (0u64 << 16u64) == 0 && (0u64 << 24u64) == 0
            && (0u64 << 32u64) == 0 && (0u64 << 40u64) == 0 && (0u64 << 48u64) == 0
            && (0u64 << 56u64) == 0 && (3u64 | 0u64) == 3u64) by (bit_vector);
        assert(word(b@)==3u64);
        let i=Intent{bridge:key,registry,source:owner,destination:recipient,mint,amount:3,decimals:6};
        let row=[i,i];
        let direct=retained_row(&b,Origin::Message,1,0,a,h,&row,&row,1);
        let via=retained_row(&b,Origin::Invocation,1,3,a,h,&row,&row,0);
        assert(direct && via);
    }
    }
}
