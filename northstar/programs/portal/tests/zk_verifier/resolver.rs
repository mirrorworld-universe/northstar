use {
    borsh::{BorshDeserialize, BorshSerialize},
    northstar_portal::{
        Challenge, ChallengeStatus, ChallengeTurn, Checkpoint, CheckpointBondStatus,
        CheckpointCursor, CheckpointStatus, DataAvailabilityProof, DataAvailabilityStatus,
        PortalError, PortalInstruction, ResolveChallenge, StepProofAccount, StepProofVerifierMode,
    },
    solana_account::{Account, AccountSharedData},
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_instruction::{AccountMeta, Instruction},
    solana_program_test::ProgramTest,
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hashv,
    solana_signer::Signer,
    solana_transaction::Transaction,
};

const PROGRAM: Pubkey = solana_pubkey::pubkey!("5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf");
type Fixture = (u64, u64, Vec<(Pubkey, Account)>);

fn fixture() -> Fixture {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../zkvm-replay/evidence/resolver-v1/resolver-fixture.bin");
    bincode::deserialize(&std::fs::read(path).unwrap()).unwrap()
}

fn update<T: BorshDeserialize + BorshSerialize>(
    account: &mut Account,
    change: impl FnOnce(&mut T),
) {
    let mut state = T::try_from_slice(&account.data).unwrap();
    change(&mut state);
    account.data = borsh::to_vec(&state).unwrap();
}

