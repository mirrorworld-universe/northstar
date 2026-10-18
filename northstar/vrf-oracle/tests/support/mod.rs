#![allow(dead_code)]
use {
    northstar_vrf_oracle::{Metadata, Mode, OracleKey, Policy, Request, SUITE, VrfResult},
    serde::Deserialize,
    zeroize::Zeroizing,
};

#[derive(Deserialize)]
pub struct Vector {
    pub public_test_scalar: String,
    pub public_key: String,
    pub alpha: String,
    pub proof: String,
    pub output: String,
}

impl Vector {
    pub fn load() -> Self {
        serde_json::from_str(include_str!("../fixtures/c2sp.json")).unwrap()
    }
    pub fn result(&self) -> VrfResult {
        VrfResult {
            proof: self.proof.clone(),
            output: self.output.clone(),
        }
    }
}

pub fn array(value: &str) -> [u8; 32] {
    hex::decode(value).unwrap().try_into().unwrap()
}

pub fn key() -> OracleKey {
    OracleKey::from_scalar(Zeroizing::new(array(&Vector::load().public_test_scalar))).unwrap()
}

pub fn other_key() -> OracleKey {
    let mut scalar = [0; 32];
    scalar[0] = 1;
    OracleKey::from_scalar(Zeroizing::new(scalar)).unwrap()
}

pub fn metadata() -> Metadata {
    Metadata {
        version: 1,
        mode: Mode::OfflineFixtureV1,
        suite: SUITE.into(),
        chain: [1; 32],
        program: [2; 32],
        application_fixture_digest: [3; 32],
        consumer_fixture_digest: [4; 32],
    }
}

pub fn policy() -> Policy {
    Policy {
        max_attempts: 3,
        retry_delay: 2,
        pending_timeout: 5,
    }
}

pub fn request() -> Request {
    Request {
        metadata_digest: metadata().digest().unwrap(),
        id: [5; 32],
        public_key: array(&Vector::load().public_key),
        input: b"PUBLIC offline request; no on-chain ABI".to_vec(),
        context_digest: [6; 32],
        not_before: 10,
        expires_at: 1000,
    }
}
