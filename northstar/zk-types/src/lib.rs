#![no_std]

extern crate alloc;

pub mod trace;

use borsh::{
    io::{Error, ErrorKind, Read, Result as IoResult},
    BorshDeserialize, BorshSerialize,
};

pub const ER_STEP_PUBLIC_INPUTS_V1: usize = 8;
pub const GROTH16_PROOF_RAW_LEN: usize = 256;
pub const ER_STEP_PROOF_KIND_ONE_ACCOUNT: u8 = 1;
pub const ER_STEP_PROOF_KIND_FULL_TRANSACTION: u8 = 2;
pub const ER_STEP_PROOF_VERSION_V1: u8 = 1;

/// BN254 scalar-field modulus, big-endian.
pub const BN254_FR_MODULUS_BE: [u8; 32] = [
    0x30, 0x64, 0x4e, 0x72, 0xe1, 0x31, 0xa0, 0x29, 0xb8, 0x50, 0x45, 0xb6, 0x81, 0x81, 0x58, 0x5d,
    0x28, 0x33, 0xe8, 0x48, 0x79, 0xb9, 0x70, 0x91, 0x43, 0xe1, 0xf5, 0x93, 0xf0, 0x00, 0x00, 0x01,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZkTypeError {
    NonCanonicalFieldElement,
    InvalidProofLength,
    InvalidProofDomain,
    MissingPublicCommitment,
}

/// Canonical BN254 scalar-field element encoded as 32-byte big-endian bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize)]
pub struct FrBytes([u8; 32]);

impl BorshDeserialize for FrBytes {
    fn deserialize_reader<R: Read>(reader: &mut R) -> IoResult<Self> {
        let bytes = <[u8; 32]>::deserialize_reader(reader)?;
        Self::new(bytes).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                "noncanonical BN254 scalar field element",
            )
        })
    }
}

impl FrBytes {
    pub const ZERO: Self = Self([0; 32]);

    pub fn new(bytes: [u8; 32]) -> Result<Self, ZkTypeError> {
        if bytes >= BN254_FR_MODULUS_BE {
            return Err(ZkTypeError::NonCanonicalFieldElement);
        }
        Ok(Self(bytes))
    }

    #[allow(clippy::arithmetic_side_effects)]
    pub const fn from_u64(value: u64) -> Self {
        let mut bytes = [0; 32];
        let value = value.to_be_bytes();
        let mut index = 0;
        while index < value.len() {
            bytes[24 + index] = value[index];
            index += 1;
        }
        Self(bytes)
    }

    /// Packs `(high, low)` into one 128-bit field value.
    #[allow(clippy::arithmetic_side_effects)]
    pub const fn from_u64_pair(high: u64, low: u64) -> Self {
        let mut bytes = [0; 32];
        let high = high.to_be_bytes();
        let low = low.to_be_bytes();
        let mut index = 0;
        while index < 8 {
            bytes[16 + index] = high[index];
            bytes[24 + index] = low[index];
            index += 1;
        }
        Self(bytes)
    }