#[tokio::test]
async fn production_resolver_binds_metadata_and_preserves_rejected_state() {
    let (slot, er_slot, original) = fixture();
    assert_eq!(original.len(), 7);
    let mut test = ProgramTest::new("northstar_portal", PROGRAM, None);
    for (key, account) in &original {
        test.add_account(*key, account.clone());
    }
    let mut context = test.start_with_context().await;
    context.warp_to_slot(slot).unwrap();
    let cases = [
        "valid",
        "session_pda",
        "session_owner",
        "checkpoint_owner",
        "challenge_owner",
        "da_owner",
        "cursor_owner",
        "proof_bytes",
        "proof_owner",
        "proof_pda",
        "checkpoint_binding",
        "challenge_binding",
        "authority",
        "unsealed",
        "length",
        "proof_hash",
        "public_hash",
        "kind",
        "version",
        "session_context",
        "transaction_effect",
        "readonly_root",
        "settlement_root",
        "pre_root",
        "post_root",
        "turn",
        "range",
        "da_state",
        "cursor",
        "step_index",
        "invalid_proof",
    ];
    for (index, name) in cases.iter().enumerate() {
        let mut accounts = original.clone();
        let rejection = match *name {
            "valid" => None,
            "session_pda" => {
                accounts[0].0 = Pubkey::new_unique();
                Some(PortalError::InvalidPdaSeeds)
            }
            "session_owner" => {
                accounts[0].1.owner = solana_sdk_ids::system_program::id();
                Some(PortalError::SessionAccountOwnerMismatch)
            }
            "checkpoint_owner" => {
                accounts[1].1.owner = solana_sdk_ids::system_program::id();
                Some(PortalError::CheckpointStateInvalid)
            }
            "challenge_owner" => {
                accounts[2].1.owner = solana_sdk_ids::system_program::id();
                Some(PortalError::ChallengeStateInvalid)
            }
            "da_owner" => {
                accounts[3].1.owner = solana_sdk_ids::system_program::id();
                Some(PortalError::DataAvailabilityStateInvalid)
            }
            "cursor_owner" => {
                accounts[6].1.owner = solana_sdk_ids::system_program::id();
                Some(PortalError::CheckpointCursorStateInvalid)
            }
            "proof_bytes" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.data[100] ^= 1);
                Some(PortalError::StepProofHashMismatch)
            }
            "proof_owner" => {
                accounts[4].1.owner = solana_sdk_ids::system_program::id();
                Some(PortalError::StepProofStateInvalid)
            }
            "proof_pda" => {
                accounts[4].0 = Pubkey::new_unique();
                Some(PortalError::InvalidPdaSeeds)
            }
            "checkpoint_binding" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| {
                    s.checkpoint = Pubkey::new_unique()
                });
                Some(PortalError::StepProofStateInvalid)
            }
            "challenge_binding" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| {
                    s.challenge = Pubkey::new_unique()
                });
                Some(PortalError::StepProofCheckpointMismatch)
            }
            "authority" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| {
                    s.authority = Pubkey::new_unique()
                });
                Some(PortalError::Unauthorized)
            }
            "unsealed" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.sealed = false);
                Some(PortalError::StepProofNotSealed)
            }
            "length" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.written_len = 0);
                Some(PortalError::StepProofChunkOutOfBounds)
            }
            "proof_hash" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.proof_hash[0] ^= 1);
                Some(PortalError::StepProofHashMismatch)
            }
            "public_hash" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.public_input_hash[0] ^= 1);
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "kind" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.proof_kind ^= 1);
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "version" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.proof_version ^= 1);
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "session_context" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.session_context[31] ^= 1);
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "transaction_effect" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.tx_effect_root[31] ^= 1);
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "readonly_root" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.readonly_l1_root[31] ^= 1);
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "settlement_root" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| {
                    s.settlement_effect_root[31] ^= 1
                });
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "pre_root" => {
                update::<Challenge>(&mut accounts[2].1, |s| s.start_state_root[31] ^= 1);
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "post_root" => {
                update::<Challenge>(&mut accounts[2].1, |s| s.end_state_root[31] ^= 1);
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "turn" => {
                update::<Challenge>(&mut accounts[2].1, |s| s.turn = ChallengeTurn::Respondent);
                Some(PortalError::ChallengeTurnInvalid)
            }
            "range" => {
                update::<Challenge>(&mut accounts[2].1, |s| s.end_step += 1);
                Some(PortalError::ChallengeTurnInvalid)
            }
            "da_state" => {
                update::<DataAvailabilityProof>(&mut accounts[3].1, |s| {
                    s.status = DataAvailabilityStatus::Missing
                });
                Some(PortalError::DataAvailabilityStateInvalid)
            }
            "cursor" => {
                update::<CheckpointCursor>(&mut accounts[6].1, |s| s.active_er_slot += 1);
                Some(PortalError::CheckpointStateInvalid)
            }
            "step_index" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| s.step_index += 1);
                None
            }
            "invalid_proof" => {
                update::<StepProofAccount>(&mut accounts[4].1, |s| {
                    s.data[163] ^= 1;
                    s.proof_hash = hashv(&[&s.data]).to_bytes();
                });
                None
            }
            _ => unreachable!(),
        };
        for (key, account) in &accounts {
            context.set_account(key, &AccountSharedData::from(account.clone()));
        }
        let keys: Vec<_> = accounts.iter().map(|(key, _)| *key).collect();
        let instruction = Instruction {
            program_id: PROGRAM,
            accounts: vec![
                AccountMeta::new_readonly(context.payer.pubkey(), true),
                AccountMeta::new_readonly(keys[0], false),
                AccountMeta::new(keys[1], false),
                AccountMeta::new(keys[2], false),
                AccountMeta::new_readonly(keys[3], false),
                AccountMeta::new_readonly(keys[4], false),
                AccountMeta::new(keys[5], false),
                AccountMeta::new(keys[6], false),
            ],
            data: borsh::to_vec(&PortalInstruction::ResolveChallenge(ResolveChallenge {
                er_slot,
                verifier_mode: StepProofVerifierMode::Production,
            }))
            .unwrap(),
        };
        // Distinct signatures ensure every case executes even when its instruction is unchanged.
        let transaction = Transaction::new_signed_with_payer(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(130_000 + index as u32),
                instruction,
            ],
            Some(&context.payer.pubkey()),
            &[&context.payer],
            context.last_blockhash,
        );
        let result = context.banks_client.process_transaction(transaction).await;
        if let Some(error) = rejection {
            let actual = format!("{:?}", result.unwrap_err());
            assert!(
                actual.contains(&format!("Custom({})", error as u32)),
                "{name}: {actual}"
            );
        } else {
            result.unwrap_or_else(|error| panic!("{name}: {error:?}"));
            let valid = *name == "valid";
            let bond = Checkpoint::try_from_slice(&accounts[1].1.data)
                .unwrap()
                .bond_lamports;
            update::<Checkpoint>(&mut accounts[1].1, |s| {
                s.status = if valid {
                    CheckpointStatus::Pending
                } else {
                    CheckpointStatus::Invalid
                };
                s.challenge_resolved = true;
                if !valid {
                    s.bond_status = CheckpointBondStatus::Slashed;
                }
            });
            update::<Challenge>(&mut accounts[2].1, |s| {
                s.status = if valid {
                    ChallengeStatus::ValidatorWon
                } else {
                    ChallengeStatus::ChallengerWon
                }
            });
            if !valid {
                accounts[1].1.lamports -= bond;
                accounts[5].1.lamports += bond;
                update::<CheckpointCursor>(&mut accounts[6].1, |s| {
                    s.active_er_slot = 0;
                    s.active_checkpoint = Pubkey::default();
                });
            }
        }
        for (key, expected) in accounts {
            let actual = context
                .banks_client
                .get_account(key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                actual, expected,
                "{name}: unexpected account change for {key}"
            );
        }
    }
}

