use {
    crate::{PortalError, Session},
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_zk_types::continuity::{SessionIdentityV2, StateRootOpeningV2},
    pinocchio::Address as Pubkey,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SessionV2 {
    pub discriminator: [u8; 8],
    pub base: Session,
    pub l1_genesis: [u8; 32],
    pub profile: [u8; 32],
    pub ingress_count: u64,
    pub ingress_root: [u8; 32],
    pub consumed_ingress_count: u64,
    pub consumed_ingress_root: [u8; 32],
}

impl SessionV2 {
    pub const DISCRIMINATOR: [u8; 8] = *b"NSSESS02";

    pub fn identity(&self, portal: &Pubkey, session: &Pubkey) -> SessionIdentityV2 {
        SessionIdentityV2 {
            version: 2,
            l1_genesis: self.l1_genesis,
            portal: portal.to_bytes(),
            session: session.to_bytes(),
            nonce: self.base.nonce,
            grid_id: self.base.grid_id,
            validator: self.base.validator.to_bytes(),
            profile: self.profile,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
            && self.base.is_valid()
            && self.base.nonce > 0
            && self.l1_genesis != [0; 32]
            && self.profile != [0; 32]
            && self.consumed_ingress_count <= self.ingress_count
            && (self.ingress_count == 0) == (self.ingress_root == [0; 32])
            && (self.consumed_ingress_count == 0) == (self.consumed_ingress_root == [0; 32])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SessionEpochV2 {
    pub discriminator: [u8; 8],
    pub last_nonce: u128,
    pub bump: u8,
}

impl SessionEpochV2 {
    pub const DISCRIMINATOR: [u8; 8] = *b"NSEPOCH2";
    pub const SEED_PREFIX: &[u8] = b"session_epoch_v2";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DepositReceiptV2 {
    pub discriminator: [u8; 8],
    pub base: crate::DepositReceipt,
    pub session_context: [u8; 32],
    pub total_deposited: u64,
}

impl DepositReceiptV2 {
    pub const DISCRIMINATOR: [u8; 8] = *b"NSRECP02";
    pub const SEED_PREFIX: &[u8] = b"deposit_receipt_v2";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CheckpointBindingV2 {
    pub discriminator: [u8; 8],
    pub checkpoint: [u8; 32],
    pub pre: StateRootOpeningV2,
    pub post: StateRootOpeningV2,
    pub prefix_record: [u8; 32],
    pub input_context_root: [u8; 32],
    pub bump: u8,
}

impl CheckpointBindingV2 {
    pub const DISCRIMINATOR: [u8; 8] = *b"NSCHKP02";
    pub const SEED_PREFIX: &[u8] = b"checkpoint_v2";
}

pub fn parse_session(data: &[u8]) -> Result<Session, PortalError> {
    if data.starts_with(&SessionV2::DISCRIMINATOR) {
        let state =
            SessionV2::try_from_slice(data).map_err(|_| PortalError::SessionDeserializeFailed)?;
        if !state.is_valid() {
            return Err(PortalError::SessionStateInvalid);
        }
        Ok(state.base)
    } else {
        let state =
            Session::try_from_slice(data).map_err(|_| PortalError::SessionDeserializeFailed)?;
        if !state.is_valid() {
            return Err(PortalError::SessionStateInvalid);
        }
        Ok(state)
    }
}

pub fn parse_deposit_receipt(data: &[u8]) -> Result<crate::DepositReceipt, PortalError> {
    if data.starts_with(&DepositReceiptV2::DISCRIMINATOR) {
        let frame = DepositReceiptV2::try_from_slice(data)
            .map_err(|_| PortalError::DepositReceiptDeserializeFailed)?;
        if !frame.base.is_valid()
            || frame.session_context == [0; 32]
            || frame
                .base
                .balance
                .checked_add(frame.base.withdrawn)
                .is_none_or(|amount| amount > frame.total_deposited)
        {
            return Err(PortalError::DepositReceiptStateInvalid);
        }
        Ok(frame.base)
    } else {
        let state = crate::DepositReceipt::try_from_slice(data)
            .map_err(|_| PortalError::DepositReceiptDeserializeFailed)?;
        if !state.is_valid() {
            return Err(PortalError::DepositReceiptStateInvalid);
        }
        Ok(state)
    }
}

pub fn write_deposit_receipt(
    data: &mut [u8],
    state: &crate::DepositReceipt,
) -> Result<(), PortalError> {
    if data.starts_with(&DepositReceiptV2::DISCRIMINATOR) {
        let mut frame = DepositReceiptV2::try_from_slice(data)
            .map_err(|_| PortalError::DepositReceiptDeserializeFailed)?;
        if state.session != frame.base.session
            || state.recipient != frame.base.recipient
            || state.bump != frame.base.bump
            || state.withdrawn < frame.base.withdrawn
            || state
                .balance
                .checked_add(state.withdrawn)
                .is_none_or(|amount| amount > frame.total_deposited)
        {
            return Err(PortalError::DepositReceiptStateInvalid);
        }
        frame.base = *state;
        frame
            .serialize(&mut &mut data[..])
            .map_err(|_| PortalError::DepositReceiptStateInvalid)
    } else {
        state
            .serialize(&mut &mut data[..])
            .map_err(|_| PortalError::DepositReceiptStateInvalid)
    }
}

pub fn write_session(data: &mut [u8], state: &Session) -> Result<(), PortalError> {
    if data.starts_with(&SessionV2::DISCRIMINATOR) {
        let mut frame =
            SessionV2::try_from_slice(data).map_err(|_| PortalError::SessionDeserializeFailed)?;
        if !frame.is_valid() {
            return Err(PortalError::SessionStateInvalid);
        }
        let old = frame.base;
        if (
            old.nonce,
            old.grid_id,
            old.authority,
            old.validator,
            old.created_at,
            old.ttl_slots,
            old.fee_cap,
            old.settlement_interval_slots,
            old.bump,
        ) != (
            state.nonce,
            state.grid_id,
            state.authority,
            state.validator,
            state.created_at,
            state.ttl_slots,
            state.fee_cap,
            state.settlement_interval_slots,
            state.bump,
        ) {
            return Err(PortalError::SessionStateInvalid);
        }
        frame.base = *state;
        frame
            .serialize(&mut &mut data[..])
            .map_err(|_| PortalError::SessionStateInvalid)
    } else {
        state
            .serialize(&mut &mut data[..])
            .map_err(|_| PortalError::SessionStateInvalid)
    }
}
