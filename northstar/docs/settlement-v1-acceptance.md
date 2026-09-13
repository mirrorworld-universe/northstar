# Settlement v1 acceptance matrix

Status: **GPU-free implementation and coverage are substantial; production proof acceptance is incomplete**.

This runbook separates reproducible CPU/SBF checks from proof-generation checks that require CUDA hardware or an authorized proving service. Passing the CPU section does not authorize enabling the production verifier.

## Versioned inputs

| Input | Value |
|---|---|
| Replay witness encoding | v2 |
| Proof kind / version | full transaction `2` / `1` |
| Canonical checkpoint size | 16 successful ER transactions |
| SP1 SDK | 6.1.0 |
| Replay fixture | `northstar/zkvm-replay/fixture-v1.bin` |
| Replay fixture SHA-256 | `952f20bfa1d3e3f7eae8e5b2d05bd7a623e9fb12d67ab6667a42c26af3dc8a80` |
| Preserved baseline replay program key (not validated for the partial-checkpoint guest) | `0x00566483d6fa2d3e348b61ce6acca85960a7e0152748ab20770add0b7d8c953f` |
| Portal proof envelope | 356 bytes |
| Portal public inputs | 256 bytes, eight canonical BN254 fields |

Do not modify the replay fixture before producing the compatibility artifact. A guest or fixture change can change the replay ELF and program key.

## Acceptance matrix

| Criterion | Status | Reproducible evidence |
|---|---|---|
| Canonical checkpoint contains 16 ordered successful ER transactions | Pass | `canonical_checkpoint_uses_sixteen_executed_er_transactions` |
| Three independent executions produce identical checkpoint and DA bytes | Pass | Same test performs three fresh Bank/client executions; fixed genesis time removes fixture-only wall-clock input |
| Canonical page paths and roots reject missing, reordered, or changed data | Pass | `checkpoint` unit tests and live checkpoint mutation simulations |
| Real captured checkpoint reaches one disputed transaction after four bisection rounds | Pass with scope limit | `real_checkpoint_bisects_to_captured_transaction`; ER executes in a local Bank/client while Portal executes on `solana-test-validator` |
| Replay account values can be recovered from ER history | Partial | `ErHistoryStore` retains up to 256 immutable in-memory pre/post account captures for checkpoint transactions; captures follow RPC commitment and slot-retention rules |
| Supported SBF transactions can seal a real ER-client checkpoint with replay captures | Pass with scope limit | Paid and gasless `sbf_checkpoint` tests execute the supported account-write program |
| Live bisection fixture transactions are supported by the current replay relation | Pass | Live harness now executes 16 gasless SBF account-write transactions, isolates step 10 through Portal, then extracts its witness from finalized ER history |
| Default gasless ER fee policy is supported by the current replay relation | Pass | `gasless_execution_replays_without_changing_the_relation`; replay v2 checks the configured fee, not a fixed 5,000-lamport fee |
| Complete supported `ReplayWitnessV1` is extracted from ER history | Pass with scope limit | Feature `replay` exposes `extract_replay_witness_v1`; reconstructs VM traces from bounded runtime snapshots and checks committed accounts, units, loaded-data size, and fees. L1 context and build provenance are caller-supplied |
| Extracted witness reproduces all eight checkpoint public inputs | Pass with scope limit | Paid and gasless ER-client tests reject mutations to every field. Live Portal bisection checks all 256 bytes using on-chain session/challenge state and authenticated checkpoint data |
| Portal authenticates trace, transaction effect, readonly L1, settlement effects, and session context | Pass in implementation/tests | Portal integration and replay mutation tests |
| Real unchanged SP1 Groth16 proof passes direct Portal SBF verification | Blocked | No checkpoint-bound 356-byte proof artifact has been generated |
| Successful direct verification uses at most 130K CU | Blocked | 97,156 CU is only a full-path failing-proof measurement, not successful verification |
| Production `ResolveChallenge` accepts a real proof three times | Blocked | Requires the real proof artifact; default verifier remains fail-closed |
| No checkpoint proposal before configured L1 cadence; proposal occurs at due slot | Pass | `validator_checkpoint_flow_waits_then_settles` |
| Retry cannot create a second active checkpoint or lock a second proposer bond | Pass | `checkpoint_rejects_second_active_proposal` |
| Pending/challenged checkpoint blocks early commit, settlement, and close | Pass | Portal deadline, DA timeout, and validator checkpoint-flow tests |
| Respondent and challenger timeout outcomes are covered | Pass | `checkpoint_da_timeout_slashes_and_allows_recovery` and `challenge_bisects_to_one_step_and_challenger_timeout_restores_checkpoint` |
| Invalid checkpoint can be replaced at the same ER slot | Pass | DA-timeout recovery test |
| Finalized checkpoint roots remain monotonic | Pass | `checkpoint_proposal_commit_deadline_flow` rejects a stale previous root |
| Settlement effects are exactly once under retries | Pass | Validator post-settlement no-op assertion plus `test_portal_settlement_retry_after_applied_ops_is_idempotent` |
| Three complete challenged checkpoints finish within ten minutes including proving | Blocked | Requires three independent real proofs and timing capture |

## GPU-free reproduction

Run from repository root:

```bash
cargo +nightly-2026-07-20 fmt --all
git diff --check
cargo clippy --all --tests
cargo test -p solana-rpc
BPF_OUT_DIR="$PWD/target/deploy" cargo test -p northstar
```

Build default fail-closed Portal SBF before the default integration suite. If `cargo build-sbf` selects the host compiler, put Solana platform-tools Rust first in `PATH`.

