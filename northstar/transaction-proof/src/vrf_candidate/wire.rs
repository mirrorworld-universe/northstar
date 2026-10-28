use {
    super::*,
    crate::{read_short_vec, take, take_array, Digest, Sha256},
    ed25519_dalek::{Signature, VerifyingKey},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInstructionV1 {
    pub program_id_index: u8,
    pub accounts: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSignedTransactionV1 {
    pub header: [u8; 3],
    pub account_keys: Vec<Hash32>,
    pub recent_blockhash: Hash32,
    pub instructions: Vec<CandidateInstructionV1>,
    pub message_bytes: Vec<u8>,
    pub signature: [u8; 64],
    pub compute_unit_limit: u64,
}

impl CandidateSignedTransactionV1 {
    pub fn is_writable(&self, index: usize) -> bool {
        let Some(writable_unsigned_end) = self
            .account_keys
            .len()
            .checked_sub(usize::from(self.header[2]))
        else {
            return false;
        };
        index < writable_unsigned_end
    }
}

fn wire_error(_: crate::ReplayError) -> CandidateError {
    CandidateError::Wire
}

/// This parser is separate from, and cannot broaden, the frozen legacy parser.
pub fn admit_signed_transaction(
    bytes: &[u8],
) -> Result<CandidateSignedTransactionV1, CandidateError> {
    if bytes.len() > MAX_TRANSACTION_BYTES {
        return Err(CandidateError::ResourceLimit);
    }
    let mut cursor = 0;
    if read_short_vec(bytes, &mut cursor).map_err(wire_error)? != 1 {
        return Err(CandidateError::Unsupported(
            CandidateUnsupported::TransactionShape,
        ));
    }
    let signature = take_array(bytes, &mut cursor).map_err(wire_error)?;
    let start = cursor;
    let header: [u8; 3] = take_array(bytes, &mut cursor).map_err(wire_error)?;
    if header[0] & 0x80 != 0 {
        return Err(CandidateError::Unsupported(
            CandidateUnsupported::VersionedMessage,
        ));
    }
    if header[0] != 1 || header[1] != 0 {
        return Err(CandidateError::Wire);
    }
    let key_count = read_short_vec(bytes, &mut cursor).map_err(wire_error)?;
    let writable_count = key_count
        .checked_sub(usize::from(header[2]))
        .ok_or(CandidateError::Wire)?;
    if !(9..=10).contains(&key_count) || !(3..=4).contains(&writable_count) {
        return Err(CandidateError::Unsupported(
            CandidateUnsupported::TransactionShape,
        ));
    }
    let mut account_keys = Vec::new();
    for _ in 0..key_count {
        let key = take_array(bytes, &mut cursor).map_err(wire_error)?;
        if account_keys.contains(&key) {
            return Err(CandidateError::Accounts);
        }
        account_keys.push(key);
    }
    let recent_blockhash = take_array(bytes, &mut cursor).map_err(wire_error)?;
    if read_short_vec(bytes, &mut cursor).map_err(wire_error)? != 2 {
        return Err(CandidateError::Unsupported(
            CandidateUnsupported::TransactionShape,
        ));
    }
    let mut instructions = Vec::new();
    for _ in 0..2 {
        let [program_id_index] = take_array::<1>(bytes, &mut cursor).map_err(wire_error)?;
        if usize::from(program_id_index) >= key_count {
            return Err(CandidateError::Wire);
        }
        let count = read_short_vec(bytes, &mut cursor).map_err(wire_error)?;
        let accounts = take(bytes, &mut cursor, count)
            .map_err(wire_error)?
            .to_vec();
        if accounts
            .iter()
            .any(|index| usize::from(*index) >= key_count)
        {
            return Err(CandidateError::Wire);
        }
        let count = read_short_vec(bytes, &mut cursor).map_err(wire_error)?;
        let data = take(bytes, &mut cursor, count)
            .map_err(wire_error)?
            .to_vec();
        instructions.push(CandidateInstructionV1 {
            program_id_index,
            accounts,
            data,
        });
    }
    if cursor != bytes.len() {
        return Err(CandidateError::Wire);
    }
    let compute = &instructions[0];
    if account_keys[usize::from(compute.program_id_index)] != COMPUTE_BUDGET_ID
        || !compute.accounts.is_empty()
        || compute.data.len() != 5
        || compute.data[0] != 2
    {
        return Err(CandidateError::Unsupported(
            CandidateUnsupported::TransactionShape,
        ));
    }
    let compute_unit_limit = u64::from(u32::from_le_bytes(
        compute.data[1..5]
            .try_into()
            .map_err(|_| CandidateError::Wire)?,
    ));
    if compute_unit_limit == 0 || compute_unit_limit > MAX_COMPUTE_UNITS {
        return Err(CandidateError::Runtime);
    }
    let app = &instructions[1];
    let program = account_keys[usize::from(app.program_id_index)];
    let (name, account_count) = match program {
        TEST_CONSUMER_ID => ("make_request", 9),
        COORDINATOR_ID => ("fulfill", 8),
        _ => return Err(CandidateError::Unsupported(CandidateUnsupported::Program)),
    };
    if app.accounts.len() != account_count
        || app.data.get(..8) != Some(&Sha256::digest(format!("global:{name}"))[..8])
    {
        return Err(CandidateError::Unsupported(
            CandidateUnsupported::TransactionShape,
        ));
    }
    if app
        .accounts
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != app.accounts.len()
    {
        return Err(CandidateError::Accounts);
    }
    // Logical payer roles alias inside Coordinator's CPI, not in these outer maps.
    if app.accounts[0] != 0 {
        return Err(CandidateError::Accounts);
    }
    let key = VerifyingKey::from_bytes(&account_keys[0]).map_err(|_| CandidateError::Signature)?;
    key.verify_strict(&bytes[start..], &Signature::from_bytes(&signature))
        .map_err(|_| CandidateError::Signature)?;
    Ok(CandidateSignedTransactionV1 {
        header,
        account_keys,
        recent_blockhash,
        instructions,
        message_bytes: bytes[start..].to_vec(),
        signature,
        compute_unit_limit,
    })
}
