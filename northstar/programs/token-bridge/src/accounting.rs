//! Decisions over decoded ER state. No account writes, system calls, or token CPIs.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

type Key = [u8; 32];

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) struct Holding<'a> {
    pub(crate) session_bridge: &'a Key,
    pub(crate) mint: &'a Key,
    pub(crate) amount: u64,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum BridgeError {
    MissingSignature,
    Binding,
    Insufficient,
    Overflow,
    Withdrawal,
}

#[cfg_attr(verus_keep_ghost, verus_spec(result =>
    ensures crate::bridge_spec::access(signed, source, destination, result)
))]
pub(crate) fn transfer_access(
    signed: bool,
    source: &Key,
    destination: &Key,
) -> Result<(), BridgeError> {
    if !signed {
        return Err(BridgeError::MissingSignature);
    }
    if source.as_slice() == destination.as_slice() {
        return Err(BridgeError::Binding);
    }
    Ok(())
}

#[cfg_attr(verus_keep_ghost, verus_spec(result =>
    ensures crate::bridge_spec::transfer(authority, owner, source, destination, amount, result)
))]
pub(crate) fn transfer_balances(
    authority: &Key,
    owner: &Key,
    source: Holding<'_>,
    destination: Holding<'_>,
    amount: u64,
) -> Result<(u64, u64), BridgeError> {
    if authority.as_slice() != owner.as_slice()
        || source.session_bridge.as_slice() != destination.session_bridge.as_slice()
        || source.mint.as_slice() != destination.mint.as_slice()
    {
        return Err(BridgeError::Binding);
    }
    let debit = source
        .amount
        .checked_sub(amount)
        .ok_or(BridgeError::Insufficient)?;
    let credit = destination
        .amount
        .checked_add(amount)
        .ok_or(BridgeError::Overflow)?;
    Ok((debit, credit))
}

// None is a successful replay with no authorization consumption or transfer to perform.
#[cfg_attr(verus_keep_ghost, verus_spec(result =>
    ensures crate::bridge_spec::withdrawal(stored, deposited, proposed, amount, result)
))]
pub(crate) fn withdrawal_target(
    stored: u64,
    deposited: u64,
    proposed: u64,
    amount: u64,
) -> Result<Option<u64>, BridgeError> {
    if proposed <= stored {
        return Ok(None);
    }
    let next = stored.checked_add(amount).ok_or(BridgeError::Overflow)?;
    if proposed != next || next > deposited {
        return Err(BridgeError::Withdrawal);
    }
    Ok(Some(next))
}
