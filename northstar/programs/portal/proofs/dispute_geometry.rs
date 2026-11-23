#![allow(dead_code)]
#[path = "../src/dispute_range.rs"]
mod dispute_range;

mod range_spec {
    use super::dispute_range::*;
    use vstd::prelude::*;

    verus! {
    pub(super) open spec fn width(r: Range) -> int { r.hi as int - r.lo as int }

    pub(super) open spec fn response(s: Range, r: Option<Response>) -> bool {
        match r {
            None => width(s) <= 0,
            Some(Response::Step) => width(s) == 1,
            Some(Response::Mid(m)) => width(s) > 1 && m as int == s.lo as int + width(s) / 2 && s.lo < m < s.hi,
        }
    }

    pub(super) open spec fn selection(s: Range, m: u64, upper: bool, round: u32, r: Result<(Range, u32), RangeError>) -> bool {
        if !(s.lo < m < s.hi) { r == Err(RangeError::Invalid) }
        else if round == u32::MAX { r == Err(RangeError::Overflow) }
        else { match r {
            Ok((n, k)) => k as int == round as int + 1 && n.lo == (if upper { m } else { s.lo })
                && n.hi == (if upper { s.hi } else { m }) && 0 < width(n) < width(s),
            Err(_) => false,
        } }
    }

    pub(super) open spec fn deadline(now: u64, hard: u64, window: u64, r: Option<u64>) -> bool {
        let end = now as int + window as int;
        match r {
            None => end > u64::MAX as int,
            Some(d) => end <= u64::MAX as int && d as int == (if end < hard as int { end } else { hard as int }),
        }
    }

    fn both_halves(s: Range, round: u32) {
        if let Some(Response::Mid(m)) = respond(s) {
            if round < u32::MAX {
                let a = select_half(s, m, false, round);
                let b = select_half(s, m, true, round);
                assert(a is Ok && b is Ok);
                if let (Ok((a, _)), Ok((b, _))) = (a, b) {
                    assert(a.lo == s.lo && a.hi == b.lo && b.hi == s.hi);
                    assert(width(a) + width(b) == width(s));
                    assert(width(a) <= width(b) <= width(a) + 1);
                    assert(width(b) <= (width(s) + 1) / 2);
                }
            }
        }
    }

    fn four_rounds(s: Range, choices: [bool; 4]) requires width(s) == 16 {
        let mut cur = s;
        let mut i: usize = 0;
        while i < 4
            invariant i <= 4, s.lo <= cur.lo, cur.hi <= s.hi,
                width(cur) == (if i == 0 { 16int } else if i == 1 { 8 } else if i == 2 { 4 } else if i == 3 { 2 } else { 1 }),
            decreases 4 - i,
        {
            let r = respond(cur);
            assert(r matches Some(Response::Mid(_)));
            if let Some(Response::Mid(m)) = r {
                let n = select_half(cur, m, choices[i], i as u32);
                assert(n is Ok);
                if let Ok((next, _)) = n { cur = next; }
            }
            i += 1;
        }
        let single = isolated(cur);
        assert(single);
    }

    fn deadline_partition(now: u64, turn: u64, hard: u64) {
        let permitted = live(now, turn, hard);
        assert(!permitted == (now >= turn || now >= hard));
        let edge = live(turn, turn, hard);
        assert(!edge);
    }

    fn reachable_rounds() {
        four_rounds(Range { lo: 0, hi: 16 }, [true, false, true, false]);
        let end = next_deadline(100, 102, 750);
        assert(end == Some(102u64));
    }
    }
}
