//! Verify the functions used by Portal's SOL receipt handler and v2 frame validation.
#![allow(dead_code)]
#[path = "../src/receipt.rs"]
mod receipt;

mod receipt_spec {
    use super::receipt::*;
    use vstd::prelude::*;

    verus! {
    pub(super) open spec fn duplicate(old: Counters, next: Counters) -> bool {
        old.balance == next.balance && old.withdrawn >= next.withdrawn
    }

    pub(super) open spec fn classification(old: Counters, next: Counters, claimed: u64,
        result: Result<Option<u64>, ReceiptError>) -> bool {
        if duplicate(old, next) { result == Ok(None) }
        else if next.withdrawn as int == old.withdrawn as int + claimed as int {
            result == Ok(Some(claimed))
        } else { result == Err(ReceiptError::InvalidUpdate) }
    }

    pub(super) open spec fn funding(next: Counters, payout: u64, before: Lamports, rent: u64,
        cap: Option<u64>, same_account: bool, result: Result<Lamports, ReceiptError>) -> bool {
        let backed = rent as int + next.balance as int + payout as int <= before.receipt as int;
        let credit_fits = before.recipient as int + payout as int <= u64::MAX as int;
        let capped = match cap {
            None => true, Some(total) => next.balance as int + next.withdrawn as int <= total as int,
        };
        &&& (result is Ok) == ((!same_account || payout == 0) && backed && credit_fits && capped)
        &&& match result {
            Ok(after) => {
                &&& after.receipt as int == before.receipt as int - payout as int
                &&& after.recipient as int == before.recipient as int + payout as int
                &&& rent as int + next.balance as int <= after.receipt as int
                &&& after.receipt as int + after.recipient as int == before.receipt as int + before.recipient as int
            },
            Err(e) => e == (if same_account && payout != 0 { ReceiptError::InvalidUpdate }
                else if !backed { ReceiptError::InsufficientBacking }
                else if !credit_fits { ReceiptError::Overflow }
                else { ReceiptError::DepositCap }),
        }
    }

    // Compose the exact two calls made by the handler. Replaying the persisted pair
    // returns no action even when the retry claims a different amount.
    fn settled_update_cannot_pay_twice(old: Counters, next: Counters, claimed: u64,
        before: Lamports, rent: u64, cap: Option<u64>, alias: bool, retry_claim: u64) {
        if let Ok(Some(delta)) = payout_delta(old, next, claimed) {
            if let Ok(after) = plan_payment(next, delta, before, rent, cap, alias) {
                assert(next.withdrawn >= old.withdrawn);
                assert(after.recipient as int - before.recipient as int == next.withdrawn as int - old.withdrawn as int);
                let replay = payout_delta(next, next, retry_claim);
                assert(replay == Ok::<Option<u64>, ReceiptError>(None));
            }
        }
    }

    fn reachable_receipts() {
        let old = Counters { balance: 10, withdrawn: 1 };
        let next = Counters { balance: 6, withdrawn: 5 };
        let delta = payout_delta(old, next, 4);
        assert(delta == Ok::<Option<u64>, ReceiptError>(Some(4)));
        let before = Lamports { receipt: 30, recipient: 7 };
        let paid = plan_payment(next, 4, before, 20, Some(11), false);
        assert(paid is Ok);
        let zero_update = payout_delta(old, Counters { balance: 9, withdrawn: 1 }, 0);
        assert(zero_update == Ok::<Option<u64>, ReceiptError>(Some(0)));
        let older = payout_delta(next, Counters { balance: 6, withdrawn: 2 }, u64::MAX);
        assert(older == Ok::<Option<u64>, ReceiptError>(None));
    }
    }
}
