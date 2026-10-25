//! Binding-only fixtures. The test engine below does not execute SBPF or verify VRF.
//! Actual interpreter differential tests belong to northstar-vrf-sbpf-replay.

use {
    borsh::BorshDeserialize,
    ed25519_dalek::{Signer, SigningKey},
    northstar_transaction_proof::{
        checkpoint::StateAccountValueV1, public_inputs_bytes, session_context_bytes_v1,
        vrf_candidate::*,
    },
    northstar_zk_types::FullTransactionPublicInputsV1,
    sha2::{Digest, Sha256},
};

fn bounded<T, const MAX: usize>(values: Vec<T>) -> BoundedVec<T, MAX> {
    BoundedVec::new(values).unwrap()
}
fn hash(value: &[u8]) -> Hash32 {
    Sha256::digest(value).into()
}

struct BindingOnlyEngine {
    output: CandidateExecutionOutputV1,
    calls: usize,
}
impl CandidateReplayEngine for BindingOnlyEngine {
    fn execute(
        &mut self,
        _: &CandidateExecutionInputV1,
    ) -> Result<CandidateExecutionOutputV1, CandidateError> {
        self.calls = self.calls.checked_add(1).unwrap();
        Ok(self.output.clone())
    }
}

fn state(owner: Hash32, data: Vec<u8>) -> CandidateAccountStateV1 {
    CandidateAccountStateV1 {
        lamports: 1_000_000,
        owner,
        executable: false,
        rent_epoch: u64::MAX,
        data: bounded(data),
    }
}
fn sign(message: &[u8]) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend_from_slice(&SigningKey::from_bytes(&[42; 32]).sign(message).to_bytes());
    bytes.extend_from_slice(message);
    bytes
}
fn transaction(fulfill: bool) -> Vec<u8> {
    let payer = SigningKey::from_bytes(&[42; 32]).verifying_key().to_bytes();
    let (readonly, keys, accounts, name) = if fulfill {
        (
            6,
            vec![
                payer,
                [42; 32],
                [43; 32],
                [41; 32],
                [44; 32],
                [45; 32],
                TEST_CONSUMER_ID,
                REFERENCE_VERIFY_ID,
                COORDINATOR_ID,
                COMPUTE_BUDGET_ID,
            ],
            vec![0, 4, 1, 2, 5, 6, 7, 3],
            "fulfill",
        )
    } else {
        (
            7,
            vec![
                payer,
                [41; 32],
                [42; 32],
                [43; 32],
                [44; 32],
                SLOT_HASHES_ID,
                COORDINATOR_ID,
                [0; 32],
                TEST_CONSUMER_ID,
                COMPUTE_BUDGET_ID,
            ],
            vec![0, 1, 3, 8, 4, 2, 5, 6, 7],
            "make_request",
        )
    };
    let mut message = vec![1, 0, readonly, 10];
    for key in keys {
        message.extend_from_slice(&key);
    }
    message.extend_from_slice(&[241; 32]);
    message.extend_from_slice(&[2, 9, 0, 5, 2]);
    message.extend_from_slice(&500_000u32.to_le_bytes());
    message.extend_from_slice(&[8, accounts.len() as u8]);
    message.extend_from_slice(&accounts);
    let mut data = hash(format!("global:{name}").as_bytes())[..8].to_vec();
    if fulfill {
        // A deliberately non-verifying payload; this fixture tests bindings only.
        data.extend_from_slice(&80u32.to_le_bytes());
        data.extend_from_slice(&[85; 80]);
    }
    message.push(u8::try_from(data.len()).unwrap());
    message.extend_from_slice(&data);
    sign(&message)
}

