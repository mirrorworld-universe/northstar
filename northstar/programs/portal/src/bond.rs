//! Pure accounting for the two checkpoint-bond resolution paths.

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(verus_keep_ghost),
    derive(borsh::BorshSerialize, borsh::BorshDeserialize)
)]
#[cfg_attr(not(verus_keep_ghost), borsh(use_discriminant = true))]
#[repr(u8)]
pub enum CheckpointBondStatus {
    Locked = 0,
    Released = 1,
    Slashed = 2,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    Release,
    Slash,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Balances {
    pub(crate) checkpoint: u64,
    pub(crate) recipient: u64,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Payment {
    pub(crate) balances: Balances,
    pub(crate) status: CheckpointBondStatus,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BondError {
    Aliased,
    AlreadyResolved,
    Insufficient,
    Overflow,
}

#[cfg_attr(verus_keep_ghost, verus_spec(result =>
    ensures crate::bond_spec::correct_result(status, action, before, bond, rent, same_account, result)
))]
pub(crate) fn plan_resolution(
    status: CheckpointBondStatus,
    action: Resolution,
    before: Balances,
    bond: u64,
    rent: u64,
    same_account: bool,
) -> Result<Payment, BondError> {
    if same_account {
        return Err(BondError::Aliased);
    }
    if !matches!(status, CheckpointBondStatus::Locked) {
        return Err(BondError::AlreadyResolved);
    }
    if bond == 0 {
        return Err(BondError::Insufficient);
    }
    let minimum = rent.checked_add(bond).ok_or(BondError::Overflow)?;
    if before.checkpoint < minimum {
        return Err(BondError::Insufficient);
    }
    let recipient = before
        .recipient
        .checked_add(bond)
        .ok_or(BondError::Overflow)?;
    Ok(Payment {
        balances: Balances {
            checkpoint: before.checkpoint - bond,
            recipient,
        },
        status: match action {
            Resolution::Release => CheckpointBondStatus::Released,
            Resolution::Slash => CheckpointBondStatus::Slashed,
        },
    })
}
