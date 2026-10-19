use {
    crate::{Error, Result},
    serde::{Deserialize, Serialize},
    sha2::{Digest, Sha256},
    vrf_r255::PublicKey,
};

pub const SUITE: &str = "C2SP-ECVRF-RISTRETTO255-SHA512/vrf-r255-0.1.0";
const DOMAIN: &[u8] = b"northstar/vrf-oracle/offline-v1\0";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Mode {
    OfflineFixtureV1,
    LiveSolana,
}

/// These digests identify local fixtures, not an on-chain instruction format.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub version: u32,
    pub mode: Mode,
    pub suite: String,
    pub chain: [u8; 32],
    pub program: [u8; 32],
    pub application_fixture_digest: [u8; 32],
    pub consumer_fixture_digest: [u8; 32],
}

impl Metadata {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.mode != Mode::OfflineFixtureV1
            || self.suite != SUITE
            || [
                self.chain,
                self.program,
                self.application_fixture_digest,
                self.consumer_fixture_digest,
            ]
            .contains(&[0; 32])
        {
            return Err(Error::Metadata);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<[u8; 32]> {
        self.validate()?;
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        hash.update(self.version.to_be_bytes());
        hash.update(SUITE.as_bytes());
        hash.update(self.chain);
        hash.update(self.program);
        hash.update(self.application_fixture_digest);
        hash.update(self.consumer_fixture_digest);
        Ok(hash.finalize().into())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub max_attempts: u32,
    pub retry_delay: u64,
    pub pending_timeout: u64,
}

impl Policy {
    pub fn validate(&self) -> Result<()> {
        if !(1..=16).contains(&self.max_attempts)
            || !(1..=86_400).contains(&self.retry_delay)
            || !(1..=86_400).contains(&self.pending_timeout)
        {
            return Err(Error::Request);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub metadata_digest: [u8; 32],
    pub id: [u8; 32],
    pub public_key: [u8; 32],
    /// Complete, frozen public application input; no oracle-selected salt.
    pub input: Vec<u8>,
    pub context_digest: [u8; 32],
    pub not_before: u64,
    pub expires_at: u64,
}

impl Request {
    pub fn validate(&self, metadata: &Metadata) -> Result<()> {
        if self.metadata_digest != metadata.digest()? {
            return Err(Error::Metadata);
        }
        if self.id == [0; 32]
            || self.context_digest == [0; 32]
            || self.input.is_empty()
            || self.input.len() > 4096
            || self.expires_at <= self.not_before
            || self.expires_at - self.not_before > 86_400
            || PublicKey::from_bytes(self.public_key).is_none()
        {
            return Err(Error::Request);
        }
        Ok(())
    }

    /// Internal fixture encoding only. Every request field enters the VRF input.
    pub fn alpha(&self, metadata: &Metadata) -> Result<Vec<u8>> {
        self.validate(metadata)?;
        let mut bytes = DOMAIN.to_vec();
        bytes.extend_from_slice(&self.metadata_digest);
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(&self.public_key);
        bytes.extend_from_slice(&self.context_digest);
        bytes.extend_from_slice(&self.not_before.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        bytes.extend_from_slice(&(self.input.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&self.input);
        Ok(bytes)
    }

    pub fn binding(&self, metadata: &Metadata) -> Result<[u8; 32]> {
        Ok(Sha256::digest(self.alpha(metadata)?).into())
    }
}
