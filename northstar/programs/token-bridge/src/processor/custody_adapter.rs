use {
    super::{AccountInfo, ProgramError, ProgramResult, TokenVault},
    crate::custody::{self, Amounts, CustodyError, Direction, Expected, Ledger},
    pinocchio_token::state::Account as TokenAccount,
};

impl From<CustodyError> for ProgramError {
    fn from(e: CustodyError) -> Self {
        match e {
            CustodyError::Overflow => Self::ArithmeticOverflow,
            CustodyError::Insufficient => Self::InsufficientFunds,
            CustodyError::Movement => Self::InvalidAccountData,
        }
    }
}

// Drop both SPL data borrows before either CPI. Read again from the same views after it.
fn amounts(vault: &AccountInfo, peer: &AccountInfo) -> Result<Amounts, ProgramError> {
    Ok(Amounts {
        vault: TokenAccount::from_account_view(vault)?.amount(),
        peer: TokenAccount::from_account_view(peer)?.amount(),
    })
}
pub(super) fn prepare(
    direction: Direction,
    state: &TokenVault,
    vault: &AccountInfo,
    peer: &AccountInfo,
    n: u64,
) -> Result<Expected, ProgramError> {
    Ok(custody::prepare(
        direction,
        Ledger {
            deposited: state.deposited,
            withdrawn: state.withdrawn,
        },
        amounts(vault, peer)?,
        n,
    )?)
}
pub(super) fn finish(
    e: Expected,
    state: &mut TokenVault,
    vault: &AccountInfo,
    peer: &AccountInfo,
) -> ProgramResult {
    let posted = e.confirm(amounts(vault, peer)?)?;
    state.deposited = posted.deposited;
    state.withdrawn = posted.withdrawn;
    Ok(())
}