```bash
PATH="$HOME/.cache/solana/v1.56/platform-tools/rust/bin:$PATH" \
  cargo build-sbf --manifest-path northstar/programs/portal/Cargo.toml
BPF_OUT_DIR="$PWD/target/deploy" \
  cargo test -p northstar-portal --test integration
```

The live bisection harness requires a fresh local ledger and no paid services:

```bash
tmux new-session -d -s northstar-live-checkpoint \
  "RUST_LOG=warn target/debug/solana-test-validator \
   --ledger /tmp/northstar-live-checkpoint-acceptance \
   --rpc-port 18999 --bind-address 127.0.0.1 \
   > /tmp/northstar-live-checkpoint-validator.log 2>&1"

# Poll getHealth until ready, then:
NORTHSTAR_LIVE_RPC_URL=http://127.0.0.1:18999 \
  cargo test -p northstar \
  real_checkpoint_bisects_to_captured_transaction -- --ignored

tmux kill-session -t northstar-live-checkpoint
```

Use a new ledger path for each live run. The default payer is `~/.config/solana/id.json`; `NORTHSTAR_LIVE_PAYER` can override it.

## Proof-dependent reproduction

An independent maintainer must generate the proof on one CUDA machine or through an authorized proving service, record hardware and phase timings, and preserve the generated bytes unchanged for Portal checks.

```bash
cd northstar/zkvm-replay
export SP1_PROVER=cuda
cargo run --release \
  -p northstar-zkvm-replay-script \
  --features cuda -- \
  groth16 fixture-v1.bin sp1-gpu-measurements.json checkpoint-v2-gpu
```

Required outputs:

- `northstar-sp1-groth16-onchain.bin`: exactly 356 bytes;
- `northstar-sp1-public-inputs.bin`: exactly 256 bytes;
- `sp1-gpu-measurements.json`: expected program key, hardware, setup, proving, wrapping, and SDK verification results.

Then build Portal with `zk-verifier-prototype` and run the ignored real-proof compatibility test three times. Each unchanged proof must pass at or below 130K CU. Changed proof bytes must fail. Next, commit immutable test vectors and run the production `ResolveChallenge` flow three times before considering default feature enablement.

## Low-traffic checkpoint policy

When settlement is due and no checkpoint is active, the runtime seals 1–16 actual successful transactions. Empty intervals are skipped; full batches still seal at 16. Admission stays blocked after sealing until settlement consumes the artifact. Existing forced-undelegation scheduling is preserved. Outstanding challenges do not permit overlapping proposals.

Artifact and replay regression tests cover sizes 1, 2, 3, 15, and 16. Real ER-history extraction covers partial SBF batches; admission tests verify empty-skip, immutable sealing, and resumption. The canonical 16-step/four-round benchmark is unchanged. Full live partial-count bisection and restart/recovery acceptance remain outstanding.

## Acceptance ownership

- Implementer: run all GPU-free commands, publish exact commit and fixture hashes, and report failures without substituting narrower checks.
- Independent maintainer/reviewer: reproduce the live bisection and GPU proof checks from a clean checkout.
- Protocol owner: decide the low-traffic partial-checkpoint policy and approve any production-verifier default enablement.
- Infrastructure owner: authorize paid CUDA or proving-service use. No paid run is implied by this document.

## History-derived replay (GPU-free)

Build the host extraction API with `cargo check -p northstar --features replay`.
Run `cargo test -p northstar sbf_checkpoint_` and
`cargo test -p northstar-transaction-proof --test gasless_replay`.

Supported legacy account-write transactions retain a versioned runtime snapshot before execution:
account/program/loader/sysvar values, active/inactive features, blockhash queue, signature-fee
policy, processing age, and total epoch stake. Snapshot size is capped at 128 KiB; snapshots
share the existing 256-entry, commitment-aware in-memory capture retention. Oversized or
unsupported inputs do not gain a replay snapshot. This is not restart-durable proof storage.

Extraction requires finalized history, a verified checkpoint artifact, authenticated expected
public inputs, and caller-supplied L1 session context/build provenance. It re-executes using
the captured fee policy, compares committed results, replaces fixture checkpoint bindings
with the actual DA paths, and runs the unchanged replay relation. Missing/corrupt snapshots,
changed account values or fees, and public-input mismatches return errors.

The original checked-in GPU compatibility fixture remains unchanged. Its synthetic surrounding
checkpoint leaves are not used as the extracted witness's final checkpoint binding.

## Remaining September delivery sequence

1. Completed GPU-free O1 integration: supported-workload extraction follows four live Portal bisection rounds. The local run passed in 8.91 seconds; this is not an end-to-end proof-resolution timing.
2. Prepare independent maintainer reproduction and measure GPU-free timeout/recovery phases.
3. When CUDA is authorized and available, run the unchanged real-proof compatibility route,
   then production-resolution acceptance and three end-to-end timings.
4. Complete live partial-checkpoint bisection and restart/recovery acceptance; the runtime/artifact/replay implementation is present, but those integration gates remain.

Measured GPU-free phase timings and terminal timeout retry invariants are recorded in
[checkpoint CPU timing evidence](checkpoint-cpu-timings-v1.md). Proof, verification,
and recovery timing fields remain explicitly missing; O3 finality acceptance is not complete.

## Partial-checkpoint candidate compatibility gate

The replay guest relation now accepts authenticated step counts 1–16. The eight-field
ABI and checked-in fixture bytes are preserved, but the guest source has changed.
The baseline program key above must not be treated as the updated candidate's key.
Rebuild and fingerprint the guest, update the candidate's verifier binding, and repeat
compatibility acceptance before production enablement. No new program key or
real-proof compatibility result is claimed by the partial-checkpoint tests.
