#![cfg(feature = "zk-verifier-prototype")]

use {
    northstar_portal::{PortalError, PortalInstruction, VerifyErStepProofV1},
    serde_json::Value,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_instruction::Instruction,
    solana_program_test::{ProgramTest, ProgramTestContext},
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_transaction::Transaction,
};

const PORTAL_PROGRAM_ID: Pubkey =
    solana_pubkey::pubkey!("GikCSCpYUq7QR7esoK6GM4UbJzKgdKNvS5bR1rBYH5E4");

fn decode<const N: usize>(value: &str) -> [u8; N] {
    hex::decode(value.strip_prefix("0x").unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

fn verifier_instruction() -> Instruction {
    let vector: Value = serde_json::from_str(include_str!(
        "../../../zk-prover/test-vectors/one-account-transition-v1.json"
    ))
    .unwrap();
    let proof = &vector["proof_be"];
    let mut proof_bytes = [0; 256];
    proof_bytes[..64].copy_from_slice(&decode::<64>(proof["a"].as_str().unwrap()));
    proof_bytes[64..192].copy_from_slice(&decode::<128>(proof["b"].as_str().unwrap()));
    proof_bytes[192..].copy_from_slice(&decode::<64>(proof["c"].as_str().unwrap()));
    let public = vector["public_inputs_be"].as_array().unwrap();
    let mut public_inputs = [0; 256];
    for (output, input) in public_inputs.chunks_exact_mut(32).zip(public) {
        output.copy_from_slice(&decode::<32>(input.as_str().unwrap()));
    }
    Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts: vec![],
        data: borsh::to_vec(&PortalInstruction::VerifyErStepProofV1(
            VerifyErStepProofV1 {
                proof: proof_bytes,
                public_inputs,
            },
        ))
        .unwrap(),
    }
}

async fn setup() -> ProgramTestContext {
    let mut program_test = ProgramTest::default();
    program_test.prefer_bpf(true);
    program_test.add_program("northstar_portal", PORTAL_PROGRAM_ID, None);
    program_test.start_with_context().await
}

fn transaction(context: &ProgramTestContext, instructions: &[Instruction]) -> Transaction {
    Transaction::new_signed_with_payer(
        instructions,
        Some(&context.payer.pubkey()),
        &[&context.payer],
        context.last_blockhash,
    )
}

#[tokio::test]
async fn sbf_verifier_accepts_transition_vector() {
    let context = setup().await;
    let instruction = verifier_instruction();
    assert_eq!(instruction.data.len(), 513);
    let transaction = transaction(&context, &[instruction]);
    assert!(bincode::serialized_size(&transaction).unwrap() <= 1_232);

    let result = context
        .banks_client
        .simulate_transaction(transaction)
        .await
        .unwrap();
    assert_eq!(result.result, Some(Ok(())));
    assert!(result.simulation_details.unwrap().units_consumed > 100_000);
}

#[tokio::test]
async fn sbf_verifier_rejects_mutated_proof() {
    let context = setup().await;
    let mut instruction = verifier_instruction();
    instruction.data[64] ^= 1;
    let result = context
        .banks_client
        .simulate_transaction(transaction(&context, &[instruction]))
        .await
        .unwrap();
    let error = result.result.unwrap().unwrap_err();
    assert!(format!("{error:?}").contains(&format!(
        "Custom({})",
        PortalError::StepProofVerificationFailed as u32
    )));
}

#[tokio::test]
async fn sbf_verifier_respects_compute_budget() {
    let context = setup().await;
    let verifier = verifier_instruction();
    let budget = ComputeBudgetInstruction::set_compute_unit_limit(100_000);
    let result = context
        .banks_client
        .simulate_transaction(transaction(&context, &[budget, verifier]))
        .await
        .unwrap();
    assert!(result.result.unwrap().is_err());
}