fn fixture(fulfill: bool) -> (CandidateEnvelopeV1, CandidatePolicyV1, BindingOnlyEngine) {
    let wire = transaction(fulfill);
    let transaction = admit_signed_transaction(&wire).unwrap();
    let mut accounts = transaction
        .account_keys
        .iter()
        .map(|key| CandidateAccountInputV1 {
            key: *key,
            state: Some(state([0; 32], vec![])),
        })
        .collect::<Vec<_>>();
    if !fulfill {
        accounts
            .iter_mut()
            .find(|a| a.key == [42; 32])
            .unwrap()
            .state = None;
    }
    let mut programs = Vec::new();
    let mut program_ids = [COORDINATOR_ID, REFERENCE_VERIFY_ID, TEST_CONSUMER_ID];
    program_ids.sort_unstable();
    for (index, program_id) in program_ids.into_iter().enumerate() {
        let programdata_id = [u8::try_from(index).unwrap().checked_add(150).unwrap(); 32];
        let elf = [b"binding-only-not-executable".as_slice(), &program_id].concat();
        let mut program_data = 2u32.to_le_bytes().to_vec();
        program_data.extend_from_slice(&programdata_id);
        let mut executable = state(UPGRADEABLE_LOADER_ID, program_data);
        executable.executable = true;
        if let Some(entry) = accounts.iter_mut().find(|a| a.key == program_id) {
            entry.state = Some(executable);
        } else {
            accounts.push(CandidateAccountInputV1 {
                key: program_id,
                state: Some(executable),
            });
        }
        let mut data = vec![0; 45];
        data[..4].copy_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&elf);
        accounts.push(CandidateAccountInputV1 {
            key: programdata_id,
            state: Some(state(UPGRADEABLE_LOADER_ID, data)),
        });
        programs.push(CandidateProgramV1 {
            program_id,
            programdata_id,
            loader_id: UPGRADEABLE_LOADER_ID,
            sbpf_version: 2,
            elf_sha256: hash(&elf),
        });
    }
    let mut clock = vec![0; 40];
    clock[..8].copy_from_slice(&20u64.to_le_bytes());
    let mut hashes = 1u64.to_le_bytes().to_vec();
    hashes.extend_from_slice(&19u64.to_le_bytes());
    hashes.extend_from_slice(&[19; 32]);
    for (key, data) in [
        (CLOCK_ID, clock),
        (SLOT_HASHES_ID, hashes),
        (RENT_ID, vec![0; 17]),
        (EPOCH_SCHEDULE_ID, vec![0; 33]),
    ] {
        let value = Some(state(SYSVAR_OWNER_ID, data));
        if let Some(entry) = accounts.iter_mut().find(|a| a.key == key) {
            entry.state = value;
        } else {
            accounts.push(CandidateAccountInputV1 { key, state: value });
        }
    }
    accounts.sort_by_key(|account| account.key);
    let profile = CandidateProfileV1 {
        version: 1,
        library_revision: [
            0xd1, 0xb8, 0xc3, 0xff, 0xc5, 0x7a, 0x0d, 0x6c, 0xaa, 0x50, 0xb8, 0xd1, 0x78, 0x93,
            0xf0, 0x36, 0xbf, 0x61, 0xca, 0xbd,
        ],
        runtime_revision: [1; 20],
        agave_revision: [2; 20],
        program_identities_sha256: candidate_programs_sha256(&programs).unwrap(),
        active_features: bounded(vec![CandidateFeatureV1 {
            id: [11; 32],
            activation_slot: 0,
        }]),
        inactive_features: bounded(vec![[12; 32]]),
        vm_config_sha256: [1; 32],
        syscall_registry_sha256: [2; 32],
        compute_budget_sha256: [3; 32],
        checkpoint_steps: 1,
        lamports_per_signature: 0,
        max_compute_units: 1_400_000,
        heap_bytes: 32_768,
        max_processing_age: 1800,
    };
    let policy = CandidatePolicyV1 {
        profile_sha256: candidate_profile_sha256(&profile).unwrap(),
        programs: programs.clone().try_into().unwrap(),
    };
    let input = CandidateExecutionInputV1 {
        session_context: bounded(session_context_bytes_v1([7; 32], [8; 32], 1, 0, [9; 32])),
        er_slot: 20,
        step_index: 0,
        transaction_bytes: bounded(wire),
        profile,
        runtime: CandidateRuntimeV1 {
            slot: 20,
            observation_slot: 20,
            recent_blockhashes: bounded(vec![CandidateRecentBlockhashV1 {
                blockhash: [241; 32],
                age: 0,
                lamports_per_signature: 0,
            }]),
            compute_unit_limit: 500_000,
            heap_bytes: 32_768,
        },
        accounts: bounded(accounts.clone()),
        programs: bounded(programs),
    };
    let mut post = accounts;
    if fulfill {
        post.iter_mut().find(|a| a.key == [42; 32]).unwrap().state = None;
    } else {
        post.iter_mut().find(|a| a.key == [42; 32]).unwrap().state =
            Some(state(COORDINATOR_ID, vec![1]));
    }
    let output = CandidateExecutionOutputV1 {
        status: CandidateExecutionStatusV1::Success,
        accounts: bounded(post),
        executed_units: 123,
        loaded_accounts_data_size: 456,
        transaction_fee: 0,
        prioritization_fee: 0,
        return_data: Some(CandidateReturnDataV1 {
            program_id: REFERENCE_VERIFY_ID,
            data: bounded(vec![64; 64]),
        }),
        log_commitment: [4; 32],
    };
    let checkpoint = checkpoint(&input, &output);
    (
        CandidateEnvelopeV1 {
            magic: CANDIDATE_MAGIC,
            version: CANDIDATE_VERSION,
            proof_kind: CANDIDATE_PROOF_KIND,
            proof_version: 1,
            input,
            expected_output: output.clone(),
            checkpoint,
        },
        policy,
        BindingOnlyEngine { output, calls: 0 },
    )
}
fn values(accounts: &[CandidateAccountInputV1]) -> Vec<StateAccountValueV1> {
    accounts
        .iter()
        .filter_map(|account| {
            account
                .state
                .as_ref()
                .map(|state| candidate_state_value(account.key, state))
        })
        .collect()
}
fn checkpoint(
    input: &CandidateExecutionInputV1,
    output: &CandidateExecutionOutputV1,
) -> CandidateCheckpointV1 {
    let path = CandidateMerklePathV1 {
        leaf_index: 0,
        leaf_count: 1,
        siblings: BoundedVec::default(),
    };
    CandidateCheckpointV1 {
        pre_state: bounded(values(&input.accounts)),
        post_state: bounded(values(&output.accounts)),
        transaction_effect_root: candidate_transaction_root(
            candidate_transaction_effect_leaf(input, output).unwrap(),
            &path,
        )
        .unwrap(),
        transaction_effect_path: path.clone(),
        readonly_context_root: candidate_readonly_root(input).unwrap(),
        readonly_context_path: path,
        settlement_effect_root: empty_candidate_settlement_root(),
    }
}
fn changed_account(
    input: &mut CandidateExecutionInputV1,
    key: Hash32,
    change: impl FnOnce(&mut CandidateAccountInputV1),
) {
    let mut accounts = input.accounts.clone().into_vec();
    change(
        accounts
            .iter_mut()
            .find(|account| account.key == key)
            .unwrap(),
    );
    input.accounts = bounded(accounts);
}

