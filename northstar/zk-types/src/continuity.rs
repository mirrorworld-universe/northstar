use {
    crate::{FrBytes, ZkTypeError},
    borsh::{BorshDeserialize, BorshSerialize},
};

pub const CHECKPOINT_FORMAT_VERSION_V2: u8 = 2;
pub const ER_STEP_PROOF_KIND_CONTINUITY: u8 = 4;
pub const INGRESS_RECORD_DISCRIMINATOR_V2: [u8; 8] = *b"NSINGR02";
pub const INGRESS_RECORD_SEED_V2: &[u8] = b"ingress_v2";
pub const MAX_INGRESS_ENCODING_BYTES_V2: usize = 512;
pub type Hash32 = [u8; 32];
pub type CommitmentHasher = fn(&[&[u8]]) -> Hash32;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuityError {
    Encoding,
    UnsupportedVersion,
    InvalidIdentity,
    InvalidSequence,
    InvalidRoot,
    InvalidPayload,
}

impl core::fmt::Display for ContinuityError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, formatter)
    }
}

impl core::error::Error for ContinuityError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SessionIdentityV2 {
    pub version: u8,
    pub l1_genesis: Hash32,
    pub portal: Hash32,
    pub session: Hash32,
    pub nonce: u128,
    pub grid_id: u64,
    pub validator: Hash32,
    pub profile: Hash32,
}

