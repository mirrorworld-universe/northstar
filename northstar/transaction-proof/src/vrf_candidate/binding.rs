use {
    super::*,
    crate::{
        checkpoint::{self, StateAccountValueV1},
        commitment::{bytes, fr_to_bytes, SESSION_CONTEXT_TAG},
        Digest, Sha256, SESSION_CONTEXT_DOMAIN_V1, SETTLEMENT_POLICY_VERSION_V1,
    },
    northstar_zk_types::{ErStepPublicInputsV1, FrBytes},
    std::collections::{BTreeMap, BTreeSet},
};

pub fn candidate_profile_sha256(profile: &CandidateProfileV1) -> Result<Hash32, CandidateError> {
    digest(b"northstar-vrf-profile-v1", profile)
}

pub fn candidate_programs_sha256(
    programs: &[CandidateProgramV1],
) -> Result<Hash32, CandidateError> {
    digest(b"northstar-vrf-program-identities-v1", &programs)
}

pub fn encode_candidate(envelope: &CandidateEnvelopeV1) -> Result<Vec<u8>, CandidateError> {
    validate_encoded_length(envelope)?;
    borsh::to_vec(envelope).map_err(|_| CandidateError::Encoding)
}

pub fn decode_candidate(bytes: &[u8]) -> Result<CandidateEnvelopeV1, CandidateError> {
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(CandidateError::ResourceLimit);
    }
    let envelope = borsh::from_slice(bytes).map_err(|_| CandidateError::Encoding)?;
    if encode_candidate(&envelope)? != bytes {
        return Err(CandidateError::Encoding);
    }
    Ok(envelope)
}

pub fn verify_candidate<E: CandidateReplayEngine>(
    envelope: &CandidateEnvelopeV1,
    policy: &CandidatePolicyV1,
    engine: &mut E,
) -> Result<ErStepPublicInputsV1, CandidateError> {
    validate_encoded_length(envelope)?;
    if envelope.magic != CANDIDATE_MAGIC
        || envelope.version != CANDIDATE_VERSION
        || envelope.proof_kind != CANDIDATE_PROOF_KIND
        || envelope.proof_version != CANDIDATE_PROOF_VERSION
    {
        return Err(CandidateError::Domain);
    }
    let transaction = validate_candidate_input(&envelope.input, policy)?;
    validate_prestate(&envelope.input, &transaction, &envelope.checkpoint)?;
    let output = engine.execute(&envelope.input)?;
    if output != envelope.expected_output {
        return Err(CandidateError::Outcome);
    }
    bind_validated_candidate_execution(&envelope.input, &transaction, &output, &envelope.checkpoint)
}

