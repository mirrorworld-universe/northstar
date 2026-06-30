use northstar_portal::{
    Checkpoint, CheckpointCursor, DelegationRecord, DepositReceipt, FeeVault, Session,
};

/// Enum representing any portal program account type.
#[derive(Debug, Clone)]
pub enum PortalAccount {
    Session(Session),
    FeeVault(FeeVault),
    DelegationRecord(DelegationRecord),
    DepositReceipt(DepositReceipt),
    Checkpoint(Checkpoint),
    CheckpointCursor(CheckpointCursor),
}

pub fn try_parse_raw_portal_account(data: &[u8]) -> Option<PortalAccount> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        Session::DISCRIMINATOR => borsh::from_slice::<Session>(data)
            .ok()
            .map(PortalAccount::Session),
        FeeVault::DISCRIMINATOR => borsh::from_slice::<FeeVault>(data)
            .ok()
            .map(PortalAccount::FeeVault),
        DelegationRecord::DISCRIMINATOR => borsh::from_slice::<DelegationRecord>(data)
            .ok()
            .map(PortalAccount::DelegationRecord),
        DepositReceipt::DISCRIMINATOR => borsh::from_slice::<DepositReceipt>(data)
            .ok()
            .map(PortalAccount::DepositReceipt),
        Checkpoint::DISCRIMINATOR => borsh::from_slice::<Checkpoint>(data)
            .ok()
            .map(PortalAccount::Checkpoint),
        CheckpointCursor::DISCRIMINATOR => borsh::from_slice::<CheckpointCursor>(data)
            .ok()
            .map(PortalAccount::CheckpointCursor),
        _ => None,
    }
}
