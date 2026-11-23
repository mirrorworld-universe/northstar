//! SOL receipt settlement, after the instruction has bound the accounts and receipt.

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) struct Binding<'a> {
    pub(crate) session: &'a [u8; 32],
    pub(crate) recipient: &'a [u8; 32],
    pub(crate) bump: u8,
}

#[cfg_attr(verus_keep_ghost, verus_spec(result =>
    ensures result == ((a.session@ =~= b.session@) && (a.recipient@ =~= b.recipient@) && a.bump == b.bump)
))]
pub(crate) fn same_binding(a: Binding<'_>, b: Binding<'_>) -> bool {
    a.session.as_slice() == b.session.as_slice()
        && a.recipient.as_slice() == b.recipient.as_slice()
        && a.bump == b.bump
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Counters {
    pub(crate) balance: u64,
    pub(crate) withdrawn: u64,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) struct Lamports {
    pub(crate) receipt: u64,
    pub(crate) recipient: u64,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy)]
pub(crate) enum ReceiptError {
    InvalidUpdate,
    InsufficientBacking,
    Overflow,
    DepositCap,
}

// None means a duplicate, not an update that happens to pay zero.
#[cfg_attr(verus_keep_ghost, verus_spec(result =>
    ensures crate::receipt_spec::classification(old, next, claimed, result)
))]
pub(crate) fn payout_delta(
    old: Counters,
    next: Counters,
    claimed: u64,
) -> Result<Option<u64>, ReceiptError> {
    if old.balance == next.balance && next.withdrawn <= old.withdrawn {
        return Ok(None);
    }
    let delta = next
        .withdrawn
        .checked_sub(old.withdrawn)
        .ok_or(ReceiptError::InvalidUpdate)?;
    if delta != claimed {
        return Err(ReceiptError::InvalidUpdate);
    }
    Ok(Some(delta))
}

#[cfg_attr(verus_keep_ghost, verus_spec(result =>
    ensures result == (balance as int + withdrawn as int <= total as int)
))]
pub(crate) fn within_deposit_cap(balance: u64, withdrawn: u64, total: u64) -> bool {
    match balance.checked_add(withdrawn) {
        Some(sum) => sum <= total,
        None => false,
    }
}

#[cfg_attr(verus_keep_ghost, verus_spec(result =>
    ensures crate::receipt_spec::funding(next, payout, before, rent, cap, same_account, result)
))]
pub(crate) fn plan_payment(
    next: Counters,
    payout: u64,
    before: Lamports,
    rent: u64,
    cap: Option<u64>,
    same_account: bool,
) -> Result<Lamports, ReceiptError> {
    if payout != 0 && same_account {
        return Err(ReceiptError::InvalidUpdate);
    }
    let spendable = before
        .receipt
        .checked_sub(rent)
        .ok_or(ReceiptError::InsufficientBacking)?;
    let remaining = spendable
        .checked_sub(payout)
        .ok_or(ReceiptError::InsufficientBacking)?;
    if next.balance > remaining {
        return Err(ReceiptError::InsufficientBacking);
    }
    let recipient = before
        .recipient
        .checked_add(payout)
        .ok_or(ReceiptError::Overflow)?;
    if let Some(limit) = cap {
        if !within_deposit_cap(next.balance, next.withdrawn, limit) {
            return Err(ReceiptError::DepositCap);
        }
    }
    Ok(Lamports {
        receipt: before.receipt - payout,
        recipient,
    })
}
