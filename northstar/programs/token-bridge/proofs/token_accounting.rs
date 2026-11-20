#![allow(dead_code)]
#[path = "../src/accounting.rs"]
mod accounting;

mod bridge_spec {
    use super::accounting::*;
    use vstd::prelude::*;

    verus! {
    pub(super) open spec fn access(signed: bool, source: &[u8; 32], dest: &[u8; 32],
        result: Result<(), BridgeError>) -> bool {
        result == (if !signed { Err(BridgeError::MissingSignature) }
            else if source@ =~= dest@ { Err(BridgeError::Binding) }
            else { Ok(()) })
    }

    pub(super) open spec fn transfer(authority: &[u8; 32], owner: &[u8; 32], source: Holding,
        dest: Holding, amount: u64, result: Result<(u64, u64), BridgeError>) -> bool {
        let bound = (authority@ =~= owner@) && (source.session_bridge@ =~= dest.session_bridge@)
            && (source.mint@ =~= dest.mint@);
        let credit_fits = dest.amount as int + amount as int <= u64::MAX as int;
        &&& (result is Ok) == (bound && amount <= source.amount && credit_fits)
        &&& match result {
            Ok((s, d)) => {
                &&& s as int == source.amount as int - amount as int
                &&& d as int == dest.amount as int + amount as int
                &&& s as int + d as int == source.amount as int + dest.amount as int
            },
            Err(e) => e == (if !bound { BridgeError::Binding }
                else if source.amount < amount { BridgeError::Insufficient } else { BridgeError::Overflow }),
        }
    }

    pub(super) open spec fn withdrawal(stored: u64, deposited: u64, proposed: u64, amount: u64,
        result: Result<Option<u64>, BridgeError>) -> bool {
        let exact = stored as int + amount as int;
        if proposed <= stored { result == Ok(None) }
        else if exact > u64::MAX as int { result == Err(BridgeError::Overflow) }
        else if proposed as int == exact && proposed <= deposited { result == Ok(Some(proposed)) }
        else { result == Err(BridgeError::Withdrawal) }
    }

    fn retry_cannot_request_payment(stored: u64, deposited: u64, proposed: u64, amount: u64, retry_amount: u64) {
        if let Ok(Some(next)) = withdrawal_target(stored, deposited, proposed, amount) {
            assert(next > stored && next <= deposited && amount > 0);
            let retry = withdrawal_target(next, deposited, proposed, retry_amount);
            assert(retry == Ok::<Option<u64>, BridgeError>(None));
        }
    }

    fn reachable_steps() {
        let a = [1u8; 32]; let b = [2u8; 32];
        assert(a@[0] != b@[0]);
        let access = transfer_access(true, &a, &b);
        assert(access is Ok);
        let balances = transfer_balances(&a, &a,
            Holding { session_bridge: &a, mint: &b, amount: 6 },
            Holding { session_bridge: &a, mint: &b, amount: 0 }, 6);
        assert(balances == Ok::<(u64,u64), BridgeError>((0,6)));
        let w = withdrawal_target(4, 10, 10, 6);
        assert(w == Ok::<Option<u64>, BridgeError>(Some(10)));
    }
    }
}
