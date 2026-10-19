//! Experimental offline CPU oracle. No Solana adapter or agreed application ABI.

mod binding;
mod crypto;
mod journal;

pub use {
    binding::{Metadata, Mode, Policy, Request, SUITE},
    crypto::{OracleKey, VrfResult, verify},
    journal::{Attempt, Ingest, Oracle, Status},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("incompatible or missing offline application metadata")]
    Metadata,
    #[error("invalid request or retry policy")]
    Request,
    #[error("invalid secret scalar")]
    Secret,
    #[error("request/key binding mismatch")]
    Key,
    #[error("invalid VRF proof, encoding, or output")]
    Proof,
    #[error("request identifier conflicts with its frozen binding")]
    Conflict,
    #[error("unknown request")]
    Unknown,
    #[error("confirmation or failure does not match the pending attempt")]
    Receipt,
    #[error("logical clock moved backwards")]
    Clock,
    #[error("journal is incompatible or inconsistent")]
    Journal,
    #[error("journal operation failed; reopen before continuing")]
    Poisoned,
    #[error("journal already has a writer")]
    Locked,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
