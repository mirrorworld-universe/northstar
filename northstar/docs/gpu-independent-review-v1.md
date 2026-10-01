# Independent GPU checkpoint review — Jonas handoff

**Status: awaiting independent review, not production acceptance.** No deployment, default-verifier enablement or rollout approval is implied.

## Pin and scope

Review/reproduce source commit [`58aa973959d608b7723e67f848c8272bd13c49e8`](https://github.com/mirrorworld-universe/northstar/tree/58aa973959d608b7723e67f848c8272bd13c49e8). It includes the runner, userspace recipe, real resolver checks and all evidence below. Review discussion is in [PR194](https://github.com/mirrorworld-universe/northstar/pull/194).

The retained guest ELF and candidate manifest are authoritative. Do not rebuild the guest and silently adopt a new key: build paths affect ELF bytes. SP1 SDK is 6.1.0; expected program key is `0x0050535e1d6450ca9f99ea6ac433acc14e0defb4795ee36f3e8fb375155f9e6c`. The preserved ELF SHA-256 is `23f1520bf46ab8852770c0f4802005c8cb88b7fa6657f99f2d7c4a948d1551c5`.

## Existing evidence to inspect

| Evidence directory under `northstar/zkvm-replay/evidence` | Demonstrated result |
| --- | --- |
| `l40s-v1` | Three consecutive live canonical proofs: 67.814/68.957/67.246s prove+wrap, 127,977 CU resolver, roughly 122–125s challenge-to-outcome; partial fixture also passes |
| `recovery-v1.json` | Service Pending, zero-effect, challenged and post-settlement crash drills; ER slot floor and next exact payout |
| `hardened-runner-v1` | Bounded SSH adapter through real resolution |
| `combined-settlement-v1` | GPU resolution → natural deadline → commit/bond release → 16-account data settlement; snapshot-fenced mid-settlement restart and durable-plan replay |
| `upload-recovery-v1` | Restart after first 128-byte proof chunk, account conservation, resumed upload/resolution/settlement |
| `resolver-v1` | Captured public account state for 31 resolver and 10 proof-creation/path invariant cases |
| `userspace-v1` | Fresh private-prefix build, nine privately resolved Icicle libraries, 81.008s proof and 329ms SDK verification |

The cold compatibility proof took 190.803s and **did not meet** the 120s target. Warm measurements must not be presented as empty-cache results. No unmeasured phase should be reported as zero. See [GPU validation](gpu-proof-validation-v1.md), [combined recovery](gpu-proof-to-settlement-v1.md), [resolver checks](resolver-bindings-v1.md) and [userspace setup](gpu-userspace-provisioning-v1.md) for exact boundaries and per-phase artifacts.

## CPU-only reproduction

Use the pinned Rust/Solana build environment. No GPU, wallet, external RPC or paid proving service is needed for these checks:

```sh
git switch --detach 58aa973959d608b7723e67f848c8272bd13c49e8
export CARGO_TARGET_DIR="$PWD/target"
for name in l40s-v1 hardened-runner-v1 combined-settlement-v1 upload-recovery-v1 resolver-v1 userspace-v1; do
  (cd "northstar/zkvm-replay/evidence/$name" && sha256sum -c SHA256SUMS)
done
cargo clippy --locked --all --tests -- -D warnings
cargo +nightly-2026-07-20 fmt --all -- --check
python3 -B northstar/scripts/test_gpu_prover.py
bash northstar/scripts/test-gpu-provision.sh
cargo test --locked -p northstar
cargo test --locked -p northstar-portal --features zk-verifier-prototype --lib
env -u RUSTC -u RUSTDOC cargo build-sbf \
  --manifest-path northstar/programs/portal/Cargo.toml \
  --features zk-verifier-prototype -- --locked
BPF_OUT_DIR="$CARGO_TARGET_DIR/deploy" cargo test --locked \
  -p northstar-portal --features zk-verifier-prototype --test zk_verifier
```

Expected: five Python runner tests, provisioning refusal guards, 167 Northstar tests (three ignored live/export tests), 21 prototype library tests, and seven SBF tests (one ignored export test). The SBF suite verifies 11 retained proofs, changed proof/public fields, and the resolver/path cases. ProgramTest clock setup is not a new live wall-clock measurement.

## Authorized GPU reproduction

Use an already authorized L40S host. The tested userspace configuration is documented in [setup](gpu-userspace-provisioning-v1.md); existing compatible driver/toolchain prerequisites are required. Run a compatibility proof before timed challenges. Retain cold-start measurements separately.

On the machine running the local test validator, build the validator **before** the explicit prototype SBF; later host builds can replace bundled SBF placeholders:

```sh
cargo build --locked --bin solana-test-validator
env -u RUSTC -u RUSTDOC cargo build-sbf \
  --manifest-path northstar/programs/portal/Cargo.toml \
  --features zk-verifier-prototype -- --locked
export NORTHSTAR_LIVE_PORTAL_SBF="$CARGO_TARGET_DIR/deploy/northstar_portal.so"
export NORTHSTAR_LIVE_PROVER="$PWD/northstar/scripts/gpu-prover-ssh.sh"
export NORTHSTAR_GPU_SSH='<authorized-user>@<authorized-host>'
export NORTHSTAR_GPU_REMOTE_RUNNER='/absolute/private-prefix/bin/gpu-prover'
export NORTHSTAR_GPU_REMOTE_ARTIFACTS='/absolute/new-evidence-parent'
for run in 1 2 3; do
  bash northstar/scripts/live-gpu-settlement.sh settle
done
bash northstar/scripts/live-gpu-settlement.sh crash-upload
bash northstar/scripts/live-gpu-settlement.sh crash-settling
```

The remote artifact parent must exist. The scripts allocate new evidence/ledger directories, refuse occupied endpoints and clean up their validator sessions. Do not reuse ledgers. Read each printed evidence directory; record phase durations, restart fences and terminal account state. The three new combined runs above would be independent reproduction, not runs already claimed in the retained canonical timing series.

## Reviewer decision record

Record reviewer/date, exact commit, machine/toolchain, cache state and links to fresh evidence. Check independently:

- [ ] Artifact hashes, frozen key and 356-byte/256-byte ABI match.
- [ ] Real SBF accepts all retained proofs; named rejected cases preserve account state and invalid-proof outcomes conserve/slash the exact bond.
- [ ] Three consecutive fresh proofs each take ≤120s prove+wrap; successful full resolver stays ≤130,000 CU. Escalate at 150,000 CU rather than weakening limits.
- [ ] Each challenge reaches its outcome within ten minutes; no live slot warps or unmeasured phases disguised as zero.
- [ ] Settlement and snapshot-fenced restart results agree with the recorded receipt/cursor/bond invariants.
- [ ] Scope exclusions are acceptable for this milestone; separate explicit rollout review remains required.

**Not demonstrated:** arbitrary-crash L1 durability; fresh delegation CPI in the combined GPU fixture; automatic service ownership/recovery of that combined fixture; cold-cache ≤120s; independent production acceptance. The combined fixture uses genesis delegation accounts, data-only settlement with unchanged lamports/owner, and a surviving test driver. Do not relabel it as a full production lifecycle. Deployment also requires Bridge/client account-list compatibility and a policy for older delegations without receipt origins.
