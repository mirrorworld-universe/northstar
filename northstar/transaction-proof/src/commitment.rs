use {
    ark_bn254::Fr,
    ark_ff::{BigInteger, PrimeField},
    light_poseidon::{Poseidon, PoseidonHasher},
    northstar_zk_types::FrBytes,
};

pub const SESSION_CONTEXT_TAG: u64 = 0x100;
pub const ACCOUNT_TAG: u64 = 0x101;
pub const ACCOUNT_LIST_TAG: u64 = 0x102;
pub const TRANSACTION_TAG: u64 = 0x103;
pub const RUNTIME_TAG: u64 = 0x104;
pub const RESULT_TAG: u64 = 0x105;
pub const SETTLEMENT_TAG: u64 = 0x106;
pub const READONLY_TAG: u64 = 0x107;
pub const TRACE_SCHEMA_TAG: u64 = 0x108;
pub const TX_EFFECT_TAG: u64 = 0x109;
pub const BYTE_STRING_TAG: u64 = 0x10a;
pub const VM_TABLE_TAG: u64 = 0x10b;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitmentError {
    Poseidon,
    LengthOverflow,
}

pub fn fr_to_bytes(value: Fr) -> FrBytes {
    let encoded = value.into_bigint().to_bytes_be();
    let mut bytes = [0; 32];
    let start = bytes
        .len()
        .checked_sub(encoded.len())
        .expect("BN254 scalar encoding fits 32 bytes");
    bytes[start..].copy_from_slice(&encoded);
    FrBytes::new(bytes).expect("arkworks BN254 scalar is canonical")
}

pub fn fr_from_bytes(value: FrBytes) -> Fr {
    Fr::from_be_bytes_mod_order(value.as_bytes())
}

pub fn poseidon2(left: Fr, right: Fr) -> Result<Fr, CommitmentError> {
    Poseidon::<Fr>::new_circom(2)
        .and_then(|mut hasher| hasher.hash(&[left, right]))
        .map_err(|_| CommitmentError::Poseidon)
}

pub fn fold(tag: u64, values: &[Fr]) -> Result<Fr, CommitmentError> {
    let mut accumulator = Fr::from(tag);
    for chunk in values.chunks(11) {
        let capacity = chunk
            .len()
            .checked_add(1)
            .ok_or(CommitmentError::LengthOverflow)?;
        let mut inputs = Vec::with_capacity(capacity);
        inputs.push(accumulator);
        inputs.extend_from_slice(chunk);
        accumulator = Poseidon::<Fr>::new_circom(inputs.len())
            .and_then(|mut hasher| hasher.hash(&inputs))
            .map_err(|_| CommitmentError::Poseidon)?;
    }
    Ok(accumulator)
}

pub fn bytes(tag: u64, value: &[u8]) -> Result<Fr, CommitmentError> {
    let length = u64::try_from(value.len()).map_err(|_| CommitmentError::LengthOverflow)?;
    let capacity = value
        .len()
        .div_ceil(31)
        .checked_add(2)
        .ok_or(CommitmentError::LengthOverflow)?;
    let mut fields = Vec::with_capacity(capacity);
    fields.push(Fr::from(BYTE_STRING_TAG));
    fields.push(Fr::from(length));
    for chunk in value.chunks(31) {
        let mut encoded = [0; 32];
        let start = encoded
            .len()
            .checked_sub(chunk.len())
            .ok_or(CommitmentError::LengthOverflow)?;
        encoded[start..].copy_from_slice(chunk);
        fields.push(Fr::from_be_bytes_mod_order(&encoded));
    }
    fold(tag, &fields)
}

pub fn list(tag: u64, values: &[Fr]) -> Result<Fr, CommitmentError> {
    let length = u64::try_from(values.len()).map_err(|_| CommitmentError::LengthOverflow)?;
    let capacity = values
        .len()
        .checked_add(1)
        .ok_or(CommitmentError::LengthOverflow)?;
    let mut fields = Vec::with_capacity(capacity);
    fields.push(Fr::from(length));
    fields.extend_from_slice(values);
    fold(tag, &fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_commitments_are_length_delimited_and_chunked() {
        assert_ne!(bytes(1, &[1]).unwrap(), bytes(1, &[0, 1]).unwrap());
        assert_ne!(bytes(1, &[7; 31]).unwrap(), bytes(1, &[7; 32]).unwrap());
        assert_ne!(bytes(1, &[1, 2]).unwrap(), bytes(2, &[1, 2]).unwrap());
    }

    #[test]
    fn field_encoding_is_canonical() {
        let value = fold(9, &[Fr::from(1u64), Fr::from(2u64)]).unwrap();
        assert_eq!(fr_from_bytes(fr_to_bytes(value)), value);
    }
}
