//! Host-only transformations of canonical captured witnesses. No execution/proving.

#[cfg(all(feature = "host", not(target_os = "zkvm")))]
mod host {
    use {
        northstar_transaction_proof::{
            public_inputs_bytes, readiness::hex, vrf_candidate::*,
            vrf_coordinator_profile::inspect_signed_transaction,
        },
        sha2::{Digest, Sha256},
        std::{
            env, fs,
            io::{Read, Write},
            path::Path,
        },
    };

    type Result<T> = std::result::Result<T, String>;
    const POLICY: &str = "c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d";
    const LENGTH_OFFSET: usize = 12;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Basis {
        Request,
        Fulfill,
    }
    impl Basis {
        fn name(self) -> &'static str {
            match self {
                Self::Request => "request",
                Self::Fulfill => "fulfill-zero",
            }
        }
        fn file(self) -> &'static str {
            match self {
                Self::Request => "baselines/request.borsh",
                Self::Fulfill => "baselines/fulfill-zero.borsh",
            }
        }
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Mutation {
        Signature,
        InputData,
        RequestPresence,
        ProgramElf,
        RuntimeIdentity,
        FeatureActivation,
        FeePolicy,
        BlockhashAge,
        StepIndex,
        ContextCount,
        PostBalance,
        ReturnProducer,
        TrailingByte,
        BoundedLength,
        UnsupportedSetup,
    }
    #[derive(Clone, Copy)]
    struct Spec {
        id: &'static str,
        basis: Basis,
        mutation: Mutation,
        category: &'static str,
        expected_guard: &'static str,
        invariant: &'static str,
    }
    const SPECS: [Spec; 15] = [
        Spec {
            id: "request-signature",
            basis: Basis::Request,
            mutation: Mutation::Signature,
            category: "incorrect_witness",
            expected_guard: "Signature",
            invariant: "The signature must authenticate the unchanged signed message.",
        },
        Spec {
            id: "request-account-data",
            basis: Basis::Request,
            mutation: Mutation::InputData,
            category: "incorrect_witness",
            expected_guard: "Checkpoint",
            invariant: "Bank account bytes must match authenticated checkpoint prestate.",
        },
        Spec {
            id: "request-initial-presence",
            basis: Basis::Request,
            mutation: Mutation::RequestPresence,
            category: "unsupported_lifecycle",
            expected_guard: "Unsupported(AccountLifecycle)",
            invariant: "The bounded creation profile requires initial request absence.",
        },
        Spec {
            id: "request-program-elf",
            basis: Basis::Request,
            mutation: Mutation::ProgramElf,
            category: "incorrect_witness",
            expected_guard: "Program",
            invariant: "Authenticated ProgramData ELF bytes must match the fixed program identity.",
        },
        Spec {
            id: "request-runtime-identity",
            basis: Basis::Request,
            mutation: Mutation::RuntimeIdentity,
            category: "incorrect_witness",
            expected_guard: "Profile",
            invariant: "The runtime identity must match the guest-owned fixed policy digest.",
        },
        Spec {
            id: "request-feature-activation",
            basis: Basis::Request,
            mutation: Mutation::FeatureActivation,
            category: "incorrect_witness",
            expected_guard: "Profile",
            invariant: "Feature activation metadata must match the fixed policy digest.",
        },
        Spec {
            id: "request-fixed-fee",
            basis: Basis::Request,
            mutation: Mutation::FeePolicy,
            category: "incorrect_witness",
            expected_guard: "Profile",
            invariant: "The fixed profile declares zero transaction fees; this is a profile \
                        mismatch, not a fee execution measurement.",
        },
        Spec {
            id: "request-blockhash-age",
            basis: Basis::Request,
            mutation: Mutation::BlockhashAge,
            category: "incorrect_witness",
            expected_guard: "Runtime",
            invariant: "The signed blockhash must be eligible under the effective transaction-age \
                        policy.",
        },
        Spec {
            id: "request-step-index",
            basis: Basis::Request,
            mutation: Mutation::StepIndex,
            category: "incorrect_witness",
            expected_guard: "Checkpoint",
            invariant: "The selected step index must fit the fixed one-step policy.",
        },
        Spec {
            id: "request-context-count",
            basis: Basis::Request,
            mutation: Mutation::ContextCount,
            category: "incorrect_witness",
            expected_guard: "Checkpoint",
            invariant: "Context inclusion count must match the fixed policy and transaction tree \
                        count.",
        },
        Spec {
            id: "request-expected-post-balance",
            basis: Basis::Request,
            mutation: Mutation::PostBalance,
            category: "incorrect_witness",
            expected_guard: "Outcome",
            invariant: "Every expected poststate balance must equal actual interpreter output.",
        },
        Spec {
            id: "fulfill-empty-return-producer",
            basis: Basis::Fulfill,
            mutation: Mutation::ReturnProducer,
            category: "incorrect_witness",
            expected_guard: "Outcome",
            invariant: "The final return-data producer must match even when the return bytes are \
                        empty.",
        },
        Spec {
            id: "request-trailing-byte",
            basis: Basis::Request,
            mutation: Mutation::TrailingByte,
            category: "malformed_encoding",
            expected_guard: "Encoding",
            invariant: "Canonical Borsh rejects trailing bytes.",
        },
        Spec {
            id: "request-bounded-length",
            basis: Basis::Request,
            mutation: Mutation::BoundedLength,
            category: "malformed_encoding",
            expected_guard: "Encoding",
            invariant: "The session-context vector length must be bounded before allocation.",
        },
        Spec {
            id: "actual-signed-setup",
            basis: Basis::Request,
            mutation: Mutation::UnsupportedSetup,
            category: "unsupported_transaction_shape",
            expected_guard: "Unsupported(TransactionShape)",
            invariant: "The authentic signed setup instruction is outside the admitted \
                        request/fulfillment profile.",
        },
    ];

    struct Source {
        bytes: Vec<u8>,
        envelope: CandidateEnvelopeV1,
        public: Vec<u8>,
    }
    struct Control {
        spec: Spec,
        bytes: Vec<u8>,
        changed_field: String,
        canonical: bool,
    }

    fn need(condition: bool, message: &str) -> Result<()> {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }
    fn candidate<T>(value: std::result::Result<T, CandidateError>) -> Result<T> {
        value.map_err(|error| format!("{error:?}"))
    }
    fn hash(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }
    fn read(path: &Path, max: usize) -> Result<Vec<u8>> {
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        need(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "regular file required",
        )?;
        need(
            metadata.len() <= max as u64,
            "input file exceeds local resource bound",
        )?;
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(max.checked_add(1).ok_or("size overflow")? as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        need(bytes.len() <= max, "input grew beyond local resource bound")?;
        Ok(bytes)
    }
    fn load_bundle(path: &Path, basis: Basis) -> Result<Source> {
        let bytes = read(&path.join("witness.borsh"), MAX_ENVELOPE_BYTES)?;
        let public = read(&path.join("public-inputs.bin"), 256)?;
        let envelope = candidate(decode_candidate(&bytes))?;
        need(
            envelope.magic == CANDIDATE_MAGIC
                && envelope.version == CANDIDATE_VERSION
                && envelope.proof_kind == 3
                && envelope.proof_version == 1,
            "frozen v3 envelope required",
        )?;
        need(
            hex(&candidate(candidate_profile_sha256(
                &envelope.input.profile,
            ))?) == POLICY,
            "fixed c627 policy required",
        )?;
        need(
            envelope.input.profile.checkpoint_steps == 1
                && envelope.input.profile.max_processing_age == 900,
            "fixed count/age required",
        )?;
        need(
            envelope.expected_output.status == CandidateExecutionStatusV1::Success,
            "positive source envelope must claim success",
        )?;
        let transaction = candidate(admit_signed_transaction(&envelope.input.transaction_bytes))?;
        let program =
            transaction.account_keys[usize::from(transaction.instructions[1].program_id_index)];
        need(
            program
                == match basis {
                    Basis::Request => TEST_CONSUMER_ID,
                    Basis::Fulfill => COORDINATOR_ID,
                },
            "source role mismatch",
        )?;
        // Consistency only: no engine is invoked and no execution validity is inferred.
        let bound = candidate(bind_candidate_execution(
            &envelope.input,
            &envelope.expected_output,
            &envelope.checkpoint,
        ))?;
        need(
            public == public_inputs_bytes(bound),
            "source public bytes do not match claimed canonical bindings",
        )?;
        if basis == Basis::Fulfill {
            need(
                envelope
                    .expected_output
                    .return_data
                    .as_ref()
                    .is_some_and(|value| value.data.is_empty()),
                "fulfillment source must preserve an actual empty-data producer",
            )?;
        }
        Ok(Source {
            bytes,
            envelope,
            public,
        })
    }
    fn replace_accounts(
        input: &mut CandidateExecutionInputV1,
        values: Vec<CandidateAccountInputV1>,
    ) -> Result<()> {
        input.accounts = candidate(BoundedVec::new(values))?;
        Ok(())
    }
    fn account_index(input: &CandidateExecutionInputV1, key: Hash32) -> Result<usize> {
        input
            .accounts
            .binary_search_by_key(&key, |value| value.key)
            .map_err(|_| "required source account absent".into())
    }
    fn flip_data(input: &mut CandidateExecutionInputV1, index: usize, offset: usize) -> Result<()> {
        let mut accounts = input.accounts.clone().into_vec();
        let state = accounts
            .get_mut(index)
            .and_then(|a| a.state.as_mut())
            .ok_or("required source state absent")?;
        let mut bytes = state.data.clone().into_vec();
        *bytes.get_mut(offset).ok_or("source data byte absent")? ^= 1;
        state.data = candidate(BoundedVec::new(bytes))?;
        replace_accounts(input, accounts)
    }
    fn construct(spec: Spec, source: &Source, setup: &[u8]) -> Result<Control> {
        let mut changed = source.envelope.clone();
        let transaction = candidate(admit_signed_transaction(&changed.input.transaction_bytes))?;
        let app = &transaction.instructions[1];
        let mut restored;
        let field;
        match spec.mutation {
            Mutation::TrailingByte => {
                let mut bytes = source.bytes.clone();
                bytes.push(0);
                need(
                    decode_candidate(&bytes).is_err(),
                    "trailing bytes unexpectedly decoded",
                )?;
                return Ok(Control {
                    spec,
                    bytes,
                    changed_field: "encoding.trailing_byte".into(),
                    canonical: false,
                });
            }
            Mutation::BoundedLength => {
                let end = LENGTH_OFFSET.checked_add(4).ok_or("offset overflow")?;
                need(
                    source.bytes.get(LENGTH_OFFSET..end) == Some(149u32.to_le_bytes().as_slice()),
                    "frozen session-vector offset mismatch",
                )?;
                let mut bytes = source.bytes.clone();
                bytes[LENGTH_OFFSET..end].copy_from_slice(&u32::MAX.to_le_bytes());
                need(
                    decode_candidate(&bytes).is_err(),
                    "oversized vector unexpectedly decoded",
                )?;
                let mut restored = bytes.clone();
                restored[LENGTH_OFFSET..end].copy_from_slice(&source.bytes[LENGTH_OFFSET..end]);
                need(
                    restored == source.bytes,
                    "length mutation changed other bytes",
                )?;
                return Ok(Control {
                    spec,
                    bytes,
                    changed_field: "encoding.input.session_context.length".into(),
                    canonical: false,
                });
            }
            Mutation::Signature => {
                let mut bytes = changed.input.transaction_bytes.clone().into_vec();
                *bytes.get_mut(1).ok_or("signature byte absent")? ^= 1;
                changed.input.transaction_bytes = candidate(BoundedVec::new(bytes))?;
                field = "input.transaction_bytes.signature[0]".into();
                restored = changed.clone();
                let mut restored_wire = restored.input.transaction_bytes.clone().into_vec();
                restored_wire[1] ^= 1;
                restored.input.transaction_bytes = candidate(BoundedVec::new(restored_wire))?;
            }
            Mutation::InputData => {
                let key = transaction.account_keys[usize::from(app.accounts[1])];
                let index = account_index(&changed.input, key)?;
                flip_data(&mut changed.input, index, 0)?;
                field = format!("input.accounts[{index}].state.data[0]");
                restored = changed.clone();
                flip_data(&mut restored.input, index, 0)?;
            }
            Mutation::RequestPresence => {
                let key = transaction.account_keys[usize::from(app.accounts[5])];
                let index = account_index(&changed.input, key)?;
                need(
                    changed.input.accounts[index].state.is_none(),
                    "request source must start absent",
                )?;
                let actual_post = source
                    .envelope
                    .expected_output
                    .accounts
                    .iter()
                    .find(|a| a.key == key)
                    .and_then(|a| a.state.clone())
                    .ok_or("actual created request state absent")?;
                let mut accounts = changed.input.accounts.clone().into_vec();
                accounts[index].state = Some(actual_post);
                replace_accounts(&mut changed.input, accounts)?;
                field = format!("input.accounts[{index}].state");
                restored = changed.clone();
                let mut values = restored.input.accounts.clone().into_vec();
                values[index].state = None;
                replace_accounts(&mut restored.input, values)?;
            }
            Mutation::ProgramElf => {
                let index =
                    account_index(&changed.input, changed.input.programs[0].programdata_id)?;
                flip_data(
                    &mut changed.input,
                    index,
                    UPGRADEABLE_PROGRAMDATA_METADATA_BYTES,
                )?;
                field = format!("input.accounts[{index}].state.data[45]");
                restored = changed.clone();
                flip_data(
                    &mut restored.input,
                    index,
                    UPGRADEABLE_PROGRAMDATA_METADATA_BYTES,
                )?;
            }
            Mutation::RuntimeIdentity => {
                changed.input.profile.vm_config_sha256[0] ^= 1;
                field = "input.profile.vm_config_sha256[0]".into();
                restored = changed.clone();
                restored.input.profile.vm_config_sha256[0] ^= 1;
            }
            Mutation::FeatureActivation => {
                let mut values = changed.input.profile.active_features.clone().into_vec();
                let feature = values.first_mut().ok_or("active feature absent")?;
                feature.activation_slot = feature
                    .activation_slot
                    .checked_add(1)
                    .ok_or("activation overflow")?;
                changed.input.profile.active_features = candidate(BoundedVec::new(values))?;
                field = "input.profile.active_features[0].activation_slot".into();
                restored = changed.clone();
                let mut values = restored.input.profile.active_features.clone().into_vec();
                values[0].activation_slot =
                    source.envelope.input.profile.active_features[0].activation_slot;
                restored.input.profile.active_features = candidate(BoundedVec::new(values))?;
            }
            Mutation::FeePolicy => {
                need(
                    changed.input.profile.lamports_per_signature == 0,
                    "expected zero source fee",
                )?;
                changed.input.profile.lamports_per_signature = 1;
                field = "input.profile.lamports_per_signature".into();
                restored = changed.clone();
                restored.input.profile.lamports_per_signature = 0;
            }
            Mutation::BlockhashAge => {
                let mut values = changed.input.runtime.recent_blockhashes.clone().into_vec();
                let index = values
                    .iter()
                    .position(|entry| entry.blockhash == transaction.recent_blockhash)
                    .ok_or("signed blockhash entry absent")?;
                values[index].age = changed
                    .input
                    .profile
                    .max_processing_age
                    .checked_add(1)
                    .ok_or("age overflow")?;
                changed.input.runtime.recent_blockhashes = candidate(BoundedVec::new(values))?;
                field = format!("input.runtime.recent_blockhashes[{index}].age");
                restored = changed.clone();
                let mut values = restored.input.runtime.recent_blockhashes.clone().into_vec();
                values[index].age = source.envelope.input.runtime.recent_blockhashes[index].age;
                restored.input.runtime.recent_blockhashes = candidate(BoundedVec::new(values))?;
            }
            Mutation::StepIndex => {
                need(
                    changed.input.step_index == 0,
                    "expected single-step index zero",
                )?;
                changed.input.step_index = 1;
                field = "input.step_index".into();
                restored = changed.clone();
                restored.input.step_index = 0;
            }
            Mutation::ContextCount => {
                need(
                    changed.checkpoint.readonly_context_path.leaf_count == 1,
                    "expected one context leaf",
                )?;
                changed.checkpoint.readonly_context_path.leaf_count = 2;
                field = "checkpoint.readonly_context_path.leaf_count".into();
                restored = changed.clone();
                restored.checkpoint.readonly_context_path.leaf_count = 1;
            }
            Mutation::PostBalance => {
                let mut values = changed.expected_output.accounts.clone().into_vec();
                let index = values
                    .iter()
                    .position(|value| value.key == transaction.account_keys[0])
                    .ok_or("post fee payer absent")?;
                let state = values[index]
                    .state
                    .as_mut()
                    .ok_or("post fee payer state absent")?;
                state.lamports = state
                    .lamports
                    .checked_sub(1)
                    .filter(|v| *v != 0)
                    .ok_or("post fee payer needs positive balance")?;
                changed.expected_output.accounts = candidate(BoundedVec::new(values))?;
                field = format!("expected_output.accounts[{index}].state.lamports");
                restored = changed.clone();
                let mut values = restored.expected_output.accounts.clone().into_vec();
                values[index]
                    .state
                    .as_mut()
                    .ok_or("restored post payer absent")?
                    .lamports = source.envelope.expected_output.accounts[index]
                    .state
                    .as_ref()
                    .ok_or("source post payer absent")?
                    .lamports;
                restored.expected_output.accounts = candidate(BoundedVec::new(values))?;
            }
            Mutation::ReturnProducer => {
                let value = changed
                    .expected_output
                    .return_data
                    .as_mut()
                    .ok_or("source return producer absent")?;
                need(value.data.is_empty(), "empty return-data source required")?;
                value.program_id = if value.program_id != COORDINATOR_ID {
                    COORDINATOR_ID
                } else {
                    REFERENCE_VERIFY_ID
                };
                field = "expected_output.return_data.program_id".into();
                restored = changed.clone();
                restored
                    .expected_output
                    .return_data
                    .as_mut()
                    .unwrap()
                    .program_id = source
                    .envelope
                    .expected_output
                    .return_data
                    .as_ref()
                    .unwrap()
                    .program_id;
            }
            Mutation::UnsupportedSetup => {
                changed.input.transaction_bytes = candidate(BoundedVec::new(setup.to_vec()))?;
                field = "input.transaction_bytes".into();
                restored = changed.clone();
                restored.input.transaction_bytes = source.envelope.input.transaction_bytes.clone();
            }
        }
        need(changed != source.envelope, "control did not change source")?;
        need(restored == source.envelope, "control changed another field")?;
        if matches!(
            spec.mutation,
            Mutation::PostBalance | Mutation::ReturnProducer
        ) {
            // Keep the wrong claim internally consistent: a binding-only shortcut
            // must not reject these instead of independently reexecuting the input.
            changed.checkpoint = candidate(build_candidate_checkpoint(
                &changed.input,
                &changed.expected_output,
                source.envelope.checkpoint.pre_state.clone(),
                source.envelope.checkpoint.transaction_effect_path.clone(),
                source.envelope.checkpoint.readonly_context_path.clone(),
            ))?;
        }
        let bytes = candidate(encode_candidate(&changed))?;
        need(
            candidate(decode_candidate(&bytes))? == changed,
            "canonical control failed roundtrip",
        )?;
        Ok(Control {
            spec,
            bytes,
            changed_field: field,
            canonical: true,
        })
    }
    fn quote(value: &str) -> String {
        let mut out = String::from("\"");
        for ch in value.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
                ch => out.push(ch),
            }
        }
        out.push('"');
        out
    }
    fn object(fields: &[(&str, String)]) -> String {
        format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(key, value)| format!("{}:{value}", quote(key)))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
    fn json_array(values: &[String]) -> String {
        format!("[{}]", values.join(","))
    }
    fn create(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())
    }
    fn generate(request: &Source, fulfill: &Source, setup: &[u8], output: &Path) -> Result<String> {
        inspect_signed_transaction(setup)
            .map_err(|e| format!("unsupported setup signature/encoding: {e:?}"))?;
        need(
            admit_signed_transaction(setup)
                == Err(CandidateError::Unsupported(
                    CandidateUnsupported::TransactionShape,
                )),
            "setup wire does not reach the expected unsupported-shape boundary",
        )?;
        let controls = SPECS
            .iter()
            .map(|spec| {
                construct(
                    *spec,
                    match spec.basis {
                        Basis::Request => request,
                        Basis::Fulfill => fulfill,
                    },
                    setup,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        // Validate every transformation before creating a corpus directory.
        fs::create_dir(output).map_err(|e| e.to_string())?;
        let mut positives = Vec::new();
        for (basis, source) in [(Basis::Request, request), (Basis::Fulfill, fulfill)] {
            create(output, basis.file(), &source.bytes)?;
            let public_file = format!("baselines/{}.public-inputs.bin", basis.name());
            create(output, &public_file, &source.public)?;
            positives.push(object(&[
                ("id", quote(basis.name())),
                ("file", quote(basis.file())),
                ("sha256", quote(&hash(&source.bytes))),
                ("public_inputs_file", quote(&public_file)),
                ("public_inputs_sha256", quote(&hash(&source.public))),
                ("expected_guest_success", "true".into()),
            ]));
        }
        create(output, "sources/unsupported-setup.transaction.bin", setup)?;
        let mut rows = Vec::new();
        for control in controls {
            let file = format!("controls/{}.borsh", control.spec.id);
            create(output, &file, &control.bytes)?;
            rows.push(object(&[
                ("id", quote(control.spec.id)),
                ("file", quote(&file)),
                ("sha256", quote(&hash(&control.bytes))),
                ("bytes", control.bytes.len().to_string()),
                ("basis", quote(control.spec.basis.name())),
                (
                    "basis_sha256",
                    quote(&hash(match control.spec.basis {
                        Basis::Request => &request.bytes,
                        Basis::Fulfill => &fulfill.bytes,
                    })),
                ),
                ("category", quote(control.spec.category)),
                ("changed_field", quote(&control.changed_field)),
                ("canonical_borsh", control.canonical.to_string()),
                (
                    "derived_checkpoint_rebound",
                    matches!(
                        control.spec.mutation,
                        Mutation::PostBalance | Mutation::ReturnProducer
                    )
                    .to_string(),
                ),
                ("expected_guard", quote(control.spec.expected_guard)),
                ("missing_invariant", quote(control.spec.invariant)),
                ("expected_guest_exit", quote("nonzero")),
                ("expected_public_output_bytes", "0".into()),
            ]));
        }
        let snapshot = [
            (
                "northstar/transaction-proof/src/vrf_candidate.rs",
                include_bytes!("../vrf_candidate.rs").as_slice(),
            ),
            (
                "northstar/transaction-proof/src/vrf_candidate/binding.rs",
                include_bytes!("../vrf_candidate/binding.rs").as_slice(),
            ),
            (
                "northstar/transaction-proof/src/vrf_candidate/wire.rs",
                include_bytes!("../vrf_candidate/wire.rs").as_slice(),
            ),
            (
                "northstar/transaction-proof/src/vrf_candidate/ids.rs",
                include_bytes!("../vrf_candidate/ids.rs").as_slice(),
            ),
        ]
        .map(|(path, bytes)| (path, quote(&hash(bytes))));
        let domain = hex(&northstar_zk_types::FrBytes::er_step_domain_v1(3, 1).to_bytes());
        let skipped = object(&[
            ("id", quote("rollback-outcome-state")),
            (
                "reason",
                quote(
                    "No independently native-verified canonical failed-transaction witness was \
                     supplied; a valid application-rejection witness is not an incorrect witness.",
                ),
            ),
        ]);
        let mut manifest = object(&[
            ("schema", quote("northstar-vrf-candidate-controls-v1")),
            ("construction_only", "true".into()),
            ("engine_replayed", "false".into()),
            ("guest_execution_observed", "false".into()),
            ("proof_generated", "false".into()),
            ("proof_kind", "3".into()),
            ("proof_version", "1".into()),
            ("domain_hex", quote(&domain)),
            ("policy_sha256", quote(POLICY)),
            ("checkpoint_steps", "1".into()),
            ("transaction_max_age", "900".into()),
            (
                "generator_source_sha256",
                quote(&hash(include_bytes!("vrf-candidate-controls.rs"))),
            ),
            ("binding_sources_sha256", object(&snapshot)),
            ("positive_cases", json_array(&positives)),
            ("controls", json_array(&rows)),
            (
                "unsupported_signed_transaction",
                object(&[
                    ("file", quote("sources/unsupported-setup.transaction.bin")),
                    ("sha256", quote(&hash(setup))),
                    ("message_signatures_checked", "true".into()),
                ]),
            ),
            ("skipped_controls", json_array(&[skipped])),
            (
                "acceptance_rule",
                quote(
                    "Run both positive baselines first. Count only actual guest nonzero exit plus \
                     empty public output; host errors or labels are not guest rejection. Record \
                     exact tested ELF/key separately.",
                ),
            ),
        ]);
        manifest.push('\n');
        create(output, "manifest.json", manifest.as_bytes())?;
        Ok(manifest)
    }
    pub fn run() -> Result<()> {
        let args = env::args_os().skip(1).collect::<Vec<_>>();
        need(
            args.len() == 4,
            "usage: vrf-candidate-controls REQUEST_BUNDLE FULFILL_ZERO_BUNDLE \
             AUTHENTIC_SETUP_TRANSACTION FRESH_OUTPUT_DIR",
        )?;
        let request = load_bundle(Path::new(&args[0]), Basis::Request)?;
        let fulfill = load_bundle(Path::new(&args[1]), Basis::Fulfill)?;
        let setup = read(Path::new(&args[2]), MAX_TRANSACTION_BYTES)?;
        let manifest = generate(&request, &fulfill, &setup, Path::new(&args[3]))?;
        println!(
            "controls=15 positives=2 manifest_sha256={} construction_only=true \
             proof_generated=false",
            hash(manifest.as_bytes())
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn json_strings_escape_control_characters() {
            assert_eq!(quote("a\"\\\n\t"), "\"a\\\"\\\\\\n\\t\"");
        }
        #[test]
        fn control_ids_and_paths_are_unique_and_relative() {
            let mut ids = std::collections::BTreeSet::new();
            for spec in SPECS {
                assert!(ids.insert(spec.id));
                assert!(spec.id.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'));
                assert!(!Path::new(spec.id).is_absolute());
            }
        }
        #[test]
        #[ignore = "requires real canonical source bundles and authentic signed setup wire"]
        fn actual_sources_have_one_field_controls_and_deterministic_roundtrips() {
            let request = load_bundle(
                Path::new(
                    &env::var_os("NORTHSTAR_CONTROL_REQUEST_BUNDLE").expect("request bundle"),
                ),
                Basis::Request,
            )
            .unwrap();
            let fulfill = load_bundle(
                Path::new(
                    &env::var_os("NORTHSTAR_CONTROL_FULFILL_BUNDLE").expect("fulfill bundle"),
                ),
                Basis::Fulfill,
            )
            .unwrap();
            let setup = read(
                Path::new(&env::var_os("NORTHSTAR_CONTROL_SETUP_WIRE").expect("setup wire")),
                MAX_TRANSACTION_BYTES,
            )
            .unwrap();
            for spec in SPECS {
                let source = match spec.basis {
                    Basis::Request => &request,
                    Basis::Fulfill => &fulfill,
                };
                let first = construct(spec, source, &setup).unwrap();
                let repeated = construct(spec, source, &setup).unwrap();
                assert_eq!(first.bytes, repeated.bytes);
                assert_ne!(first.bytes, source.bytes);
                if first.canonical {
                    assert_eq!(
                        candidate(encode_candidate(
                            &candidate(decode_candidate(&first.bytes)).unwrap()
                        ))
                        .unwrap(),
                        first.bytes
                    );
                } else {
                    assert!(decode_candidate(&first.bytes).is_err());
                }
                if matches!(
                    spec.mutation,
                    Mutation::PostBalance | Mutation::ReturnProducer
                ) {
                    let changed = decode_candidate(&first.bytes).unwrap();
                    assert!(bind_candidate_execution(
                        &changed.input,
                        &changed.expected_output,
                        &changed.checkpoint
                    )
                    .is_ok());
                    assert_ne!(
                        changed.checkpoint.transaction_effect_root,
                        source.envelope.checkpoint.transaction_effect_root
                    );
                }
                if spec.mutation == Mutation::ReturnProducer {
                    let changed = decode_candidate(&first.bytes).unwrap();
                    let before = source
                        .envelope
                        .expected_output
                        .return_data
                        .as_ref()
                        .unwrap();
                    let after = changed.expected_output.return_data.as_ref().unwrap();
                    assert_eq!(before.data, after.data);
                    assert_ne!(before.program_id, after.program_id);
                }
            }
            let root =
                env::temp_dir().join(format!("northstar-vrf-controls-{}", std::process::id()));
            fs::create_dir(&root).unwrap();
            let a = root.join("first");
            let b = root.join("second");
            let first = generate(&request, &fulfill, &setup, &a).unwrap();
            let repeated = generate(&request, &fulfill, &setup, &b).unwrap();
            assert_eq!(first, repeated);
            assert!(generate(&request, &fulfill, &setup, &a).is_err());
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[cfg(all(feature = "host", not(target_os = "zkvm")))]
fn main() -> std::process::ExitCode {
    match host::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
#[cfg(not(all(feature = "host", not(target_os = "zkvm"))))]
fn main() {
    eprintln!("vrf-candidate-controls requires native host feature; it never runs in a guest");
    std::process::exit(2);
}