#[tokio::test]
async fn create_proof_authenticates_transaction_path_before_allocating() {
    let (slot, er_slot, original) = fixture();
    let proof = StepProofAccount::try_from_slice(&original[4].1.data).unwrap();
    let witness_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../zkvm-replay/evidence/resolver-v1/witness-v2.bin");
    let witness =
        northstar_transaction_proof::decode_witness(&std::fs::read(witness_path).unwrap()).unwrap();
    let path = witness.checkpoint.transaction_effect_path;
    let mut siblings = [[0; 32]; northstar_portal::TX_EFFECT_AUTH_PATH_NODES];
    siblings[..path.siblings.len()].copy_from_slice(&path.siblings);
    let mut test = ProgramTest::new("northstar_portal", PROGRAM, None);
    for (key, account) in &original {
        test.add_account(*key, account.clone());
    }
    let mut context = test.start_with_context().await;
    context.warp_to_slot(slot).unwrap();
    for (index, name) in [
        "valid",
        "sibling",
        "path_length",
        "effect_root",
        "step",
        "context",
        "readonly",
        "settlement",
        "pda",
        "authority",
    ]
    .iter()
    .enumerate()
    {
        let mut accounts = original.clone();
        if *name != "authority" {
            update::<Challenge>(&mut accounts[2].1, |state| {
                state.challenger = context.payer.pubkey()
            });
        }
        accounts[4].1 = Account::default();
        let mut request = northstar_portal::CreateStepProof {
            er_slot,
            proof_kind: proof.proof_kind,
            proof_version: proof.proof_version,
            step_index: proof.step_index,
            session_context: proof.session_context,
            tx_effect_root: proof.tx_effect_root,
            tx_effect_path_len: path.siblings.len() as u8,
            tx_effect_path: siblings,
            readonly_l1_root: proof.readonly_l1_root,
            settlement_effect_root: proof.settlement_effect_root,
        };
        let rejection = match *name {
            "valid" => None,
            "sibling" => {
                request.tx_effect_path[0][31] ^= 1;
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "path_length" => {
                request.tx_effect_path_len -= 1;
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "effect_root" => {
                request.tx_effect_root[31] ^= 1;
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "step" => {
                request.step_index += 1;
                Some(PortalError::ChallengeTurnInvalid)
            }
            "context" => {
                request.session_context[31] ^= 1;
                Some(PortalError::StepProofPublicInputMismatch)
            }
            "readonly" => {
                request.readonly_l1_root[31] ^= 1;
                Some(PortalError::Unauthorized)
            }
            "settlement" => {
                request.settlement_effect_root[31] ^= 1;
                Some(PortalError::Unauthorized)
            }
            "pda" => {
                accounts[4].0 = Pubkey::new_unique();
                Some(PortalError::InvalidPdaSeeds)
            }
            "authority" => Some(PortalError::Unauthorized),
            _ => unreachable!(),
        };
        for (key, account) in &accounts {
            context.set_account(key, &AccountSharedData::from(account.clone()));
        }
        let instruction = Instruction {
            program_id: PROGRAM,
            accounts: vec![
                AccountMeta::new(context.payer.pubkey(), true),
                AccountMeta::new_readonly(accounts[0].0, false),
                AccountMeta::new_readonly(accounts[1].0, false),
                AccountMeta::new_readonly(accounts[2].0, false),
                AccountMeta::new(accounts[4].0, false),
                AccountMeta::new_readonly(solana_sdk_ids::system_program::id(), false),
            ],
            data: borsh::to_vec(&PortalInstruction::CreateStepProof(request)).unwrap(),
        };
        let transaction = Transaction::new_signed_with_payer(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(200_000 + index as u32),
                instruction,
            ],
            Some(&context.payer.pubkey()),
            &[&context.payer],
            context.last_blockhash,
        );
        let result = context.banks_client.process_transaction(transaction).await;
        if let Some(error) = rejection {
            let actual = format!("{:?}", result.unwrap_err());
            assert!(
                actual.contains(&format!("Custom({})", error as u32)),
                "{name}: {actual}"
            );
            assert!(
                context
                    .banks_client
                    .get_account(accounts[4].0)
                    .await
                    .unwrap()
                    .is_none(),
                "{name}: rejected creation allocated proof account"
            );
        } else {
            result.unwrap();
            let created = context
                .banks_client
                .get_account(accounts[4].0)
                .await
                .unwrap()
                .unwrap();
            let state = StepProofAccount::try_from_slice(&created.data).unwrap();
            assert_eq!(state.public_input_hash, proof.public_input_hash);
            assert_eq!(state.authority, context.payer.pubkey());
            assert_eq!(state.written_len, 0);
            assert!(!state.sealed);
        }
        for (index, (key, expected)) in accounts.iter().enumerate() {
            if index == 4 {
                continue;
            }
            assert_eq!(
                context
                    .banks_client
                    .get_account(*key)
                    .await
                    .unwrap()
                    .unwrap(),
                *expected,
                "{name}: state changed"
            );
        }
    }
}
