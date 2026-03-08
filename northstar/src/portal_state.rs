use northstar_portal::{DelegationRecord, FeeVault, Session};

/// Enum representing any portal program account type.
#[derive(Debug, Clone)]
pub enum PortalAccount {
    Session(Session),
    FeeVault(FeeVault),
    DelegationRecord(DelegationRecord),
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
        _ => None,
    }
}
