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
}

impl From<PortalError> for ProgramError {
    fn from(e: PortalError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
