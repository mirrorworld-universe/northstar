#![cfg(feature = "host")]

use {
    northstar_transaction_proof::{
        checkpoint::fixture_checkpoint_binding, fixture::assemble_replay_witness_v1, replay,
        set_trace_hash, ReplayError,
    },
    solana_account::ReadableAccount,
    solana_runtime::conformance::{
        proof_fixture::{full_transaction_fixture_v1, ExecutedFullTransactionFixtureV1},
        trace::{build_transaction_trace_v1, fixture_trace_header_v1},
        txn::execute_er_txn_with_trace,
    },
};

#[test]
fn gasless_execution_replays_without_changing_the_relation() {
    let mut fixture = full_transaction_fixture_v1();
    fixture.expected_fee_payer = fixture
        .accounts
        .iter()
        .find(|(key, _)| *key == fixture.fee_payer)
        .unwrap()
        .1
        .clone();
    let execution = execute_er_txn_with_trace(
        &fixture.accounts,
        fixture.feature_set.clone(),
        fixture.blockhash_queue.clone(),
        fixture.fee_rate_governor.clone(),
        0,
        fixture.transaction.clone(),
        &solana_fee_structure::FeeStructure {
            lamports_per_signature: 0,
            lamports_per_write_lock: 0,
            compute_fee_bins: vec![],
        },
        150,
    );
    let header = fixture_trace_header_v1(&fixture.transaction_bytes, b"gasless-replay-regression");
    let trace = build_transaction_trace_v1(header, &fixture.accounts, &execution);
    let executed = ExecutedFullTransactionFixtureV1 {
        fixture,
        execution,
        trace,
    };
    executed.assert_expected_success();
    assert_eq!(executed.fixture.expected_fee_payer.lamports(), 10_000_000);
    let mut witness = assemble_replay_witness_v1(executed).unwrap();
    assert_eq!(witness.result.transaction_fee, 0);
    witness.runtime.lamports_per_signature = 0;
    witness.checkpoint = fixture_checkpoint_binding(&witness).unwrap();
    set_trace_hash(&mut witness);
    replay(&witness).unwrap();

    witness.runtime.lamports_per_signature = 5_000;
    assert!(matches!(replay(&witness), Err(ReplayError::Fee)));
}