    /// Packs protocol identity and proof selector into one stable field value.
    #[allow(clippy::arithmetic_side_effects)]
    pub const fn er_step_domain_v1(proof_kind: u8, proof_version: u8) -> Self {
        let mut bytes = [0; 32];
        let domain = *b"northstar-er-step-v1";
        let mut index = 0;
        while index < domain.len() {
            bytes[8 + index] = domain[index];
            index += 1;
        }
        bytes[30] = proof_kind;
        bytes[31] = proof_version;
        Self(bytes)
    }

    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl TryFrom<[u8; 32]> for FrBytes {
    type Error = ZkTypeError;

    fn try_from(value: [u8; 32]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<FrBytes> for [u8; 32] {
    fn from(value: FrBytes) -> Self {
        value.to_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct Groth16ProofRaw {
    pub a: [u8; 64],
    pub b: [u8; 128],
    pub c: [u8; 64],
}

impl Groth16ProofRaw {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ZkTypeError> {
        if bytes.len() != GROTH16_PROOF_RAW_LEN {
            return Err(ZkTypeError::InvalidProofLength);
        }
        let mut a = [0; 64];
        let mut b = [0; 128];
        let mut c = [0; 64];
        a.copy_from_slice(&bytes[..64]);
        b.copy_from_slice(&bytes[64..192]);
        c.copy_from_slice(&bytes[192..]);
        Ok(Self { a, b, c })
    }

    pub fn to_bytes(self) -> [u8; GROTH16_PROOF_RAW_LEN] {
        let mut bytes = [0; GROTH16_PROOF_RAW_LEN];
        bytes[..64].copy_from_slice(&self.a);
        bytes[64..192].copy_from_slice(&self.b);
        bytes[192..].copy_from_slice(&self.c);
        bytes
    }
}

/// Canonical public-input order for `northstar-er-step-v1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
pub struct ErStepPublicInputsV1 {
    pub domain: FrBytes,
    pub session_context: FrBytes,
    pub slot_step: FrBytes,
    pub pre_state_root: FrBytes,
    pub post_state_root: FrBytes,
    pub tx_effect_root: FrBytes,
    pub readonly_l1_root: FrBytes,
    pub settlement_effect_root: FrBytes,
}

impl ErStepPublicInputsV1 {
    pub const fn to_array(self) -> [[u8; 32]; ER_STEP_PUBLIC_INPUTS_V1] {
        [
            self.domain.to_bytes(),
            self.session_context.to_bytes(),
            self.slot_step.to_bytes(),
            self.pre_state_root.to_bytes(),
            self.post_state_root.to_bytes(),
            self.tx_effect_root.to_bytes(),
            self.readonly_l1_root.to_bytes(),
            self.settlement_effect_root.to_bytes(),
        ]
    }

    pub fn from_array(inputs: [[u8; 32]; ER_STEP_PUBLIC_INPUTS_V1]) -> Result<Self, ZkTypeError> {
        Ok(Self {
            domain: FrBytes::new(inputs[0])?,
            session_context: FrBytes::new(inputs[1])?,
            slot_step: FrBytes::new(inputs[2])?,
            pre_state_root: FrBytes::new(inputs[3])?,
            post_state_root: FrBytes::new(inputs[4])?,
            tx_effect_root: FrBytes::new(inputs[5])?,
            readonly_l1_root: FrBytes::new(inputs[6])?,
            settlement_effect_root: FrBytes::new(inputs[7])?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullTransactionPublicInputsV1(ErStepPublicInputsV1);

impl FullTransactionPublicInputsV1 {
    pub const fn into_inner(self) -> ErStepPublicInputsV1 {
        self.0
    }
}

impl TryFrom<ErStepPublicInputsV1> for FullTransactionPublicInputsV1 {
    type Error = ZkTypeError;

    fn try_from(inputs: ErStepPublicInputsV1) -> Result<Self, Self::Error> {
        if inputs.domain
            != FrBytes::er_step_domain_v1(
                ER_STEP_PROOF_KIND_FULL_TRANSACTION,
                ER_STEP_PROOF_VERSION_V1,
            )
        {
            return Err(ZkTypeError::InvalidProofDomain);
        }
        for commitment in [
            inputs.session_context,
            inputs.pre_state_root,
            inputs.post_state_root,
            inputs.tx_effect_root,
            inputs.readonly_l1_root,
            inputs.settlement_effect_root,
        ] {
            if commitment == FrBytes::ZERO {
                return Err(ZkTypeError::MissingPublicCommitment);
            }
        }
        Ok(Self(inputs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_scalar_modulus_and_accepts_previous_value() {
        assert_eq!(
            FrBytes::new(BN254_FR_MODULUS_BE),
            Err(ZkTypeError::NonCanonicalFieldElement)
        );
        let mut previous = BN254_FR_MODULUS_BE;
        previous[31] = 0;
        assert!(FrBytes::new(previous).is_ok());
    }

    #[test]
    fn borsh_rejects_noncanonical_field_element() {
        assert!(borsh::from_slice::<FrBytes>(&BN254_FR_MODULUS_BE).is_err());
    }

    #[test]
    fn slot_step_pack_is_stable() {
        let packed = FrBytes::from_u64_pair(0x0102_0304_0506_0708, 0x1112_1314_1516_1718);
        assert_eq!(
            packed.to_bytes(),
            [
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 17, 18, 19,
                20, 21, 22, 23, 24,
            ]
        );
    }

    #[test]
    fn domain_pack_is_stable() {
        assert_eq!(
            FrBytes::er_step_domain_v1(3, 1).to_bytes(),
            [
                0, 0, 0, 0, 0, 0, 0, 0, b'n', b'o', b'r', b't', b'h', b's', b't', b'a', b'r', b'-',
                b'e', b'r', b'-', b's', b't', b'e', b'p', b'-', b'v', b'1', 0, 0, 3, 1,
            ]
        );
    }

    #[test]
    fn proof_layout_is_exact() {
        let proof = Groth16ProofRaw {
            a: [1; 64],
            b: [2; 128],
            c: [3; 64],
        };
        let bytes = proof.to_bytes();
        assert_eq!(bytes.len(), GROTH16_PROOF_RAW_LEN);
        assert_eq!(Groth16ProofRaw::from_bytes(&bytes), Ok(proof));
        assert_eq!(borsh::to_vec(&proof).unwrap(), bytes);
    }

    #[test]
    fn public_input_order_and_size_are_exact() {
        let public = ErStepPublicInputsV1 {
            domain: FrBytes::from_u64(1),
            session_context: FrBytes::from_u64(2),
            slot_step: FrBytes::from_u64(3),
            pre_state_root: FrBytes::from_u64(4),
            post_state_root: FrBytes::from_u64(5),
            tx_effect_root: FrBytes::from_u64(6),
            readonly_l1_root: FrBytes::from_u64(7),
            settlement_effect_root: FrBytes::from_u64(8),
        };
        let array = public.to_array();
        for (index, input) in array.iter().enumerate() {
            assert_eq!(input[31], index as u8 + 1);
        }
        assert_eq!(borsh::to_vec(&public).unwrap().len(), 8 * 32);
        assert_eq!(ErStepPublicInputsV1::from_array(array), Ok(public));
    }

    #[test]
    fn full_transaction_inputs_fail_closed() {
        let mut public = ErStepPublicInputsV1 {
            domain: FrBytes::er_step_domain_v1(
                ER_STEP_PROOF_KIND_FULL_TRANSACTION,
                ER_STEP_PROOF_VERSION_V1,
            ),
            session_context: FrBytes::from_u64(1),
            slot_step: FrBytes::ZERO,
            pre_state_root: FrBytes::from_u64(2),
            post_state_root: FrBytes::from_u64(3),
            tx_effect_root: FrBytes::from_u64(4),
            readonly_l1_root: FrBytes::from_u64(5),
            settlement_effect_root: FrBytes::from_u64(6),
        };
        assert!(FullTransactionPublicInputsV1::try_from(public).is_ok());
        public.domain =
            FrBytes::er_step_domain_v1(ER_STEP_PROOF_KIND_ONE_ACCOUNT, ER_STEP_PROOF_VERSION_V1);
        assert_eq!(
            FullTransactionPublicInputsV1::try_from(public),
            Err(ZkTypeError::InvalidProofDomain)
        );
        public.domain = FrBytes::er_step_domain_v1(
            ER_STEP_PROOF_KIND_FULL_TRANSACTION,
            ER_STEP_PROOF_VERSION_V1,
        );
        public.tx_effect_root = FrBytes::ZERO;
        assert_eq!(
            FullTransactionPublicInputsV1::try_from(public),
            Err(ZkTypeError::MissingPublicCommitment)
        );
    }
}
