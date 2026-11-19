//! Standalone Verus crate. The module below is the executable module Portal builds.
#![allow(dead_code)]

#[path = "../src/bond.rs"]
mod bond;

mod bond_spec {
    use super::bond::*;
    use vstd::prelude::*;

    verus! {
    pub(super) open spec fn admitted(
        status: CheckpointBondStatus, before: Balances, bond: u64, rent: u64, same_account: bool,
    ) -> bool {
        &&& !same_account
        &&& status == CheckpointBondStatus::Locked
        &&& bond > 0
        &&& rent as int + bond as int <= before.checkpoint as int
        &&& before.recipient as int + bond as int <= u64::MAX as int
    }

    pub(super) open spec fn correct_result(
        status: CheckpointBondStatus, action: Resolution, before: Balances,
        bond: u64, rent: u64, same_account: bool, result: Result<Payment, BondError>,
    ) -> bool {
        &&& (result is Ok) == admitted(status, before, bond, rent, same_account)
        &&& match result {
            Ok(p) => {
                &&& p.status == match action {
                    Resolution::Release => CheckpointBondStatus::Released,
                    Resolution::Slash => CheckpointBondStatus::Slashed,
                }
                &&& p.balances.checkpoint as int == before.checkpoint as int - bond as int
                &&& p.balances.recipient as int == before.recipient as int + bond as int
                &&& p.balances.checkpoint >= rent
                &&& p.balances.checkpoint as int + p.balances.recipient as int
                    == before.checkpoint as int + before.recipient as int
            },
            Err(e) => e == (
                if same_account { BondError::Aliased }
                else if status != CheckpointBondStatus::Locked { BondError::AlreadyResolved }
                else if bond == 0 { BondError::Insufficient }
                else if rent as int + bond as int > u64::MAX as int { BondError::Overflow }
                else if (before.checkpoint as int) < rent as int + bond as int { BondError::Insufficient }
                else { BondError::Overflow }
            ),
        }
    }

    // Both calls are real code. Even a different action, recipient balance, bond or rent
    // on the second call cannot make this checkpoint's terminal status payable again.
    fn cannot_pay_twice(
        action: Resolution, retry: Resolution, before: Balances, bond: u64, rent: u64,
        retry_recipient: u64, retry_bond: u64, retry_rent: u64,
    ) {
        if let Ok(p) = plan_resolution(CheckpointBondStatus::Locked, action, before, bond, rent, false) {
            let later = Balances { checkpoint: p.balances.checkpoint, recipient: retry_recipient };
            let again = plan_resolution(p.status, retry, later, retry_bond, retry_rent, false);
            assert(again == Err::<Payment, BondError>(BondError::AlreadyResolved));
        }
    }

    // A universal implication that never reaches success would be of no audit value.
    fn both_actions_reachable() {
        let before = Balances { checkpoint: 38, recipient: 7 };
        let r = plan_resolution(CheckpointBondStatus::Locked, Resolution::Release, before, 12, 20, false);
        let s = plan_resolution(CheckpointBondStatus::Locked, Resolution::Slash, before, 12, 20, false);
        assert(r is Ok && s is Ok);
    }
    }
}