#[test]
fn binding_only_success_creation_close_and_domain_separation() {
    for fulfill in [false, true] {
        let (envelope, policy, mut engine) = fixture(fulfill);
        let public = verify_candidate(&envelope, &policy, &mut engine).unwrap();
        assert_eq!(engine.calls, 1);
        assert_eq!(
            public.session_context,
            northstar_transaction_proof::commitment::fr_to_bytes(
                northstar_transaction_proof::commitment::bytes(
                    northstar_transaction_proof::commitment::SESSION_CONTEXT_TAG,
                    &envelope.input.session_context,
                )
                .unwrap()
            )
        );
        assert_eq!(
            public.domain,
            northstar_zk_types::FrBytes::er_step_domain_v1(3, 1)
        );
        assert!(FullTransactionPublicInputsV1::try_from(public).is_err());
        assert_eq!(
            public.tx_effect_root.to_bytes(),
            envelope.checkpoint.transaction_effect_root
        );
        let public_bytes = public_inputs_bytes(public);
        assert_eq!(public_bytes.len(), 256);
        assert_eq!(
            public_bytes[160..192],
            envelope.checkpoint.transaction_effect_root
        );
        assert_eq!(
            public_bytes[192..224],
            envelope.checkpoint.readonly_context_root
        );
        assert_eq!(
            public_bytes[224..256],
            envelope.checkpoint.settlement_effect_root
        );
        let encoded = encode_candidate(&envelope).unwrap();
        assert_eq!(decode_candidate(&encoded).unwrap(), envelope);
    }
}

