# Opt-in checkpoint proof coordination

The coordinator is experimental and disabled by default. It prepares restart-durable proof jobs before the manager proposes a supported checkpoint, runs an external prover off service threads, verifies completed proofs locally, and resumes uploads using authenticated L1 state. It does not open challenges or choose bisection moves.

## Configuration

Build the validator with `agave-validator/proof-coordinator` (or `solana-core/proof-coordinator` for an embedding application). Set all three absolute paths:

- `NORTHSTAR_PROOF_JOB_DIR`: private durable directory, retained with the ledger.
- `NORTHSTAR_PROOF_PROVER`: executable supporting `preflight` and `groth16 WITNESS MEASUREMENTS PROFILE`.
- `NORTHSTAR_PROOF_CHALLENGER_KEYPAIR`: private keypair file, separate from validator identity. Only challenges belonging to this key are resumed.

`NORTHSTAR_PROOF_CHALLENGE_WINDOW_SLOTS` optionally selects the proposal's challenge window, from 10 through 9,000 slots; the experimental default is 750. Session cadence remains separately bounded by Portal. A future policy can use live validator membership and recent dispute activity; this implementation does not invent that policy. The performance target remains proof submission within two minutes, not merely before the longer protocol deadline. Existing deadlines are never extended during recovery. Validators without this opt-in retain their existing window selection.

Use an explicitly deployed Portal built with `zk-verifier-prototype` for challenged proof-resolution validation. The CPU-only `smoke` and `settlement` modes also support the default, fail-closed Portal build. Neither this feature nor the coordinator enables production acceptance. A validator built without coordinator support rejects coordinator configuration when starting its Northstar service.

## Architectural decisions

### Persist before proposing

Replay captures otherwise live in a bounded in-memory history. The coordinator extracts and natively checks every supported step, then publishes immutable checksummed jobs and an atomic, fsynced checkpoint record before permitting proposal. Unsupported traces block proposal rather than changing the frozen guest. A completed preparation survives restart without the original history. A host-only guard binds jobs to immutable session-origin fields, including creation slot, without expanding the frozen public inputs. Retirement checks matching commitments and a publication-slot fence, or a finalized newer session, rather than treating a terminal PDA alone as sufficient.

A mature ER blockhash queue also needs room in the host replay snapshot: the former 128 KiB cap rejected a measured 146,118-byte snapshot. The host cap is now 256 KiB, with a populated-queue regression. This does not change the frozen guest, public inputs, or witness schema.

### Keep proving off service threads

A dedicated thread consumes a replaceable latest-bank view and one pending preparation. Only one signed transaction occupies the outbound mailbox. Every fresh preparation requests another verified preflight before publication. Prover failure clears readiness before retry; proving does not silently fall back to another relation or backend.

Prover children receive an environment allowlist, no signing configuration, a process-group deadline, and a sampled log-size cap. Linux parent-death signaling handles validator termination. The external GPU worker remains independently supervised. Environment filtering is not a sandbox: the adapter is trusted, and same-UID processes still have ordinary filesystem access.

### Reconcile, do not trust a local upload cursor

The coordinator authenticates session, checkpoint, challenge, DA, cursor, and proof accounts before selecting a create/write/seal/resolve action. It checks existing byte prefixes and sealed hashes. Cached proofs are cryptographically reverified after restart. Uploads and settlement do not depend on GPU readiness: an already completed proof must remain usable when the worker is unavailable. Expired turns, another challenger, changed bindings, or inconsistent uploads do not produce transactions. Failed transactions pause that job rather than repeatedly spending fees on a deterministic rejection.

Recovery retains the finalized snapshot boundary. Durable jobs do not restore missing L1 state and do not make pre-finalized ledger rollback safe. Manager settlement remains separate and consumes the existing durable settlement plan.

## Local recovery harness

`northstar/scripts/live-proof-coordinator.sh` supports:

- `smoke`: fresh delegation, a real ER RPC transaction, native witness preparation, and automatic proposal. Uses a preflight-only adapter; this is not GPU evidence.
- `settlement`: the same GPU-independent flow, followed by a finalized snapshot fence, validator termination, and snapshot-only restart. A read-only observer checks automatic unchallenged settlement, bond release, account data and lamports, and the cleared checkpoint cursor. The adapter rejects proof requests; no challenge is opened.
- `proving`: the driver opens the challenge and reveals the isolated runtime step, then exits. Fence on a finalized snapshot, start GPU work, terminate the validator, and observe manager-owned recovery.
- `upload`: fence after an initial upload chunk, terminate the validator and GPU worker, and resume the remaining upload from L1 without another proof request or GPU preflight.