pub fn validate_candidate_input(
    input: &CandidateExecutionInputV1,
    policy: &CandidatePolicyV1,
) -> Result<CandidateSignedTransactionV1, CandidateError> {
    if input.session_context.len() != 149
        || !input.session_context.starts_with(SESSION_CONTEXT_DOMAIN_V1)
        || input.session_context.last() != Some(&SETTLEMENT_POLICY_VERSION_V1)
    {
        return Err(CandidateError::Domain);
    }
    if policy.profile_sha256 == [0; 32]
        || candidate_profile_sha256(&input.profile)? != policy.profile_sha256
    {
        return Err(CandidateError::Profile);
    }
    let profile = &input.profile;
    if candidate_programs_sha256(&input.programs)? != profile.program_identities_sha256 {
        return Err(CandidateError::Profile);
    }
    if profile.version != 1
        || profile.library_revision
            != [
                0xd1, 0xb8, 0xc3, 0xff, 0xc5, 0x7a, 0x0d, 0x6c, 0xaa, 0x50, 0xb8, 0xd1, 0x78, 0x93,
                0xf0, 0x36, 0xbf, 0x61, 0xca, 0xbd,
            ]
        || profile.runtime_revision == [0; 20]
        || profile.agave_revision == [0; 20]
        || profile.vm_config_sha256 == [0; 32]
        || profile.syscall_registry_sha256 == [0; 32]
        || profile.compute_budget_sha256 == [0; 32]
    {
        return Err(CandidateError::Profile);
    }
    if profile.checkpoint_steps == 0
        || profile.checkpoint_steps > 16
        || input.step_index >= profile.checkpoint_steps
    {
        return Err(CandidateError::Checkpoint);
    }
    // This ER-native profile has zero transaction fees. Escrow is executed by System CPI.
    if profile.lamports_per_signature != 0 {
        return Err(CandidateError::Unsupported(CandidateUnsupported::FeePolicy));
    }
    if profile.heap_bytes != 32_768
        || input.runtime.heap_bytes != profile.heap_bytes
        || profile.max_compute_units == 0
        || profile.max_compute_units > MAX_COMPUTE_UNITS
        || profile.max_processing_age > 2047
        || input.er_slot != input.runtime.slot
        || input.runtime.observation_slot != input.runtime.slot
    {
        return Err(CandidateError::Runtime);
    }
    if profile
        .active_features
        .windows(2)
        .any(|pair| pair[0].id >= pair[1].id)
        || profile
            .inactive_features
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || profile.active_features.iter().any(|feature| {
            feature.activation_slot > input.runtime.slot
                || profile.inactive_features.binary_search(&feature.id).is_ok()
        })
    {
        return Err(CandidateError::Profile);
    }
    let transaction = admit_signed_transaction(&input.transaction_bytes)?;
    if transaction.compute_unit_limit != input.runtime.compute_unit_limit
        || transaction.compute_unit_limit > profile.max_compute_units
    {
        return Err(CandidateError::Runtime);
    }
    if input
        .runtime
        .recent_blockhashes
        .windows(2)
        .any(|pair| pair[0].blockhash >= pair[1].blockhash)
        || !input.runtime.recent_blockhashes.iter().any(|entry| {
            entry.blockhash == transaction.recent_blockhash
                && entry.age <= profile.max_processing_age
                && entry.lamports_per_signature == 0
        })
    {
        return Err(CandidateError::Runtime);
    }
    canonical_accounts(&input.accounts)?;
    for key in &transaction.account_keys {
        account(input, key)?;
    }
    let payer = account(input, &transaction.account_keys[0])?
        .state
        .as_ref()
        .ok_or(CandidateError::Accounts)?;
    if payer.owner != [0; 32] || payer.executable || !payer.data.is_empty() {
        return Err(CandidateError::Accounts);
    }
    let app = &transaction.instructions[1];
    let request_role =
        if transaction.account_keys[usize::from(app.program_id_index)] == TEST_CONSUMER_ID {
            5
        } else {
            2
        };
    let request = account(
        input,
        &transaction.account_keys[usize::from(app.accounts[request_role])],
    )?;
    if (request_role == 5 && request.state.is_some())
        || (request_role == 2 && request.state.is_none())
    {
        return Err(CandidateError::Unsupported(
            CandidateUnsupported::AccountLifecycle,
        ));
    }
    for key in [CLOCK_ID, SLOT_HASHES_ID, RENT_ID, EPOCH_SCHEDULE_ID] {
        let state = account(input, &key)?
            .state
            .as_ref()
            .ok_or(CandidateError::Runtime)?;
        if state.executable || state.owner != SYSVAR_OWNER_ID {
            return Err(CandidateError::Runtime);
        }
    }
    let clock = account(input, &CLOCK_ID)?
        .state
        .as_ref()
        .ok_or(CandidateError::Runtime)?;
    if clock.data.len() != 40
        || u64::from_le_bytes(
            clock.data[..8]
                .try_into()
                .map_err(|_| CandidateError::Runtime)?,
        ) != input.runtime.slot
    {
        return Err(CandidateError::Runtime);
    }
    let hashes = account(input, &SLOT_HASHES_ID)?
        .state
        .as_ref()
        .ok_or(CandidateError::Runtime)?;
    if hashes.data.len() < 48 {
        return Err(CandidateError::Runtime);
    }
    let count = u64::from_le_bytes(
        hashes.data[..8]
            .try_into()
            .map_err(|_| CandidateError::Runtime)?,
    );
    let required = count
        .checked_mul(40)
        .and_then(|value| value.checked_add(8))
        .ok_or(CandidateError::Runtime)?;
    if count == 0 || count > 512 || required > hashes.data.len() as u64 {
        return Err(CandidateError::Runtime);
    }
    validate_programs(input, policy)?;
    Ok(transaction)
}

