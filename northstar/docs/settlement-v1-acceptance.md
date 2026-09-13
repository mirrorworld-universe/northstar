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
| Expected replay program key | `0x00566483d6fa2d3e348b61ce6acca85960a7e0152748ab20770add0b7d8c953f` |
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
| Replay account values can be recovered from ER history | Partial | `ErHistoryStore` retains bounded immutable pre/post account captures for checkpoint transactions; captures follow RPC commitment and retention rules |
| Complete supported `ReplayWitnessV1` is extracted from ER history | Missing | Program ELF/loader state, committed runtime inputs, and canonical VM trace are not yet persisted or deterministically reconstructed from history |
| Extracted witness reproduces all eight checkpoint public inputs | Missing | Existing checkpoint-bound fixture reproduces the fields, but it is not yet built from an ER-history extraction pipeline |
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

## Known protocol gap: low traffic

L1 cadence is enforced as an earliest proposal slot. Runtime sealing still requires exactly 16 successful ER transactions and pauses admission after sealing. Therefore a session with fewer than 16 successful transactions does **not** produce a time-driven checkpoint merely because the L1 interval elapsed. Tests must not manufacture transactions or redefine the canonical 16-step checkpoint to hide this gap. Product/protocol policy must explicitly choose how partial low-traffic intervals are handled.

## Acceptance ownership

- Implementer: run all GPU-free commands, publish exact commit and fixture hashes, and report failures without substituting narrower checks.
- Independent maintainer/reviewer: reproduce the live bisection and GPU proof checks from a clean checkout.
- Protocol owner: decide the low-traffic partial-checkpoint policy and approve any production-verifier default enablement.
- Infrastructure owner: authorize paid CUDA or proving-service use. No paid run is implied by this document.
