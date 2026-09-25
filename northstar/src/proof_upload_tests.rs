use {
    super::*,
    borsh::BorshSerialize,
    northstar_zk_types::SP1_GROTH16_PROOF_V1_LEN,
    solana_account::{Account, AccountSharedData, WritableAccount},
    solana_runtime::genesis_utils::create_genesis_config,
};

const PORTAL: Pubkey = solana_pubkey::pubkey!("5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf");

fn put<T: BorshSerialize>(bank: &Bank, key: Pubkey, value: &T) {
    bank.store_account(
        &key,
        &AccountSharedData::from(Account {
            lamports: 10_000_000,
            data: borsh::to_vec(value).unwrap(),
            owner: PORTAL,
            executable: false,
            rent_epoch: 0,
        }),
    );
}

fn binding_hash(binding: &ProofJobBinding) -> [u8; 32] {
    hashv(&[
        b"northstar-er-step-v1",
        &binding.portal,
        &binding.session,
        &binding.checkpoint,
        &[ER_STEP_PROOF_KIND_FULL_TRANSACTION],
        &[ER_STEP_PROOF_VERSION_V1],
        &binding.er_slot.to_le_bytes(),
        &u64::from(binding.step).to_le_bytes(),
        &binding.public_inputs[32..64],
        &binding.public_inputs[96..128],
        &binding.public_inputs[128..160],
        &binding.public_inputs[160..192],
        &binding.public_inputs[192..224],
        &binding.public_inputs[224..256],
    ])
    .to_bytes()
}

struct Fixture {
    bank: Bank,
    binding: ProofJobBinding,
    artifact: CheckpointArtifactV1,
    challenger: Pubkey,
    checkpoint: Checkpoint,
    challenge: Challenge,
    upload: StepProofAccount,
    proof: [u8; SP1_GROTH16_PROOF_V1_LEN],
}

impl Fixture {
    fn new() -> Self {
        let (_, _, accounts): (u64, u64, Vec<(Pubkey, Account)>) = bincode::deserialize(
            include_bytes!("../zkvm-replay/evidence/resolver-v1/resolver-fixture.bin"),
        )
        .unwrap();
        let session = Session::try_from_slice(&accounts[0].1.data).unwrap();
        let mut checkpoint = Checkpoint::try_from_slice(&accounts[1].1.data).unwrap();
        let mut challenge = Challenge::try_from_slice(&accounts[2].1.data).unwrap();
        let mut da = DataAvailabilityProof::try_from_slice(&accounts[3].1.data).unwrap();
        let mut upload = StepProofAccount::try_from_slice(&accounts[4].1.data).unwrap();
        let mut cursor = CheckpointCursor::try_from_slice(&accounts[6].1.data).unwrap();
        let session_key = northstar_portal::find_session_pda(&PORTAL).0;
        let (artifact, _) = crate::ephemeral_tx_client::tests::supported_sbf_checkpoint_with_steps(
            0,
            session_key,
            1,
        );
        let binding = expected_binding(PORTAL, &session, &artifact, 0).unwrap();
        let checkpoint_key = Pubkey::new_from_array(binding.checkpoint);
        let challenge_key = Pubkey::new_from_array(binding.challenge);
        let value = &artifact.checkpoint;
        checkpoint.session = session_key;
        checkpoint.er_slot = binding.er_slot;
        checkpoint.step_count = 1;
        checkpoint.previous_state_root = value.previous_state_root;
        checkpoint.new_state_root = value.new_state_root;
        checkpoint.trace_root = value.trace_root;
        checkpoint.tx_effect_root = value.transaction_effect_root;
        checkpoint.readonly_l1_root = value.readonly_l1_root;
        checkpoint.da_commitment = value.da_commitment;
        checkpoint.effect_commitment = value.effect_commitment;
        checkpoint.proposer = session.validator;
        checkpoint.status = CheckpointStatus::Challenged;
        checkpoint.challenge_resolved = false;
        challenge.checkpoint = checkpoint_key;
        challenge.respondent = session.validator;
        challenge.start_step = 0;
        challenge.end_step = 1;
        challenge.start_state_root = artifact.da.pages[0].pre_state_root;
        challenge.end_state_root = artifact.da.pages[0].post_state_root;
        challenge.status = ChallengeStatus::Active;
        challenge.turn = ChallengeTurn::Prove;
        challenge.turn_deadline_l1_slot = 1000;
        challenge.hard_deadline_l1_slot = 1000;
        checkpoint.challenger = challenge.challenger;
        da.checkpoint = checkpoint_key;
        da.challenge = challenge_key;
        da.commitment = value.da_commitment;
        da.payload_root = value.da_commitment;
        da.status = DataAvailabilityStatus::Revealed;
        cursor.session = session_key;
        cursor.active_checkpoint = checkpoint_key;
        cursor.active_er_slot = binding.er_slot;
        upload.checkpoint = checkpoint_key;
        upload.challenge = challenge_key;
        upload.authority = challenge.challenger;
        upload.proof_kind = ER_STEP_PROOF_KIND_FULL_TRANSACTION;
        upload.proof_version = ER_STEP_PROOF_VERSION_V1;
        upload.step_index = 0;
        upload.session_context = binding.public_inputs[32..64].try_into().unwrap();
        upload.tx_effect_root = artifact.da.pages[0].transaction_effect_commitment;
        upload.readonly_l1_root = value.readonly_l1_root;
        upload.settlement_effect_root = value.effect_commitment;
        upload.public_input_hash = binding_hash(&binding);
        upload.written_len = 0;
        upload.sealed = false;
        upload.data = [0; SP1_GROTH16_PROOF_V1_LEN];
        upload.proof_hash = [0; 32];
        let bank = Bank::new_for_tests(&create_genesis_config(1_000_000_000).genesis_config);
        put(&bank, session_key, &session);
        put(&bank, checkpoint_key, &checkpoint);
        put(&bank, challenge_key, &challenge);
        put(
            &bank,
            northstar_portal::find_da_proof_pda(&PORTAL, &challenge_key).0,
            &da,
        );
        put(
            &bank,
            northstar_portal::find_checkpoint_cursor_pda(&PORTAL, &session_key).0,
            &cursor,
        );
        Self {
            bank,
            binding,
            artifact,
            challenger: challenge.challenger,
            checkpoint,
            challenge,
            upload,
            proof: *include_bytes!(
                "../zkvm-replay/evidence/sp1-v6.8.0/northstar-sp1-groth16-onchain.bin"
            ),
        }
    }