fn validate_programs(
    input: &CandidateExecutionInputV1,
    policy: &CandidatePolicyV1,
) -> Result<(), CandidateError> {
    let mut required = [COORDINATOR_ID, REFERENCE_VERIFY_ID, TEST_CONSUMER_ID];
    required.sort_unstable();
    if input.programs.len() != 3
        || input
            .programs
            .windows(2)
            .any(|pair| pair[0].program_id >= pair[1].program_id)
        || input.programs.as_ref() != policy.programs.as_slice()
    {
        return Err(CandidateError::Program);
    }
    let mut identities = BTreeSet::new();
    for (program, id) in input.programs.iter().zip(required) {
        if program.program_id != id
            || program.loader_id != UPGRADEABLE_LOADER_ID
            || program.sbpf_version != 2
            || !identities.insert(program.programdata_id)
            || required.contains(&program.programdata_id)
        {
            return Err(CandidateError::Program);
        }
        let executable = account(input, &program.program_id)?
            .state
            .as_ref()
            .ok_or(CandidateError::Program)?;
        let data = account(input, &program.programdata_id)?
            .state
            .as_ref()
            .ok_or(CandidateError::Program)?;
        if !executable.executable
            || executable.owner != program.loader_id
            || executable.data.len() != 36
            || executable.data[..4] != 2u32.to_le_bytes()
            || executable.data[4..] != program.programdata_id
            || data.executable
            || data.owner != program.loader_id
            || data.data.len() <= UPGRADEABLE_PROGRAMDATA_METADATA_BYTES
            || data.data[..4] != 3u32.to_le_bytes()
            || data.data[12] > 1
            || u64::from_le_bytes(
                data.data[4..12]
                    .try_into()
                    .map_err(|_| CandidateError::Program)?,
            ) > input.runtime.slot
            || sha256(&data.data[UPGRADEABLE_PROGRAMDATA_METADATA_BYTES..]) != program.elf_sha256
        {
            return Err(CandidateError::Program);
        }
    }
    Ok(())
}

fn canonical_accounts(accounts: &[CandidateAccountInputV1]) -> Result<(), CandidateError> {
    if accounts.windows(2).any(|pair| pair[0].key >= pair[1].key)
        || accounts.iter().any(|account| {
            account
                .state
                .as_ref()
                .is_some_and(|state| state.lamports == 0)
        })
    {
        return Err(CandidateError::Accounts);
    }
    Ok(())
}

fn account<'a>(
    input: &'a CandidateExecutionInputV1,
    key: &Hash32,
) -> Result<&'a CandidateAccountInputV1, CandidateError> {
    input
        .accounts
        .binary_search_by_key(key, |account| account.key)
        .map(|index| &input.accounts[index])
        .map_err(|_| CandidateError::Accounts)
}

pub fn candidate_state_value(key: Hash32, state: &CandidateAccountStateV1) -> StateAccountValueV1 {
    StateAccountValueV1 {
        account: key,
        owner: state.owner,
        lamports: state.lamports,
        executable: state.executable,
        rent_epoch: state.rent_epoch,
        data_hash: sha256(&state.data),
    }
}

fn canonical_state(values: &[StateAccountValueV1]) -> Result<(), CandidateError> {
    if values
        .windows(2)
        .any(|pair| pair[0].account >= pair[1].account)
        || values
            .iter()
            .any(|value| value.lamports == 0 || value.data_hash == [0; 32])
    {
        return Err(CandidateError::Checkpoint);
    }
    Ok(())
}

fn validate_prestate(
    input: &CandidateExecutionInputV1,
    transaction: &CandidateSignedTransactionV1,
    checkpoint: &CandidateCheckpointV1,
) -> Result<(), CandidateError> {
    canonical_state(&checkpoint.pre_state)?;
    for entry in input.accounts.iter() {
        let existing = checkpoint
            .pre_state
            .binary_search_by_key(&entry.key, |value| value.account)
            .ok()
            .map(|index| checkpoint.pre_state[index]);
        let writable = transaction
            .account_keys
            .iter()
            .position(|key| *key == entry.key)
            .is_some_and(|index| transaction.is_writable(index));
        let expected = entry
            .state
            .as_ref()
            .map(|state| candidate_state_value(entry.key, state));
        if (writable || existing.is_some()) && existing != expected {
            return Err(CandidateError::Checkpoint);
        }
    }
    let context_path = &checkpoint.readonly_context_path;
    if checkpoint.transaction_effect_path.leaf_index != input.step_index
        || context_path.leaf_index != input.step_index
        || context_path.leaf_count != input.profile.checkpoint_steps
        || context_path.leaf_count != checkpoint.transaction_effect_path.leaf_count
        || candidate_readonly_context_root(readonly_leaf(input, transaction)?, context_path)?
            != checkpoint.readonly_context_root
    {
        return Err(CandidateError::Checkpoint);
    }
    if checkpoint.settlement_effect_root != empty_candidate_settlement_root() {
        return Err(CandidateError::Unsupported(
            CandidateUnsupported::CrossLayerSettlement,
        ));
    }
    Ok(())
}