The GPU crash modes (`proving` and `upload`) require `agave-validator/proof-coordinator-test-hooks`. The CPU-only `settlement` mode needs only `agave-validator/proof-coordinator`. That separate feature can pause uploads using `NORTHSTAR_PROOF_TEST_UPLOAD_FENCE` and records resolver input snapshots for offline checks. It is not needed for ordinary coordination. The proving fixture uses an external adapter gate to establish the snapshot fence before releasing GPU work; this is deliberate test control, not a deadline extension.

Set `NORTHSTAR_LIVE_PORTAL_SBF`, `NORTHSTAR_LIVE_OWNER_SBF`, `NORTHSTAR_GPU_SERVER_WRAPPER`, and `NORTHSTAR_GPU_PROVER` to the explicit local artifacts. `NORTHSTAR_COORDINATOR_CONTENTION=1` adds six serial GPU proofs while the validator and freshly delegated ER session are running. The transaction is submitted afterward so this auxiliary workload cannot consume its challenge deadline.

Only the harness's `public/` directory is eligible for retained evidence. Its parent includes signing keys and private job state and must not be published.

## GPU-independent validation

No GPU or external prover is needed for these checks:

```sh
cargo test -p solana-runtime test_er_
cargo test -p northstar --features proof-coordinator --lib
```

The bank tests require successful transfers while checking L1 replay parity and isolation between ER sessions across epoch rotation. The coordinator tests reopen durable jobs and cryptographically verify retained proofs on the CPU. With an unavailable prover, they cover creating a missing upload, resuming empty and partial uploads, sealing a complete upload, and resolving a sealed proof. The manager tests also cover persisted settlement plans and finalized-checkpoint gating.

For a live local check, build the test validator with `agave-validator/proof-coordinator`, deploy explicit Portal and replay-owner SBF artifacts, and run `northstar/scripts/live-proof-coordinator.sh smoke`. This checks fresh delegation, confirmed ER account visibility, execution, and automatic checkpoint proposal using a preflight-only adapter. It does not prove a new transaction, restart the validator, or validate end-to-end settlement. Use `settlement` instead of `smoke` to exercise snapshot-fenced restart and unchallenged settlement without a GPU. This does not validate challenged proof resolution. Follow the [build environment guidance](agave-4.4-upgrade.md#build-environment) when system RocksDB overrides are present.

### Broader CPU regression suites

```sh
cargo test -p solana-svm --lib
cargo test -p solana-runtime --lib
```

The full runtime suite includes upstream snapshot tests using registered io_uring buffers. Those tests need sufficient locked-memory allowance (`ulimit -l`), not just free RAM. An 8 MiB hard limit rejects a single 16 MiB buffer registration with `ENOMEM`; serial execution does not fix that limit. Provision an appropriately sized test environment rather than changing production snapshot defaults or suppressing these tests. The local test validator uses unregistered buffers, so the live snapshot-restart check can still run under that limit.

Portal's default tests and retained-proof verifier tests are also CPU-only. Build their SBF artifacts separately and pass an absolute `BPF_OUT_DIR`; the verifier suite requires `zk-verifier-prototype`, while default tests use the default build:

```sh
BPF_OUT_DIR=/absolute/path/to/default-artifacts cargo test -p northstar-portal
BPF_OUT_DIR=/absolute/path/to/prototype-artifacts cargo test -p northstar-portal \
  --features zk-verifier-prototype --test zk_verifier
```

## Validation commands

[Regression fixtures](../zkvm-replay/evidence/proof-coordinator-v1/README.md) retain only the inputs consumed by host and Portal tests. Live-run reports, lifecycle logs, and bulk performance measurements are not repository fixtures. The harness writes them to its selected output directory.

`gpu-worker-stress.py OUTPUT --requests 30` exercises an already warmed local worker using `NORTHSTAR_GPU_WORKER_SOCKET`. It records serial proving, busy rejection, cancellation, and immediate restart/retry without provisioning or starting services. Its output directory must not already exist.

Host verification also has an ignored CPU-only timing check:

```sh
cargo test -p northstar --features proof-coordinator benchmark_frozen_host_verification -- --ignored --nocapture
```

Development builds optimize only the existing BN254 arithmetic and verifier dependencies, retaining debug assertions. No verifier algorithm or release defaults change.

Portal's `zk-verifier-profile` feature emits diagnostic compute-unit markers around authenticated account loading, binding, verification, and storage. Use an uninstrumented `zk-verifier-prototype` build for the 130K CU acceptance check; marker-inclusive measurements are not interchangeable with normal costs.

These fixtures exercise the frozen supported replay profile, not arbitrary SVM transactions. Production enablement, arbitrary-challenger automation, bisection policy, and recovery before the finalized snapshot boundary remain outside scope.
