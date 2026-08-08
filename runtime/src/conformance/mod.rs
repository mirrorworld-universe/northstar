//! Solana runtime conformance harnesses.

#[cfg(feature = "conformance")]
pub mod cost;
pub mod txn;
// Sonic: deterministic Northstar proof-trace harness.
#[cfg(feature = "conformance")]
pub mod trace;
