#![cfg(feature = "zk-verifier-prototype")]

use {
    northstar_portal::{PortalError, PortalInstruction, VerifyErStepProofV1},
    northstar_zk_types::{
        ErStepPublicInputsV1, FrBytes, BN254_FR_MODULUS_BE, ER_STEP_PROOF_KIND_FULL_TRANSACTION,
        ER_STEP_PROOF_VERSION_V1, SP1_GROTH16_PROOF_V1_LEN, SP1_GROTH16_VK_HASH_PREFIX,
        SP1_GROTH16_VK_ROOT,
    },
    serde_json::Value,
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

fn proof_bytes() -> [u8; SP1_GROTH16_PROOF_V1_LEN] {
    let mut bytes = [0; SP1_GROTH16_PROOF_V1_LEN];
    bytes[..4].copy_from_slice(&SP1_GROTH16_VK_HASH_PREFIX);
    bytes[36..68].copy_from_slice(&SP1_GROTH16_VK_ROOT);
    bytes[99] = 1;
    let vector: Value = serde_json::from_str(include_str!(
        "../../../zk-prover/test-vectors/one-account-transition-v1.json"
    ))
    .unwrap();
    let proof = &vector["proof_be"];
    bytes[100..164].copy_from_slice(&decode::<64>(proof["a"].as_str().unwrap()));
    bytes[164..292].copy_from_slice(&decode::<128>(proof["b"].as_str().unwrap()));
    bytes[292..].copy_from_slice(&decode::<64>(proof["c"].as_str().unwrap()));
    bytes
}

fn public_input_bytes() -> [u8; 256] {
    let public = ErStepPublicInputsV1 {
        domain: FrBytes::er_step_domain_v1(
            ER_STEP_PROOF_KIND_FULL_TRANSACTION,
            ER_STEP_PROOF_VERSION_V1,
        ),
        session_context: FrBytes::from_u64(1),
        slot_step: FrBytes::from_u64_pair(2, 3),
        pre_state_root: FrBytes::from_u64(4),
        post_state_root: FrBytes::from_u64(5),
        tx_effect_root: FrBytes::from_u64(6),
        readonly_l1_root: FrBytes::from_u64(7),
        settlement_effect_root: FrBytes::from_u64(8),
    };
    let mut bytes = [0; 256];
    for (output, input) in bytes.chunks_exact_mut(32).zip(public.to_array()) {
        output.copy_from_slice(&input);
    }
    bytes
}

fn verifier_instruction() -> Instruction {
    Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts: vec![],
        data: borsh::to_vec(&PortalInstruction::VerifyErStepProofV1(
            VerifyErStepProofV1 {
                proof: proof_bytes(),
                public_inputs: public_input_bytes(),
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

fn transaction(context: &ProgramTestContext, instruction: Instruction) -> Transaction {
    Transaction::new_signed_with_payer(
        &[instruction],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        context.last_blockhash,
    )
}

#[test]
fn sp1_verifier_instruction_fits_solana_transaction() {
    let instruction = verifier_instruction();
    assert_eq!(instruction.data.len(), 613);
    let payer = solana_keypair::Keypair::new();
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&payer.pubkey()),
        &[&payer],
        solana_hash::Hash::new_unique(),
    );
    let transaction_bytes = bincode::serialized_size(&transaction).unwrap();
    println!("SP1 verifier transaction bytes: {transaction_bytes}");
    assert_eq!(transaction_bytes, 783);
}

#[tokio::test]
async fn sbf_verifier_rejects_invalid_sp1_envelope() {
    let context = setup().await;
    let mut instruction = verifier_instruction();
    instruction.data[1] ^= 1;
    let result = context
        .banks_client
        .simulate_transaction(transaction(&context, instruction))
        .await
        .unwrap();
    let error = result.result.unwrap().unwrap_err();
    assert!(format!("{error:?}").contains(&format!(
        "Custom({})",
        PortalError::StepProofVerificationFailed as u32
    )));
}

#[tokio::test]
async fn sbf_verifier_preliminary_full_path_compute_units() {
    let context = setup().await;
    let result = context
        .banks_client
        .simulate_transaction(transaction(&context, verifier_instruction()))
        .await
        .unwrap();
    let error = result.result.unwrap().unwrap_err();
    assert!(format!("{error:?}").contains(&format!(
        "Custom({})",
        PortalError::StepProofVerificationFailed as u32
    )));
    let compute_units = result.simulation_details.unwrap().units_consumed;
    println!("SP1 adapter preliminary full-path CU: {compute_units}");
    assert!(compute_units > 80_000);
    assert!(compute_units <= 130_000);
}

#[tokio::test]
async fn sbf_verifier_rejects_noncanonical_portal_input() {
    let context = setup().await;
    let mut instruction = verifier_instruction();
    let public_inputs_start = 1 + SP1_GROTH16_PROOF_V1_LEN;
    instruction.data[public_inputs_start..public_inputs_start + 32]
        .copy_from_slice(&BN254_FR_MODULUS_BE);
    let result = context
        .banks_client
        .simulate_transaction(transaction(&context, instruction))
        .await
        .unwrap();
    let error = result.result.unwrap().unwrap_err();
    assert!(format!("{error:?}").contains(&format!(
        "Custom({})",
        PortalError::StepProofVerificationFailed as u32
    )));
}