#[test]
fn binding_only_checkpoint_builder_preserves_unobserved_state_and_rejects_readonly_changes() {
    let (mut envelope, policy, mut engine) = fixture(false);
    let mut pre = envelope.checkpoint.pre_state.clone().into_vec();
    let extra = candidate_state_value([250; 32], &state([0; 32], vec![7]));
    pre.push(extra);
    pre.sort_by_key(|value| value.account);
    envelope.checkpoint = build_candidate_checkpoint(
        &envelope.input,
        &engine.output,
        bounded(pre),
        envelope.checkpoint.transaction_effect_path.clone(),
        envelope.checkpoint.readonly_context_path.clone(),
    )
    .unwrap();
    assert!(envelope.checkpoint.post_state.contains(&extra));
    verify_candidate(&envelope, &policy, &mut engine).unwrap();

    let mut post = engine.output.accounts.clone().into_vec();
    let clock = post.iter_mut().find(|entry| entry.key == CLOCK_ID).unwrap();
    clock.state.as_mut().unwrap().lamports = 999;
    engine.output.accounts = bounded(post);
    assert_eq!(
        build_candidate_checkpoint(
            &envelope.input,
            &engine.output,
            envelope.checkpoint.pre_state.clone(),
            envelope.checkpoint.transaction_effect_path.clone(),
            envelope.checkpoint.readonly_context_path.clone()
        ),
        Err(CandidateError::Accounts)
    );
}

#[test]
fn binding_only_multiple_contexts_share_one_authenticated_checkpoint_root() {
    let (mut envelope, mut policy, mut engine) = fixture(false);
    envelope.input.profile.checkpoint_steps = 2;
    policy.profile_sha256 = candidate_profile_sha256(&envelope.input.profile).unwrap();
    let mut other = envelope.input.clone();
    other.step_index = 1;
    // This sibling is a separate committed claim, not an executed second step.
    let transaction_path = CandidateMerklePathV1 {
        leaf_index: 0,
        leaf_count: 2,
        siblings: bounded(vec![candidate_transaction_effect_leaf(
            &other,
            &engine.output,
        )
        .unwrap()]),
    };
    let context_path = CandidateMerklePathV1 {
        leaf_index: 0,
        leaf_count: 2,
        siblings: bounded(vec![candidate_readonly_context_leaf(&other).unwrap()]),
    };
    envelope.checkpoint = build_candidate_checkpoint(
        &envelope.input,
        &engine.output,
        envelope.checkpoint.pre_state.clone(),
        transaction_path,
        context_path,
    )
    .unwrap();
    let first = verify_candidate(&envelope, &policy, &mut engine).unwrap();
    let other_checkpoint = build_candidate_checkpoint(
        &other,
        &engine.output,
        bounded(values(&other.accounts)),
        CandidateMerklePathV1 {
            leaf_index: 1,
            leaf_count: 2,
            siblings: bounded(vec![candidate_transaction_effect_leaf(
                &envelope.input,
                &engine.output,
            )
            .unwrap()]),
        },
        CandidateMerklePathV1 {
            leaf_index: 1,
            leaf_count: 2,
            siblings: bounded(vec![
                candidate_readonly_context_leaf(&envelope.input).unwrap()
            ]),
        },
    )
    .unwrap();
    let mut second_envelope = envelope.clone();
    second_envelope.input = other;
    second_envelope.checkpoint = other_checkpoint;
    let second = verify_candidate(&second_envelope, &policy, &mut engine).unwrap();
    assert_eq!(first.tx_effect_root, second.tx_effect_root);
    assert_eq!(first.readonly_l1_root, second.readonly_l1_root);
    let mut siblings = envelope
        .checkpoint
        .readonly_context_path
        .siblings
        .clone()
        .into_vec();
    siblings[0][1] ^= 1;
    envelope.checkpoint.readonly_context_path.siblings = bounded(siblings);
    assert_eq!(
        verify_candidate(&envelope, &policy, &mut engine),
        Err(CandidateError::Checkpoint)
    );
}

