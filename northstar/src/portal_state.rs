use northstar_portal::{
    Challenge, Checkpoint, CheckpointCursor, DataAvailabilityProof, DelegationRecord,
    DepositReceipt, FeeVault, Session, SessionBridge, StepProofAccount,
    TokenWithdrawalAuthorization, UndelegationRequest,
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
    Challenge(Challenge),
    DataAvailabilityProof(DataAvailabilityProof),
    StepProofAccount(StepProofAccount),
    SessionBridge(SessionBridge),
    TokenWithdrawalAuthorization(TokenWithdrawalAuthorization),
    UndelegationRequest(UndelegationRequest),
}

pub fn try_parse_raw_portal_account(data: &[u8]) -> Option<PortalAccount> {
    if data.starts_with(&Session::DISCRIMINATOR) {
        borsh::from_slice::<Session>(data)
            .ok()
            .map(PortalAccount::Session)
    } else if data.starts_with(&FeeVault::DISCRIMINATOR) {
        borsh::from_slice::<FeeVault>(data)
            .ok()
            .map(PortalAccount::FeeVault)
    } else if data.starts_with(&DelegationRecord::DISCRIMINATOR) {
        borsh::from_slice::<DelegationRecord>(data)
            .ok()
            .map(PortalAccount::DelegationRecord)
    } else if data.starts_with(&DepositReceipt::DISCRIMINATOR) {
        borsh::from_slice::<DepositReceipt>(data)
            .ok()
            .map(PortalAccount::DepositReceipt)
    } else if data.starts_with(&Checkpoint::DISCRIMINATOR) {
        borsh::from_slice::<Checkpoint>(data)
            .ok()
            .map(PortalAccount::Checkpoint)
    } else if data.starts_with(&CheckpointCursor::DISCRIMINATOR) {
        borsh::from_slice::<CheckpointCursor>(data)
            .ok()
            .map(PortalAccount::CheckpointCursor)
    } else if data.starts_with(&Challenge::DISCRIMINATOR) {
        borsh::from_slice::<Challenge>(data)
            .ok()
            .map(PortalAccount::Challenge)
    } else if data.starts_with(&DataAvailabilityProof::DISCRIMINATOR) {
        borsh::from_slice::<DataAvailabilityProof>(data)
            .ok()
            .map(PortalAccount::DataAvailabilityProof)
    } else if data.starts_with(&StepProofAccount::DISCRIMINATOR) {
        borsh::from_slice::<StepProofAccount>(data)
            .ok()
            .map(PortalAccount::StepProofAccount)
    } else if data.starts_with(&SessionBridge::DISCRIMINATOR) {
        borsh::from_slice::<SessionBridge>(data)
            .ok()
            .map(PortalAccount::SessionBridge)
    } else if data.starts_with(&TokenWithdrawalAuthorization::DISCRIMINATOR) {
        borsh::from_slice::<TokenWithdrawalAuthorization>(data)
            .ok()
            .map(PortalAccount::TokenWithdrawalAuthorization)
    } else if data.starts_with(&UndelegationRequest::DISCRIMINATOR) {
        borsh::from_slice::<UndelegationRequest>(data)
            .ok()
            .map(PortalAccount::UndelegationRequest)
    } else {
        None
    }
}