    fn action(&self, with_proof: bool) -> Result<ProofUploadAction, &'static str> {
        let proof = VerifiedProof::for_reconciliation_test(self.binding.clone(), self.proof);
        reconcile_upload(
            &self.bank,
            &self.challenger,
            &self.binding,
            &self.artifact,
            with_proof.then_some(&proof),
        )
    }

    fn publish_upload(&mut self, length: usize, sealed: bool) {
        self.upload.written_len = length as u32;
        self.upload.sealed = sealed;
        self.upload.data[..length].copy_from_slice(&self.proof[..length]);
        self.upload.proof_hash = hashv(&[&self.proof[..length]]).to_bytes();
        self.store_upload();
    }

    fn store_upload(&self) {
        put(
            &self.bank,
            northstar_portal::find_step_proof_pda(
                &PORTAL,
                &Pubkey::new_from_array(self.binding.checkpoint),
            )
            .0,
            &self.upload,
        );
    }

    fn instruction(&self) -> PortalInstruction {
        let ProofUploadAction::Submit(instruction) = self.action(true).unwrap() else {
            panic!("expected upload instruction")
        };
        assert_eq!(instruction.program_id, PORTAL);
        assert_eq!(instruction.accounts[0].pubkey, self.challenger);
        assert!(instruction.accounts[0].is_signer);
        PortalInstruction::try_from_slice(&instruction.data).unwrap()
    }
}

#[test]
fn l1_upload_progress_drives_create_append_seal_and_resolve() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.action(false), Ok(ProofUploadAction::Prove));
    assert!(matches!(
        fixture.instruction(),
        PortalInstruction::CreateStepProof(_)
    ));
    for offset in [0, 128, 256] {
        fixture.publish_upload(offset, false);
        let PortalInstruction::WriteStepProof(write) = fixture.instruction() else {
            panic!("expected chunk")
        };
        assert_eq!(write.offset, offset as u32);
        assert_eq!(
            &write.chunk[..usize::from(write.chunk_len)],
            &fixture.proof[offset..(offset + MAX_STEP_PROOF_CHUNK).min(fixture.proof.len())]
        );
    }
    fixture.publish_upload(356, false);
    assert!(matches!(
        fixture.instruction(),
        PortalInstruction::SealStepProof(_)
    ));
    fixture.publish_upload(356, true);
    assert!(matches!(
        fixture.instruction(),
        PortalInstruction::ResolveChallenge(_)
    ));
}

