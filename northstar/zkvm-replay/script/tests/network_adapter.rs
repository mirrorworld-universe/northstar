#![cfg(feature = "network")]

use std::process::Command;

#[test]
fn paid_requests_require_explicit_authorization_before_reading_inputs() {
    let output = Command::new(env!("CARGO_BIN_EXE_northstar-network-prover"))
        .env_clear()
        .args(["groth16", "missing-witness", "measurements.json", "test"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("NORTHSTAR_NETWORK_ALLOW_PAID=1"));
}

#[test]
fn local_hosted_requests_do_not_require_paid_authorization() {
    let output = Command::new(env!("CARGO_BIN_EXE_northstar-network-prover"))
        .env_clear()
        .env("NORTHSTAR_NETWORK_MODE", "local-hosted")
        .env("NORTHSTAR_NETWORK_RPC_URL", "http://127.0.0.1:50051")
        .args(["groth16", "missing-witness", "measurements.json", "test"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("NORTHSTAR_NETWORK_PRIVATE_KEY_FILE is required"));
}

#[test]
fn preflight_requires_configuration_but_not_paid_authorization() {
    let output = Command::new(env!("CARGO_BIN_EXE_northstar-network-prover"))
        .env_clear()
        .arg("preflight")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("NORTHSTAR_NETWORK_PRIVATE_KEY_FILE is required"));
}