#[test]
fn invariant_bounded_decoder_rejects_lengths_aliases_and_trailing_data() {
    assert!(BoundedVec::<u8, 8>::try_from_slice(&u32::MAX.to_le_bytes()).is_err());
    let (envelope, _, _) = fixture(false);
    let encoded = encode_candidate(&envelope).unwrap();
    for bytes in [
        encoded[..encoded.len().checked_sub(1).unwrap()].to_vec(),
        [encoded.clone(), vec![0]].concat(),
    ] {
        assert!(decode_candidate(&bytes).is_err());
    }
    assert_eq!(
        decode_candidate(&vec![0; MAX_ENVELOPE_BYTES + 1]),
        Err(CandidateError::ResourceLimit)
    );
}

#[test]
fn invariant_profile_feature_fee_compute_and_runtime_mutations_reject_before_engine() {
    let changes: &[fn(&mut CandidateEnvelopeV1)] = &[
        |e| e.input.profile.version = 2,
        |e| e.input.profile.vm_config_sha256[0] ^= 1,
        |e| e.input.profile.syscall_registry_sha256[0] ^= 1,
        |e| e.input.profile.compute_budget_sha256[0] ^= 1,
        |e| e.input.profile.checkpoint_steps = 2,
        |e| {
            let mut f = e.input.profile.active_features.clone().into_vec();
            f[0].activation_slot = 1;
            e.input.profile.active_features = bounded(f);
        },
        |e| e.input.profile.lamports_per_signature = 1,
        |e| e.input.runtime.compute_unit_limit = 499_999,
        |e| e.input.runtime.heap_bytes = 65_536,
        |e| e.input.runtime.slot = 21,
        |e| e.input.runtime.observation_slot = 21,
        |e| {
            let mut v = e.input.runtime.recent_blockhashes.clone().into_vec();
            v[0].age = 1801;
            e.input.runtime.recent_blockhashes = bounded(v);
        },
    ];
    for change in changes {
        let (mut envelope, policy, mut engine) = fixture(false);
        change(&mut envelope);
        assert!(verify_candidate(&envelope, &policy, &mut engine).is_err());
        assert_eq!(engine.calls, 0);
    }
}

#[test]
fn invariant_signature_wire_and_domain_reject_before_engine() {
    for signature in [true, false] {
        let (mut envelope, policy, mut engine) = fixture(false);
        let mut bytes = envelope.input.transaction_bytes.clone().into_vec();
        if signature {
            bytes[1] ^= 1;
        } else {
            *bytes.last_mut().unwrap() ^= 1;
        }
        envelope.input.transaction_bytes = bounded(bytes);
        assert!(verify_candidate(&envelope, &policy, &mut engine).is_err());
        assert_eq!(engine.calls, 0);
    }
    for field in 0..4 {
        let (mut envelope, policy, mut engine) = fixture(false);
        match field {
            0 => envelope.proof_kind = 2,
            1 => envelope.proof_version = 2,
            2 => envelope.version = 2,
            _ => envelope.magic[0] ^= 1,
        }
        assert_eq!(
            verify_candidate(&envelope, &policy, &mut engine),
            Err(CandidateError::Domain)
        );
        assert_eq!(engine.calls, 0);
    }
}

fn wire_from_admitted(transaction: &CandidateSignedTransactionV1) -> Vec<u8> {
    let mut message = transaction.header.to_vec();
    message.push(u8::try_from(transaction.account_keys.len()).unwrap());
    for key in &transaction.account_keys {
        message.extend_from_slice(key);
    }
    message.extend_from_slice(&transaction.recent_blockhash);
    message.push(u8::try_from(transaction.instructions.len()).unwrap());
    for instruction in &transaction.instructions {
        message.push(instruction.program_id_index);
        message.push(u8::try_from(instruction.accounts.len()).unwrap());
        message.extend_from_slice(&instruction.accounts);
        message.push(u8::try_from(instruction.data.len()).unwrap());
        message.extend_from_slice(&instruction.data);
    }
    sign(&message)
}

#[test]
fn invariant_signed_callback_order_privileges_and_proof_bytes_are_bound() {
    for kind in 0..3 {
        let (mut envelope, policy, mut engine) = fixture(true);
        let mut transaction = admit_signed_transaction(&envelope.input.transaction_bytes).unwrap();
        match kind {
            0 => transaction.instructions[1].accounts.swap(3, 7),
            1 => transaction.header[2] = 5,
            _ => *transaction.instructions[1].data.last_mut().unwrap() ^= 1,
        }
        envelope.input.transaction_bytes = bounded(wire_from_admitted(&transaction));
        assert!(verify_candidate(&envelope, &policy, &mut engine).is_err());
    }
}