/// Bind an output already recomputed by the actual interpreter. This function alone
/// is not execution verification; the approved guest uses verify_candidate above.
pub fn bind_candidate_execution(
    input: &CandidateExecutionInputV1,
    output: &CandidateExecutionOutputV1,
    checkpoint: &CandidateCheckpointV1,
) -> Result<ErStepPublicInputsV1, CandidateError> {
    let transaction = admit_signed_transaction(&input.transaction_bytes)?;
    validate_prestate(input, &transaction, checkpoint)?;
    bind_validated_candidate_execution(input, &transaction, output, checkpoint)
}

// Callers authenticate this immutable input's wire and checkpoint prestate first.
// verify_candidate also compares the actual engine output before reaching here.
fn bind_validated_candidate_execution(
    input: &CandidateExecutionInputV1,
    transaction: &CandidateSignedTransactionV1,
    output: &CandidateExecutionOutputV1,
    checkpoint: &CandidateCheckpointV1,
) -> Result<ErStepPublicInputsV1, CandidateError> {
    canonical_accounts(&output.accounts)?;
    if output.accounts.len() != input.accounts.len()
        || output.transaction_fee != 0
        || output.prioritization_fee != 0
        || output.executed_units == 0
        || output.executed_units > input.runtime.compute_unit_limit
    {
        return Err(CandidateError::Outcome);
    }
    let mut expected_state = checkpoint
        .pre_state
        .iter()
        .map(|value| (value.account, *value))
        .collect::<BTreeMap<_, _>>();
    for (pre, post) in input.accounts.iter().zip(output.accounts.iter()) {
        if pre.key != post.key {
            return Err(CandidateError::Accounts);
        }
        let writable = transaction
            .account_keys
            .iter()
            .position(|key| *key == pre.key)
            .is_some_and(|index| transaction.is_writable(index));
        if !writable && pre != post {
            return Err(CandidateError::Accounts);
        }
        if !matches!(output.status, CandidateExecutionStatusV1::Success) && pre != post {
            // The real loader may normalize the fee payer's rent_epoch before the
            // rollback account is captured, even with a zero transaction fee.
            if pre.key != transaction.account_keys[0] || !fee_payer_epoch_only(pre, post) {
                return Err(CandidateError::Outcome);
            }
        }
        if writable {
            match &post.state {
                Some(state) => {
                    expected_state.insert(post.key, candidate_state_value(post.key, state));
                }
                None => {
                    expected_state.remove(&post.key);
                }
            }
        }
    }
    if let CandidateExecutionStatusV1::InstructionFailure {
        instruction_index,
        error_kind,
        ..
    } = output.status
    {
        if instruction_index != 1 || error_kind != 1 {
            return Err(CandidateError::Outcome);
        }
    }
    canonical_state(&checkpoint.post_state)?;
    if expected_state.into_values().collect::<Vec<_>>() != checkpoint.post_state.to_vec() {
        return Err(CandidateError::Checkpoint);
    }
    let leaf = candidate_transaction_effect_leaf(input, output)?;
    let path = &checkpoint.transaction_effect_path;
    if path.leaf_index != input.step_index
        || candidate_transaction_root(leaf, path)? != checkpoint.transaction_effect_root
    {
        return Err(CandidateError::Checkpoint);
    }
    let public = ErStepPublicInputsV1 {
        domain: FrBytes::er_step_domain_v1(CANDIDATE_PROOF_KIND, CANDIDATE_PROOF_VERSION),
        session_context: fr_to_bytes(
            bytes(SESSION_CONTEXT_TAG, &input.session_context)
                .map_err(|_| CandidateError::Checkpoint)?,
        ),
        slot_step: FrBytes::from_u64_pair(input.er_slot, u64::from(input.step_index)),
        pre_state_root: FrBytes::new(
            checkpoint::state_root(&checkpoint.pre_state)
                .map_err(|_| CandidateError::Checkpoint)?,
        )
        .map_err(|_| CandidateError::Checkpoint)?,
        post_state_root: FrBytes::new(
            checkpoint::state_root(&checkpoint.post_state)
                .map_err(|_| CandidateError::Checkpoint)?,
        )
        .map_err(|_| CandidateError::Checkpoint)?,
        // Kind 3 binds the checkpoint transaction tree root, not legacy's selected leaf.
        tx_effect_root: FrBytes::new(checkpoint.transaction_effect_root)
            .map_err(|_| CandidateError::Checkpoint)?,
        readonly_l1_root: FrBytes::new(checkpoint.readonly_context_root)
            .map_err(|_| CandidateError::Checkpoint)?,
        settlement_effect_root: FrBytes::new(checkpoint.settlement_effect_root)
            .map_err(|_| CandidateError::Checkpoint)?,
    };
    for commitment in [
        public.session_context,
        public.pre_state_root,
        public.post_state_root,
        public.tx_effect_root,
        public.readonly_l1_root,
        public.settlement_effect_root,
    ] {
        if commitment == FrBytes::ZERO {
            return Err(CandidateError::Checkpoint);
        }
    }
    Ok(public)
}

