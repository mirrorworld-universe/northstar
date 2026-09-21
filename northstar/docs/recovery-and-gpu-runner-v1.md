# Snapshot-fenced recovery and bounded GPU runner

## Recovery boundary

The recovery drills use real Portal SBF and SIGKILL the local validator, then restart the same ledger. No slot warps or protocol deadline changes are used.

| Mode | Observed result | Transaction-to-outcome wall time |
| --- | --- | ---: |
| `crash` | Pending withdrawal checkpoint survives restart; settles and releases bond; a second 500,000-lamport withdrawal settles exactly once | 110.615s |
| `crash-no-effects` | Pending zero-effect checkpoint settles after restart; receipt unchanged; ER admission resumes | 51.023s |
| `crash-challenged` | Challenge survives restart; natural respondent timeout invalidates checkpoint and slashes bond; receipt unchanged and active cursor cleared | 46.120s |
| `crash-settled` | Completed withdrawal and released bond survive snapshot-fenced restart; receipt unchanged; next withdrawal settles exactly once | 113.111s |

`crash-settled` waits for a finalized settlement and an on-disk full L1 snapshot at or beyond that finalized slot (observed fence 161, snapshot 200). Restart uses the existing test-validator load-only snapshot mode. This is deliberately **not arbitrary-crash L1 durability**: an unfenced run restored snapshot 100 without the later settlement, while its local checkpoint plan had already been removed. Broader L1 persistence and plan-retention changes remain outside this boundary.

The fenced regression exposed an independent Northstar issue: a fresh ER clock derived from the L1 slot could be lower than a previously settled ER slot. The next proposal failed with `CheckpointErSlotNotAdvanced`. Session preparation now floors the fresh ER bank above both the persisted settled slot and active checkpoint slot. The same fenced test then passed, including receipt conservation and the next exact payout. A non-ignored runtime regression checks the slot floor in CI.

Run from the repository root with a built default `solana-test-validator` and default Portal SBF:

```sh
bash northstar/scripts/live-cadence-recovery.sh crash
bash northstar/scripts/live-cadence-recovery.sh crash-no-effects
bash northstar/scripts/live-cadence-recovery.sh crash-challenged
bash northstar/scripts/live-cadence-recovery.sh crash-settled
```

Requires tmux, curl, jq, an existing local Solana payer, and unused RPC ports 18999/8910. `CARGO_TARGET_DIR` selects existing build artifacts. `NORTHSTAR_EVIDENCE_DIR` selects an existing parent for fresh evidence directories. The runner compiles the test before starting the timing/readiness waits, waits past genesis slot zero, exits early on test failure, and cleans up its tmux sessions. Sanitized observations are retained in [`recovery-v1.json`](../zkvm-replay/evidence/recovery-v1.json).

## GPU adapter contract

`NORTHSTAR_LIVE_PROVER` must implement `preflight` and `groth16 WITNESS MEASUREMENTS PROFILE`. The live test runs preflight **before opening a session**, refuses an existing output directory, and bounds adapter execution with GNU `timeout` (240s preflight, 300s proving bridge, 5s termination grace). These process bounds do not relax the separate 120s prove-and-wrap acceptance limit or 10-minute challenge limit.

`northstar/scripts/gpu-prover.py` runs on the GPU host and requires a prebuilt pinned-ELF SP1 prover, installed CUDA runtime/GPU server and Icicle libraries. It does not install drivers, rent machines, use paid proving, or rebuild the guest. Configure `NORTHSTAR_GPU_REPLAY_DIR` and `NORTHSTAR_GPU_PROVER` if not using their repository-local defaults. Export the host's `LD_LIBRARY_PATH` and `ICICLE_BACKEND_INSTALL_DIR` through an executable wrapper.

Preflight checks the fixture SHA-256, NVIDIA availability and the embedded program key against the candidate manifest. Key setup is not a full cold Groth16 warm-up; prepare circuits and do a compatibility proof before a timed challenge. Proving always sets `SP1_PROVER=cuda`, refuses existing proof artifacts, enforces a 180s subprocess bound, and checks key/public-input/envelope/measurement consistency. Timeout or interruption terminates the whole subprocess group, including descendants, with a five-second grace before SIGKILL. Success still relies on the SDK's cryptographic verification and real Portal verification; metadata checks alone are not proof verification.

For SSH, select the checked-in `northstar/scripts/gpu-prover-ssh.sh` as `NORTHSTAR_LIVE_PROVER` and set:

- `NORTHSTAR_GPU_SSH`: explicitly authorized SSH destination.
- `NORTHSTAR_GPU_REMOTE_RUNNER`: absolute executable wrapper path on that host.
- `NORTHSTAR_GPU_REMOTE_ARTIFACTS`: existing absolute directory with simple shell-safe path characters.

The bridge uses batch-mode SSH, connection/transfer/command bounds, quoted arguments and a fresh remote directory per proof. Remote evidence is retained on failure for diagnosis; clean it up explicitly after review. Configure SSH credentials beforehand; neither adapter handles signer keys.

The hardened SSH adapter passed a fresh real checkpoint proof/resolution run: **123.382s challenge-to-outcome, 127,976 CU**. Python CPU-only tests cover timeout, descendant cleanup, child failure, retained measurement consistency and changed fields. CI runs these without GPU access.

The [combined fixture](gpu-proof-to-settlement-v1.md) now follows a GPU-resolved checkpoint through data settlement, batch replay and snapshot-fenced mid-settlement SIGKILL recovery. It uses explicit genesis delegation fixtures and a surviving test driver, not fresh delegation or automatic service recovery. Snapshot-fenced partial-upload SIGKILL recovery also passes through subsequent resolution and settlement. The [named resolver/path invariants](resolver-bindings-v1.md) and [private-prefix GPU setup](gpu-userspace-provisioning-v1.md) now pass. Independent acceptance remains required; arbitrary-crash L1 durability and the other explicit fixture boundaries are not covered. The production verifier remains disabled by default.