#[test]
fn invariant_sysvar_owner_is_not_just_a_data_hash() {
    let (mut envelope, policy, mut engine) = fixture(false);
    changed_account(&mut envelope.input, CLOCK_ID, |entry| {
        entry.state.as_mut().unwrap().owner = [99; 32]
    });
    assert_eq!(
        verify_candidate(&envelope, &policy, &mut engine),
        Err(CandidateError::Runtime)
    );
    assert_eq!(engine.calls, 0);
}

#[test]
fn invariant_failure_cannot_retain_tentative_writes_even_with_matching_claim() {
    let (mut envelope, policy, mut engine) = fixture(true);
    engine.output.status = CandidateExecutionStatusV1::InstructionFailure {
        instruction_index: 1,
        error_kind: 1,
        custom_error: 6001,
    };
    envelope.expected_output = engine.output.clone();
    envelope.checkpoint = checkpoint(&envelope.input, &envelope.expected_output);
    assert_eq!(
        verify_candidate(&envelope, &policy, &mut engine),
        Err(CandidateError::Outcome)
    );
}

#[test]
fn invariant_loader_program_elf_and_programdata_binding_rejects() {
    for kind in 0..4 {
        let (mut envelope, policy, mut engine) = fixture(false);
        let program = envelope.input.programs[0].clone();
        let key = if kind == 0 {
            program.program_id
        } else {
            program.programdata_id
        };
        changed_account(&mut envelope.input, key, |entry| {
            let state = entry.state.as_mut().unwrap();
            if kind == 1 {
                state.owner = [99; 32];
            } else {
                let mut data = state.data.clone().into_vec();
                let index = if kind == 0 {
                    4
                } else if kind == 2 {
                    0
                } else {
                    45
                };
                data[index] ^= 1;
                state.data = bounded(data);
            }
        });
        assert_eq!(
            verify_candidate(&envelope, &policy, &mut engine),
            Err(CandidateError::Program)
        );
        assert_eq!(engine.calls, 0);
    }
}

#[test]
fn invariant_clock_slot_hashes_readonly_and_absence_are_authenticated() {
    for key in [CLOCK_ID, SLOT_HASHES_ID, [44; 32]] {
        let (mut envelope, policy, mut engine) = fixture(false);
        changed_account(&mut envelope.input, key, |entry| {
            let state = entry.state.as_mut().unwrap();
            let mut data = state.data.clone().into_vec();
            if data.is_empty() {
                data.push(1);
            } else {
                *data.last_mut().unwrap() ^= 1;
            }
            state.data = bounded(data);
        });
        assert!(verify_candidate(&envelope, &policy, &mut engine).is_err());
        assert_eq!(engine.calls, 0);
    }
    let (mut envelope, policy, mut engine) = fixture(false);
    changed_account(&mut envelope.input, [42; 32], |entry| {
        entry.state = Some(state(COORDINATOR_ID, vec![0]))
    });
    assert_eq!(
        verify_candidate(&envelope, &policy, &mut engine),
        Err(CandidateError::Unsupported(
            CandidateUnsupported::AccountLifecycle
        ))
    );
    assert_eq!(engine.calls, 0);

    let (mut envelope, policy, mut engine) = fixture(false);
    let mut values = envelope.checkpoint.pre_state.clone().into_vec();
    values.push(candidate_state_value(
        [42; 32],
        &state(COORDINATOR_ID, vec![0]),
    ));
    values.sort_by_key(|value| value.account);
    envelope.checkpoint.pre_state = bounded(values);
    assert_eq!(
        verify_candidate(&envelope, &policy, &mut engine),
        Err(CandidateError::Checkpoint)
    );
    assert_eq!(engine.calls, 0);
}

