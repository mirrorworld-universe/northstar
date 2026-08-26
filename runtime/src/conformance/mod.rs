//! Solana runtime conformance harnesses.

#[cfg(feature = "conformance")]
pub mod cost;
pub mod txn;
// Sonic: deterministic Northstar proof-trace harness.
#[cfg(feature = "conformance")]
pub mod trace;
// Sonic: bounded full-transaction proof fixture shared by proving spikes.
#[cfg(feature = "conformance")]
pub mod proof_fixture;
