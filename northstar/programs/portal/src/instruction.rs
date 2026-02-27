#[repr(u8)]
pub enum PortalInstruction {
    OpenSession = 0,
    CloseSession = 1,
    DepositFee = 2,
    Delegate = 3,
    Undelegate = 4,
}

impl TryFrom<u8> for PortalInstruction {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(PortalInstruction::OpenSession),
            1 => Ok(PortalInstruction::CloseSession),
            2 => Ok(PortalInstruction::DepositFee),
            3 => Ok(PortalInstruction::Delegate),
            4 => Ok(PortalInstruction::Undelegate),
            _ => Err(()),
        }
    }
}