#[test]
fn deadline_boundary_matches_portal() {
    for hard_deadline in [false, true] {
        let mut fixture = Fixture::new();
        if hard_deadline {
            fixture.challenge.hard_deadline_l1_slot = fixture.bank.slot();
        } else {
            fixture.challenge.turn_deadline_l1_slot = fixture.bank.slot();
        }
        put(
            &fixture.bank,
            Pubkey::new_from_array(fixture.binding.challenge),
            &fixture.challenge,
        );
        assert_eq!(
            fixture.action(false),
            Err("proof challenge deadline elapsed")
        );
    }
}

#[test]
fn changed_upload_prefix_or_authority_is_rejected() {
    let mut fixture = Fixture::new();
    fixture.publish_upload(128, false);
    fixture.upload.data[0] ^= 1;
    fixture.store_upload();
    assert_eq!(fixture.action(true), Err("proof uploaded prefix mismatch"));
    fixture.publish_upload(128, false);
    fixture.upload.authority = Pubkey::new_unique();
    fixture.store_upload();
    assert_eq!(
        fixture.action(true),
        Err("proof upload authority or context changed")
    );
}

#[test]
fn changed_account_owner_and_session_nonce_are_rejected() {
    let fixture = Fixture::new();
    let session_key = Pubkey::new_from_array(fixture.binding.session);
    let mut account = fixture.bank.get_account(&session_key).unwrap();
    account.set_owner(Pubkey::new_unique());
    fixture.bank.store_account(&session_key, &account);
    assert_eq!(
        fixture.action(false),
        Err("proof account owner or discriminator")
    );
    let mut session = Session::try_from_slice(account.data()).unwrap();
    session.nonce += 1;
    put(&fixture.bank, session_key, &session);
    assert_eq!(fixture.action(false), Err("proof job context changed"));
}

#[test]
fn incomplete_seal_and_changed_binding_hash_are_rejected() {
    let mut fixture = Fixture::new();
    fixture.publish_upload(128, true);
    assert_eq!(fixture.action(true), Err("proof sealed result mismatch"));
    fixture.publish_upload(356, true);
    fixture.upload.public_input_hash[0] ^= 1;
    fixture.store_upload();
    assert_eq!(
        fixture.action(true),
        Err("proof upload authority or context changed")
    );
}

#[test]
fn verified_results_cannot_be_reused_under_another_job_binding() {
    let fixture = Fixture::new();
    let mut other = fixture.binding.clone();
    other.challenge[0] ^= 1;
    let proof = VerifiedProof::for_reconciliation_test(other, fixture.proof);
    assert_eq!(
        reconcile_upload(
            &fixture.bank,
            &fixture.challenger,
            &fixture.binding,
            &fixture.artifact,
            Some(&proof)
        ),
        Err("verified proof job binding changed")
    );
}

#[test]
fn resolved_proofs_do_not_create_more_upload_transactions() {
    let mut fixture = Fixture::new();
    fixture.checkpoint.challenge_resolved = true;
    fixture.checkpoint.status = CheckpointStatus::Pending;
    fixture.challenge.status = ChallengeStatus::ValidatorWon;
    put(
        &fixture.bank,
        Pubkey::new_from_array(fixture.binding.checkpoint),
        &fixture.checkpoint,
    );
    put(
        &fixture.bank,
        Pubkey::new_from_array(fixture.binding.challenge),
        &fixture.challenge,
    );
    assert_eq!(
        fixture.action(true),
        Ok(ProofUploadAction::Finished {
            proof_accepted: true
        })
    );
}

#[test]
fn another_challenger_or_changed_checkpoint_is_not_signed() {
    let mut fixture = Fixture::new();
    assert_eq!(
        reconcile_upload(
            &fixture.bank,
            &Pubkey::new_unique(),
            &fixture.binding,
            &fixture.artifact,
            Some(&VerifiedProof::for_reconciliation_test(
                fixture.binding.clone(),
                fixture.proof
            ))
        ),
        Err("proof challenger authority binding")
    );
    fixture.checkpoint.effect_commitment[0] ^= 1;
    put(
        &fixture.bank,
        Pubkey::new_from_array(fixture.binding.checkpoint),
        &fixture.checkpoint,
    );
    assert_eq!(fixture.action(true), Err("proof checkpoint changed"));
}
