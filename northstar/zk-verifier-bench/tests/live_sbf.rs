use {
    northstar_zk_verifier_bench::PROGRAM_ID_BYTES,
    serde_json::Value,
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_program_test::{ProgramTest, ProgramTestContext},
    solana_pubkey::Pubkey,
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_transaction::Transaction,
};

const FR_MODULUS_BE: [u8; 32] = [
    0x30, 0x64, 0x4e, 0x72, 0xe1, 0x31, 0xa0, 0x29, 0xb8, 0x50, 0x45, 0xb6, 0x81, 0x81, 0x58, 0x5d,
    0x28, 0x33, 0xe8, 0x48, 0x79, 0xb9, 0x70, 0x91, 0x43, 0xe1, 0xf5, 0x93, 0xf0, 0x00, 0x00, 0x01,
];

fn decode(value: &str) -> Vec<u8> {
    hex::decode(value.strip_prefix("0x").unwrap()).unwrap()
}

fn fixture(inputs: usize) -> Instruction {
    let source = match inputs {
        8 => include_str!("../vectors/groth16-8-inputs.json"),
        12 => include_str!("../vectors/groth16-12-inputs.json"),
        16 => include_str!("../vectors/groth16-16-inputs.json"),
        _ => panic!("unsupported input count"),
    };
    let vector: Value = serde_json::from_str(source).unwrap();
    let selector = match inputs {
        8 => 0,
        12 => 1,
        16 => 2,
        _ => unreachable!(),
    };
    let mut data = vec![selector];
    data.extend(decode(vector["proof_be"]["a"].as_str().unwrap()));
    data.extend(decode(vector["proof_be"]["b"].as_str().unwrap()));
    data.extend(decode(vector["proof_be"]["c"].as_str().unwrap()));
    for input in vector["public_inputs_be"].as_array().unwrap() {
        data.extend(decode(input.as_str().unwrap()));
    }
    Instruction {
        program_id: Pubkey::new_from_array(PROGRAM_ID_BYTES),
        accounts: vec![],
        data,
    }
}

fn portal_state_accounts() -> [Pubkey; 7] {
    core::array::from_fn(|index| {
        let value = u8::try_from(index)
            .expect("account index fits u8")
            .checked_add(1)
            .expect("account seed does not overflow");
        Pubkey::new_from_array([value; 32])
    })
}

async fn setup() -> ProgramTestContext {
    let mut program_test = ProgramTest::default();
    program_test.prefer_bpf(true);
    program_test.add_program(
        "northstar_zk_verifier_bench",
        Pubkey::new_from_array(PROGRAM_ID_BYTES),
        None,
    );
    for pubkey in portal_state_accounts() {
        program_test.add_account(pubkey, Account::new(1, 0, &system_program::id()));
    }
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

#[tokio::test]
async fn measures_live_sbf_verifier_matrix() {
    let context = setup().await;
    println!(
        "inputs,payload_bytes,transaction_bytes,portal_envelope_bytes,raw_vk_bytes,compute_units"
    );
    for inputs in [8usize, 12, 16] {
        let instruction = fixture(inputs);
        let payload_bytes = instruction.data.len();
        let signed_transaction = transaction(&context, instruction.clone());
        let transaction_bytes = bincode::serialized_size(&signed_transaction).unwrap();
        let mut portal_envelope = instruction;
        portal_envelope.accounts = portal_state_accounts()
            .into_iter()
            .map(|pubkey| AccountMeta::new_readonly(pubkey, false))
            .collect();
        let portal_transaction = transaction(&context, portal_envelope);
        let portal_envelope_bytes = bincode::serialized_size(&portal_transaction).unwrap();
        let result = context
            .banks_client
            .simulate_transaction(signed_transaction)
            .await
            .unwrap();
        assert_eq!(result.result, Some(Ok(())));
        let compute_units = result.simulation_details.unwrap().units_consumed;
        let portal_result = context
            .banks_client
            .simulate_transaction(portal_transaction)
            .await
            .unwrap();
        assert_eq!(portal_result.result, Some(Ok(())));
        let raw_vk_bytes = 512usize
            .checked_add(inputs.checked_mul(64).unwrap())
            .unwrap();
        println!(
            "{inputs},{payload_bytes},{transaction_bytes},{portal_envelope_bytes},{raw_vk_bytes},\
             {compute_units}"
        );
        assert!(transaction_bytes <= 1_232);
        assert!(portal_envelope_bytes <= 1_232);
        assert!(compute_units < 200_000);
    }
}

#[tokio::test]
async fn reports_verifier_failure_modes() {
    let context = setup().await;

    let mut malformed = fixture(8);
    malformed.data.pop();
    let malformed_result = context
        .banks_client
        .simulate_transaction(transaction(&context, malformed))
        .await
        .unwrap();
    assert!(malformed_result.result.unwrap().is_err());

    let mut noncanonical = fixture(12);
    let first_public_input = 1usize
        .checked_add(256)
        .expect("fixture offset does not overflow");
    let first_public_input_end = first_public_input
        .checked_add(32)
        .expect("public input range does not overflow");
    noncanonical.data[first_public_input..first_public_input_end].copy_from_slice(&FR_MODULUS_BE);
    let noncanonical_result = context
        .banks_client
        .simulate_transaction(transaction(&context, noncanonical))
        .await
        .unwrap();
    assert!(noncanonical_result.result.unwrap().is_err());

    let mut mutated = fixture(16);
    mutated.data[64] ^= 1;
    let mutated_result = context
        .banks_client
        .simulate_transaction(transaction(&context, mutated))
        .await
        .unwrap();
    assert!(mutated_result.result.unwrap().is_err());
}
