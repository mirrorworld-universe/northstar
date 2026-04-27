use pinocchio::program_error::ProgramError;

#[repr(u32)]
pub enum PortalError {
    InvalidInstruction = 0,
    InvalidAccountData = 1,
    SessionExpired = 2,
    SessionStillActive = 3,
    Unauthorized = 4,
    InsufficientFees = 5,
    ArithmeticOverflow = 6,
    InvalidPdaSeeds = 7,
    SessionAccountOwnerMismatch = 8,
    SessionDeserializeFailed = 9,
    SessionStateInvalid = 10,
    DepositReceiptDeserializeFailed = 11,
    DepositReceiptStateInvalid = 12,
    DelegatedAccountOwnerMismatch = 13,
    DelegationRecordAlreadyInitialized = 14,
    DelegationRecordDeserializeFailed = 15,
    DelegationRecordStateInvalid = 16,
    /// Buffer PDA's pubkey does not match the expected derivation
    /// `["portal_buffer", delegated_account]` under the owner_program.
    DelegateBufferPdaMismatch = 17,
    /// Buffer PDA is not owned by the owner_program supplied to Delegate.
    DelegateBufferOwnerMismatch = 18,
    /// Buffer PDA's data length does not match the delegated account's data length.
    DelegateBufferSizeMismatch = 19,
}

impl From<PortalError> for ProgramError {
    fn from(e: PortalError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