/// Construct the candidate checkpoint from a claimed output and the complete
/// checkpoint pre-state set. This builds commitments, not execution evidence;
/// verify_candidate must still invoke the actual engine and compare the output.
pub fn build_candidate_checkpoint(
    input: &CandidateExecutionInputV1,
    output: &CandidateExecutionOutputV1,
    pre_state: BoundedVec<StateAccountValueV1, MAX_STATE_ACCOUNTS>,
    path: CandidateMerklePathV1,
    context_path: CandidateMerklePathV1,
) -> Result<CandidateCheckpointV1, CandidateError> {
    canonical_state(&pre_state)?;
    let transaction = admit_signed_transaction(&input.transaction_bytes)?;
    let mut post_state = pre_state
        .iter()
        .map(|value| (value.account, *value))
        .collect::<BTreeMap<_, _>>();
    for entry in output.accounts.iter() {
        if transaction
            .account_keys
            .iter()
            .position(|key| *key == entry.key)
            .is_some_and(|index| transaction.is_writable(index))
        {
            match &entry.state {
                Some(state) => {
                    post_state.insert(entry.key, candidate_state_value(entry.key, state));
                }
                None => {
                    post_state.remove(&entry.key);
                }
            }
        }
    }
    let checkpoint = CandidateCheckpointV1 {
        pre_state,
        post_state: BoundedVec::new(post_state.into_values().collect())?,
        transaction_effect_root: candidate_transaction_root(
            candidate_transaction_effect_leaf(input, output)?,
            &path,
        )?,
        transaction_effect_path: path,
        readonly_context_root: candidate_readonly_context_root(
            readonly_leaf(input, &transaction)?,
            &context_path,
        )?,
        readonly_context_path: context_path,
        settlement_effect_root: empty_candidate_settlement_root(),
    };
    bind_candidate_execution(input, output, &checkpoint)?;
    Ok(checkpoint)
}

fn fee_payer_epoch_only(pre: &CandidateAccountInputV1, post: &CandidateAccountInputV1) -> bool {
    let (Some(pre), Some(post)) = (&pre.state, &post.state) else {
        return false;
    };
    pre.lamports == post.lamports
        && pre.owner == post.owner
        && pre.executable == post.executable
        && pre.data == post.data
        && post.rent_epoch == u64::MAX
}

pub fn candidate_log_commitment(logs: &[String]) -> Result<Hash32, CandidateError> {
    digest(b"northstar-vrf-execution-logs-v1", &logs)
}

/// Convenience root for a single-step checkpoint. Multi-step builders supply a
/// context inclusion path to candidate_readonly_context_root instead.
pub fn candidate_readonly_root(
    input: &CandidateExecutionInputV1,
) -> Result<Hash32, CandidateError> {
    if input.profile.checkpoint_steps != 1 {
        return Err(CandidateError::Checkpoint);
    }
    candidate_readonly_context_root(
        candidate_readonly_context_leaf(input)?,
        &CandidateMerklePathV1 {
            leaf_index: input.step_index,
            leaf_count: 1,
            siblings: BoundedVec::default(),
        },
    )
}