#[test]
fn invariant_account_aliases_and_order_are_rejected() {
    for duplicate in [true, false] {
        let (mut envelope, policy, mut engine) = fixture(false);
        let mut accounts = envelope.input.accounts.clone().into_vec();
        if duplicate {
            accounts[1].key = accounts[0].key;
        } else {
            accounts.swap(0, 1);
        }
        envelope.input.accounts = bounded(accounts);
        assert_eq!(
            verify_candidate(&envelope, &policy, &mut engine),
            Err(CandidateError::Accounts)
        );
        assert_eq!(engine.calls, 0);
    }
}

#[test]
fn invariant_unverified_output_return_producer_effects_and_rollback_never_replace_engine() {
    let changes: &[fn(&mut CandidateExecutionOutputV1)] = &[
        |o| o.executed_units = 124,
        |o| o.transaction_fee = 1,
        |o| o.loaded_accounts_data_size = 457,
        |o| o.return_data.as_mut().unwrap().program_id = COORDINATOR_ID,
        |o| o.return_data.as_mut().unwrap().data = bounded(vec![63; 64]),
        |o| o.log_commitment[0] ^= 1,
        |o| {
            o.status = CandidateExecutionStatusV1::InstructionFailure {
                instruction_index: 1,
                error_kind: 1,
                custom_error: 6003,
            }
        },
        |o| {
            let mut a = o.accounts.clone().into_vec();
            a[0].state = None;
            o.accounts = bounded(a);
        },
    ];
    for change in changes {
        let (mut envelope, policy, mut engine) = fixture(true);
        change(&mut envelope.expected_output);
        // Even a newly hashed claim cannot replace independently recomputed output.
        envelope.checkpoint = checkpoint(&envelope.input, &envelope.expected_output);
        assert_eq!(
            verify_candidate(&envelope, &policy, &mut engine),
            Err(CandidateError::Outcome)
        );
        assert_eq!(engine.calls, 1);
    }
}

#[test]
fn binding_only_failure_preserves_all_accounts_and_unsupported_is_distinct() {
    let (mut envelope, policy, mut engine) = fixture(true);
    let mut failure = engine.output.clone();
    failure.status = CandidateExecutionStatusV1::InstructionFailure {
        instruction_index: 1,
        error_kind: 1,
        custom_error: 6003,
    };
    failure.accounts = envelope.input.accounts.clone();
    envelope.expected_output = failure.clone();
    engine.output = failure;
    envelope.checkpoint = checkpoint(&envelope.input, &envelope.expected_output);
    verify_candidate(&envelope, &policy, &mut engine).unwrap();
    struct Unsupported;
    impl CandidateReplayEngine for Unsupported {
        fn execute(
            &mut self,
            _: &CandidateExecutionInputV1,
        ) -> Result<CandidateExecutionOutputV1, CandidateError> {
            Err(CandidateError::Unsupported(CandidateUnsupported::Engine))
        }
    }
    assert_eq!(
        verify_candidate(&envelope, &policy, &mut Unsupported),
        Err(CandidateError::Unsupported(CandidateUnsupported::Engine))
    );
}

#[test]
fn invariant_checkpoint_inclusion_poststate_and_settlement_reject() {
    for kind in 0..4 {
        let (mut envelope, policy, mut engine) = fixture(false);
        match kind {
            0 => envelope.checkpoint.transaction_effect_root[1] ^= 1,
            1 => envelope.checkpoint.transaction_effect_path.leaf_index = 1,
            2 => {
                let mut values = envelope.checkpoint.post_state.clone().into_vec();
                values[0].lamports = 7;
                envelope.checkpoint.post_state = bounded(values);
            }
            _ => envelope.checkpoint.settlement_effect_root[1] ^= 1,
        }
        assert!(verify_candidate(&envelope, &policy, &mut engine).is_err());
    }
}

#[test]
#[ignore = "requires retained real signed ER capture paths, not execution-witness completeness"]
fn actual_captured_wire_passes_candidate_shape_only() {
    for variable in [
        "NORTHSTAR_VRF_REQUEST_TRANSACTION",
        "NORTHSTAR_VRF_FULFILL_TRANSACTION",
    ] {
        let bytes = std::fs::read(std::env::var_os(variable).expect("capture path")).unwrap();
        let transaction = admit_signed_transaction(&bytes).unwrap();
        assert_eq!(transaction.instructions.len(), 2);
        assert_eq!(transaction.account_keys.len(), 10);
    }
}
