use {
    super::*,
    solana_address_lookup_table_interface::state::{
        AddressLookupTable, LOOKUP_TABLE_MAX_ADDRESSES, LOOKUP_TABLE_META_SIZE,
    },
    solana_message::v0::LoadedAddresses,
};
const TAG: &[u8] = b"northstar-v2-alt-openings-v1";
const MAX_TABLE: usize = LOOKUP_TABLE_META_SIZE + LOOKUP_TABLE_MAX_ADDRESSES * 32;

// v0 signs table addresses and indices, not the resolved keys. Do not infer their order
// from sorted touched accounts or consult today's table on reopen.
pub(super) fn resolve(
    tx: &VersionedTransaction,
    replay: &[u8],
    mut opens: impl FnMut(&Pubkey, &Pubkey, &[u8]) -> bool,
) -> Result<Vec<Pubkey>, StateErrorV2> {
    let mut keys = tx.message.static_account_keys().to_vec();
    let lookups = tx.message.address_table_lookups().unwrap_or_default();
    if lookups.is_empty() {
        return Ok(keys);
    }
    let mut cursor = replay.strip_prefix(TAG).ok_or(INVALID)?;
    let owner = solana_sdk_ids::address_lookup_table::id();
    let mut loaded = LoadedAddresses::default();
    for lookup in lookups {
        let (length, rest) = cursor.split_at_checked(2).ok_or(INVALID)?;
        let size = usize::from(u16::from_le_bytes(length.try_into().map_err(|_| INVALID)?));
        if size > MAX_TABLE {
            return Err(INVALID);
        }
        let (data, rest) = rest.split_at_checked(size).ok_or(INVALID)?;
        cursor = rest;
        if !opens(&lookup.account_key, &owner, data) {
            return Err(INVALID);
        }
        let table = AddressLookupTable::deserialize(data).map_err(|_| INVALID)?;
        for (indices, out) in [
            (&lookup.writable_indexes, &mut loaded.writable),
            (&lookup.readonly_indexes, &mut loaded.readonly),
        ] {
            for &i in indices {
                out.push(*table.addresses.get(usize::from(i)).ok_or(INVALID)?);
            }
        }
    }
    if !cursor.is_empty() {
        return Err(INVALID);
    }
    keys.extend(loaded.writable);
    keys.extend(loaded.readonly);
    Ok(keys)
}

#[cfg(feature = "checkpoint-continuity-v2")]
pub(crate) fn capture(
    bank: &solana_runtime::bank::Bank,
    tx: &VersionedTransaction,
    inner: &[RecordedInnerInstructionV2],
    observed_slot: u64,
    replay: &mut Vec<u8>,
    readonly: &mut Vec<crate::checkpoint::ReadonlyL1ValueV1>,
) -> Result<(), StateErrorV2> {
    use solana_message::AddressLoader;
    let lookups = tx.message.address_table_lookups().unwrap_or_default();
    if lookups.is_empty() || !has_call(tx, inner) {
        return Ok(());
    }
    // SVM re-execution snapshots currently exist only for a separate legacy relation.
    if !replay.is_empty() {
        return Err(INVALID);
    }
    replay.extend_from_slice(TAG);
    for lookup in lookups {
        let table = bank.get_account(&lookup.account_key).ok_or(INVALID)?;
        if table.executable()
            || table.owner() != &solana_sdk_ids::address_lookup_table::id()
            || table.data().len() > MAX_TABLE
        {
            return Err(INVALID);
        }
        replay.extend_from_slice(
            &u16::try_from(table.data().len())
                .map_err(|_| INVALID)?
                .to_le_bytes(),
        );
        replay.extend_from_slice(table.data());
        readonly.push(crate::checkpoint::ReadonlyL1ValueV1 {
            account: lookup.account_key,
            owner: *table.owner(),
            lamports: table.lamports(),
            data_hash: hash_parts(&[table.data()]),
            observed_l1_slot: observed_slot,
        });
    }
    let loaded = bank.load_addresses(lookups).map_err(|_| INVALID)?;
    let keys = resolve(tx, replay, |_, _, _| true)?; // The bytes above came directly from this bank.
    if !keys[tx.message.static_account_keys().len()..]
        .iter()
        .eq(loaded.writable.iter().chain(&loaded.readonly))
    {
        return Err(INVALID);
    }
    Ok(())
}