impl SessionIdentityV2 {
    pub fn commitment(&self, hasher: CommitmentHasher) -> Result<Hash32, ContinuityError> {
        if self.version != CHECKPOINT_FORMAT_VERSION_V2 {
            return Err(ContinuityError::UnsupportedVersion);
        }
        if [
            self.l1_genesis,
            self.portal,
            self.session,
            self.validator,
            self.profile,
        ]
        .contains(&[0; 32])
            || self.nonce == 0
        {
            return Err(ContinuityError::InvalidIdentity);
        }
        commitment(b"session", self, hasher)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum TransitionFamilyV2 {
    Execution = 0,
    Ingress = 1,
}

impl TransitionFamilyV2 {
    #[allow(clippy::arithmetic_side_effects)]
    pub const fn proof_domain(self) -> FrBytes {
        let mut bytes = [0; 32];
        let tag = *b"northstar-er-step-v2";
        let mut index = 0;
        while index < tag.len() {
            bytes[8 + index] = tag[index];
            index += 1;
        }
        bytes[28] = CHECKPOINT_FORMAT_VERSION_V2;
        bytes[29] = self as u8;
        bytes[30] = ER_STEP_PROOF_KIND_CONTINUITY;
        bytes[31] = 1;
        // All nonzero bytes follow the first eight bytes, below the BN254 modulus.
        FrBytes(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum IngressPayloadV2 {
    Delegation {
        account: Hash32,
        record: Hash32,
        owner: Hash32,
        lamports: u64,
        data_len: u32,
        data_hash: Hash32,
        executable: bool,
        rent_epoch: u64,
    },
    SolDeposit {
        recipient: Hash32,
        receipt: Hash32,
        amount: u64,
        cumulative_deposited: u64,
    },
    TokenDeposit {
        bridge: Hash32,
        session_bridge: Hash32,
        account: Hash32,
        receipt: Hash32,
        mint: Hash32,
        token_program: Hash32,
        origin_deposited: u64,
        amount: u64,
        cumulative_deposited: u64,
    },
    Freeze {
        account: Hash32,
        request: Hash32,
        requested_l1_slot: u64,
    },
    Release {
        account: Hash32,
        request: Hash32,
    },
}

impl IngressPayloadV2 {
    pub const fn account(&self) -> Hash32 {
        match self {
            Self::Delegation { account, .. }
            | Self::TokenDeposit { account, .. }
            | Self::Freeze { account, .. }
            | Self::Release { account, .. } => *account,
            Self::SolDeposit { recipient, .. } => *recipient,
        }
    }

    pub fn validate(&self) -> Result<(), ContinuityError> {
        let valid = match self {
            Self::Delegation {
                record,
                data_hash,
                executable,
                ..
            } => *record != [0; 32] && *data_hash != [0; 32] && !executable,
            Self::SolDeposit {
                receipt,
                amount,
                cumulative_deposited,
                ..
            } => *receipt != [0; 32] && *amount > 0 && *cumulative_deposited >= *amount,
            Self::TokenDeposit {
                bridge,
                session_bridge,
                receipt,
                mint,
                token_program,
                origin_deposited,
                amount,
                cumulative_deposited,
                ..
            } => {
                ![*bridge, *session_bridge, *receipt, *mint, *token_program].contains(&[0; 32])
                    && *amount > 0
                    && cumulative_deposited
                        .checked_sub(*origin_deposited)
                        .is_some_and(|available| available >= *amount)
            }
            Self::Freeze { request, .. } | Self::Release { request, .. } => *request != [0; 32],
        };
        if valid && self.account() != [0; 32] {
            Ok(())
        } else {
            Err(ContinuityError::InvalidPayload)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct IngressRecordV2 {
    pub discriminator: [u8; 8],
    pub version: u8,
    pub session_context: Hash32,
    pub sequence: u64,
    pub l1_slot: u64,
    pub previous_root: Hash32,
    pub payload: IngressPayloadV2,
    pub root: Hash32,
    pub bump: u8,
}

impl IngressRecordV2 {
    pub fn new(
        session_context: Hash32,
        sequence: u64,
        l1_slot: u64,
        previous_root: Hash32,
        payload: IngressPayloadV2,
        bump: u8,
        hasher: CommitmentHasher,
    ) -> Result<Self, ContinuityError> {
        let mut record = Self {
            discriminator: INGRESS_RECORD_DISCRIMINATOR_V2,
            version: CHECKPOINT_FORMAT_VERSION_V2,
            session_context,
            sequence,
            l1_slot,
            previous_root,
            payload,
            root: [0; 32],
            bump,
        };
        payload.validate()?;
        if session_context == [0; 32] || sequence == 0 {
            return Err(ContinuityError::InvalidSequence);
        }
        record.root = record.computed_root(hasher)?;
        Ok(record)
    }

    pub fn computed_root(&self, hasher: CommitmentHasher) -> Result<Hash32, ContinuityError> {
        commitment(
            b"ingress",
            &(
                self.version,
                self.session_context,
                self.sequence,
                self.l1_slot,
                self.previous_root,
                self.payload,
            ),
            hasher,
        )
    }

    /// Checks the committed sequence, not L1 account authenticity. Callers must
    /// separately bind the immutable record's Portal owner, PDA and finalized prefix.
    pub fn validate_next(
        &self,
        session_context: Hash32,
        consumed: u64,
        previous_root: Hash32,
        hasher: CommitmentHasher,
    ) -> Result<(), ContinuityError> {
        if self.discriminator != INGRESS_RECORD_DISCRIMINATOR_V2
            || self.version != CHECKPOINT_FORMAT_VERSION_V2
        {
            return Err(ContinuityError::UnsupportedVersion);
        }
        if self.session_context != session_context {
            return Err(ContinuityError::InvalidIdentity);
        }
        if Some(self.sequence) != consumed.checked_add(1) {
            return Err(ContinuityError::InvalidSequence);
        }
        self.payload.validate()?;
        if self.previous_root != previous_root || self.root != self.computed_root(hasher)? {
            return Err(ContinuityError::InvalidRoot);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct StateRootOpeningV2 {
    pub session_context: Hash32,
    pub account_root: Hash32,
    pub origin_root: Hash32,
    pub ingress_count: u64,
    pub ingress_root: Hash32,
}

impl StateRootOpeningV2 {
    pub fn commitment(&self, hasher: CommitmentHasher) -> Result<Hash32, ContinuityError> {
        if self.session_context == [0; 32] {
            return Err(ContinuityError::InvalidIdentity);
        }
        if self.account_root == [0; 32] || self.origin_root == [0; 32] {
            return Err(ContinuityError::InvalidRoot);
        }
        if (self.ingress_count == 0) != (self.ingress_root == [0; 32]) {
            return Err(ContinuityError::InvalidRoot);
        }
        commitment(b"state", self, hasher)
    }
}

pub fn empty_state_opening_v2(
    session_context: Hash32,
    hasher: CommitmentHasher,
) -> Result<StateRootOpeningV2, ContinuityError> {
    let empty_leaf = hasher(&[
        b"northstar-checkpoint-v1",
        b"state",
        b"empty-leaf",
        &4u64.to_le_bytes(),
        &0u32.to_le_bytes(),
    ]);
    let mut account_root = hasher(&[
        b"northstar-checkpoint-v1",
        b"state",
        b"root",
        &0u32.to_le_bytes(),
        &empty_leaf,
    ]);
    account_root[0] &= 0x1f;
    let origin_root = commitment(b"origins", &hasher(&[&[0; 20]]), hasher)?;
    let opening = StateRootOpeningV2 {
        session_context,
        account_root,
        origin_root,
        ingress_count: 0,
        ingress_root: [0; 32],
    };
    opening.commitment(hasher)?;
    Ok(opening)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct InputContextV2 {
    pub session_context: Hash32,
    pub readonly_l1_root: Hash32,
    pub ingress_start: u64,
    pub ingress_start_root: Hash32,
    pub ingress_end: u64,
    pub ingress_end_root: Hash32,
}

impl InputContextV2 {
    pub fn commitment(&self, hasher: CommitmentHasher) -> Result<Hash32, ContinuityError> {
        if self.session_context == [0; 32]
            || self.readonly_l1_root == [0; 32]
            || self.ingress_start > self.ingress_end
            || (self.ingress_start == 0) != (self.ingress_start_root == [0; 32])
            || (self.ingress_end == 0) != (self.ingress_end_root == [0; 32])
        {
            return Err(ContinuityError::InvalidRoot);
        }
        commitment(b"input-context", self, hasher)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct StepPublicInputsV2 {
    pub domain: FrBytes,
    pub session_context: FrBytes,
    pub slot_step: FrBytes,
    pub pre_state_root: FrBytes,
    pub post_state_root: FrBytes,
    pub transition_effect_root: FrBytes,
    pub input_context_root: FrBytes,
    pub settlement_effect_root: FrBytes,
}

impl StepPublicInputsV2 {
    pub fn validate(&self, family: TransitionFamilyV2) -> Result<(), ZkTypeError> {
        if self.domain != family.proof_domain() {
            return Err(ZkTypeError::InvalidProofDomain);
        }
        if [
            self.session_context,
            self.pre_state_root,
            self.post_state_root,
            self.transition_effect_root,
            self.input_context_root,
            self.settlement_effect_root,
        ]
        .contains(&FrBytes::ZERO)
        {
            return Err(ZkTypeError::MissingPublicCommitment);
        }
        Ok(())
    }
}

#[inline(never)]
pub fn commitment<T: BorshSerialize>(
    label: &[u8],
    value: &T,
    hasher: CommitmentHasher,
) -> Result<Hash32, ContinuityError> {
    let mut buffer = [0; MAX_INGRESS_ENCODING_BYTES_V2];
    let mut remaining = &mut buffer[..];
    value
        .serialize(&mut remaining)
        .map_err(|_| ContinuityError::Encoding)?;
    let length = MAX_INGRESS_ENCODING_BYTES_V2
        .checked_sub(remaining.len())
        .ok_or(ContinuityError::Encoding)?;
    let label_length = u16::try_from(label.len()).map_err(|_| ContinuityError::Encoding)?;
    let encoded_length = u16::try_from(length).map_err(|_| ContinuityError::Encoding)?;
    let mut root = hasher(&[
        b"northstar-continuity-v2",
        &label_length.to_le_bytes(),
        label,
        &encoded_length.to_le_bytes(),
        &buffer[..length],
    ]);
    root[0] &= 0x1f;
    Ok(root)
}
