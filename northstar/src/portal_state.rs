use northstar_portal::{
    DelegationRecord, FeeVault, Session, DELEGATION_RECORD_DISCRIMINATOR, FEE_VAULT_DISCRIMINATOR,
    SESSION_DISCRIMINATOR,
};

/// Enum representing any portal program account type.
#[derive(Debug, Clone)]
pub enum PortalAccount {
    Session(Session),
    FeeVault(FeeVault),
    DelegationRecord(DelegationRecord),
}

pub fn try_parse_portal_account(data: &[u8]) -> Option<PortalAccount> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        SESSION_DISCRIMINATOR => borsh::from_slice::<Session>(data)
            .ok()
            .map(PortalAccount::Session),
        FEE_VAULT_DISCRIMINATOR => borsh::from_slice::<FeeVault>(data)
            .ok()
            .map(PortalAccount::FeeVault),
        DELEGATION_RECORD_DISCRIMINATOR => borsh::from_slice::<DelegationRecord>(data)
            .ok()
            .map(PortalAccount::DelegationRecord),
        _ => None,
    }
}