pub fn candidate_readonly_context_leaf(
    input: &CandidateExecutionInputV1,
) -> Result<Hash32, CandidateError> {
    readonly_leaf(input, &admit_signed_transaction(&input.transaction_bytes)?)
}

fn readonly_leaf(
    input: &CandidateExecutionInputV1,
    transaction: &CandidateSignedTransactionV1,
) -> Result<Hash32, CandidateError> {
    let readonly = input
        .accounts
        .iter()
        .filter(|entry| {
            !transaction
                .account_keys
                .iter()
                .position(|key| *key == entry.key)
                .is_some_and(|index| transaction.is_writable(index))
        })
        .collect::<Vec<_>>();
    field_digest(
        b"northstar-vrf-readonly-context-v1",
        &(
            input.step_index,
            input.er_slot,
            input.runtime.observation_slot,
            &input.profile,
            &input.runtime,
            &input.programs,
            readonly,
        ),
    )
}

pub fn candidate_transaction_effect_leaf(
    input: &CandidateExecutionInputV1,
    output: &CandidateExecutionOutputV1,
) -> Result<Hash32, CandidateError> {
    field_digest(
        b"northstar-vrf-transaction-effect-v1",
        &(
            input.step_index,
            sha256(&input.transaction_bytes),
            candidate_profile_sha256(&input.profile)?,
            output,
        ),
    )
}

pub fn candidate_transaction_root(
    leaf: Hash32,
    path: &CandidateMerklePathV1,
) -> Result<Hash32, CandidateError> {
    candidate_tree_root(
        leaf,
        path,
        b"northstar-vrf-transaction-node-v1",
        b"northstar-vrf-transaction-root-v1",
    )
}

pub fn candidate_readonly_context_root(
    leaf: Hash32,
    path: &CandidateMerklePathV1,
) -> Result<Hash32, CandidateError> {
    candidate_tree_root(
        leaf,
        path,
        b"northstar-vrf-context-node-v1",
        b"northstar-vrf-context-root-v1",
    )
}

fn candidate_tree_root(
    leaf: Hash32,
    path: &CandidateMerklePathV1,
    node_tag: &[u8],
    root_tag: &[u8],
) -> Result<Hash32, CandidateError> {
    if path.leaf_count == 0
        || path.leaf_count > 16
        || path.leaf_index >= path.leaf_count
        || path.siblings.len() != path.leaf_count.next_power_of_two().trailing_zeros() as usize
    {
        return Err(CandidateError::Checkpoint);
    }
    let mut current = leaf;
    let mut index = path.leaf_index;
    for (level, sibling) in path.siblings.iter().enumerate() {
        current = if index.is_multiple_of(2) {
            field_digest(node_tag, &(level as u32, current, sibling))?
        } else {
            field_digest(node_tag, &(level as u32, sibling, current))?
        };
        index /= 2;
    }
    field_digest(root_tag, &(path.leaf_count, current))
}

pub fn empty_candidate_settlement_root() -> Hash32 {
    let mut hash = sha256(b"northstar-vrf-er-native-no-cross-layer-settlement-v1");
    hash[0] &= 0x1f;
    hash
}

fn validate_encoded_length(envelope: &CandidateEnvelopeV1) -> Result<(), CandidateError> {
    if borsh::object_length(envelope).map_err(|_| CandidateError::Encoding)? > MAX_ENVELOPE_BYTES {
        return Err(CandidateError::ResourceLimit);
    }
    Ok(())
}

fn sha256(value: &[u8]) -> Hash32 {
    Sha256::digest(value).into()
}
fn digest<T: BorshSerialize>(tag: &[u8], value: &T) -> Result<Hash32, CandidateError> {
    let bytes = borsh::to_vec(value).map_err(|_| CandidateError::Encoding)?;
    let mut hash = Sha256::new();
    hash.update(tag);
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
    Ok(hash.finalize().into())
}
fn field_digest<T: BorshSerialize>(tag: &[u8], value: &T) -> Result<Hash32, CandidateError> {
    let mut hash = digest(tag, value)?;
    hash[0] &= 0x1f;
    Ok(hash)
}
