# L40S proof validation — September 14, 2026

Implementer measurements, not independent acceptance or rollout approval.

Base source: `19200c46a66b59eba84c3e3447027b3e10f53bce`, with the live harness bridge in this change. SP1 SDK 6.1.0, guest rustc 1.93.0-dev. Hardware: AWS g6e.4xlarge, one NVIDIA L40S (46,068 MiB reported), 124 GiB host RAM, Ubuntu 26.04, driver 595.91.07. CUDA toolkit 13.1; the SP1 GPU server additionally requires CUDA 12 runtime (`nvidia-cuda-runtime-cu12` 12.9.79).

## Results

| Case | Steps / selected | Prove + wrap | Resolver CU | Challenge to outcome |
|---|---|---|---|---|
| Fresh canonical 1 | 16 / 10 | 67.814s | 127,977 | 124.373s |
| Fresh canonical 2 | 16 / 10 | 68.957s | 127,977 | 124.292s |
| Fresh canonical 3 | 16 / 10 | 67.246s | 127,977 | 122.377s |
| Fresh partial | 3 / 2 | 67.461s | 127,976 | 120.147s |

Each case generated a fresh proof for its exported history-derived witness. SDK verification passed; all 256 public bytes matched the isolated checkpoint. Each submitted `ResolveChallenge(Production)` to the real prototype Portal SBF and asserted Pending / ValidatorWon. Canonical cases used four rounds, partial used two; no slot warps or deadline changes. The prover process and SSH transfers took roughly 113–116s, including initialization and setup; this is distinct from the SDK's prove-and-wrap measurement. Setup was approximately 11.6s; upload approximately 2.6–2.7s.

The unchanged compatibility fixture also passed direct Portal SBF verification at **97,114 CU**, including rejection of a changed proof byte. The first cold compatibility proof took **190.803s**, including initial circuit acquisition in the proving path, and exceeds the 120s target. The subsequent compatibility proof took **67.794s**. The three canonical measurements above used preinstalled circuits; cold-start performance is not accepted.

## Identity gate

All accepted runs used ELF SHA-256 `23f1520bf46ab8852770c0f4802005c8cb88b7fa6657f99f2d7c4a948d1551c5` and program key `0x0050535e1d6450ca9f99ea6ac433acc14e0defb4795ee36f3e8fb375155f9e6c`. The fixture retained SHA-256 `952f20bfa1d3e3f7eae8e5b2d05bd7a623e9fb12d67ab6667a42c26af3dc8a80`. CPU setup and execution of the preserved ELF reproduced the key and 23,797,920 cycles.

A clean guest build on the GPU host produced a different ELF/key. Both binaries contain absolute source paths; rebuilding in another checkout is not currently reproducible. Validation therefore used the preserved, hash-checked candidate ELF with `SP1_SKIP_PROGRAM_BUILD=true`, not a changed verifier pin. A portable, pinned ELF distribution or reproducible-build procedure remains necessary for independent reproduction.

Icicle's build required a temporary header overlay because CUDA 13.1 and glibc 2.43 disagree about `rsqrt` declarations. No system header or Northstar relation was changed. Toolchain provisioning needs a reproducible supported-host recipe before handoff.

## Harness interface and remaining work

`NORTHSTAR_LIVE_PROVER` selects an executable using the replay script's positional interface: `groth16 WITNESS MEASUREMENTS PROFILE`. It runs with the new, absolute `NORTHSTAR_LIVE_PROOF_DIR` as working directory and must write the standard proof/public-input artifacts there. An SSH wrapper may implement this interface; it must retain remote evidence and return only after proof generation succeeds. The test retains its signer in memory throughout proving.

The explicit prototype SBF must be loaded at the harness Portal address. Bundled default Portal remains fail-closed. With the current validator, setting `--portal` to a different address and using `--bpf-program` at the harness address avoids the bundled default program replacing the explicit prototype. ER execution remains a local Bank/client, not the validator's ER service.

Remaining before complete acceptance:

- Complete portable GPU provisioning. Preflight, process-group timeout, measurement checks, SSH bounds and CPU-only runner regressions are now implemented; see [recovery and runner follow-up](recovery-and-gpu-runner-v1.md).
- Add full proof/public-field/path/metadata mutation coverage and state-preservation assertions to the real resolver route.
- Extend [combined proof-to-settlement recovery](gpu-proof-to-settlement-v1.md) beyond genesis delegation fixtures and the snapshot-fenced boundary. Resolution conservation also passed in the original canonical fixture (127,978 CU; 124.305s challenge-to-outcome).
- Finish a reproducible GPU-host provisioning and live runner. Proofs, witnesses, measurements, timing events and the preserved guest ELF are committed under [`evidence/l40s-v1`](../zkvm-replay/evidence/l40s-v1/README.md), with SHA-256 checksums. Raw logs and private signer material are excluded.
- Complete broader crash drills; obtain independent acceptance and explicit rollout review. Local full strict clippy, format, shell smoke guard, both default SBF builds, validator build, Bridge example/core/E2E, Northstar/Portal, SVM/core service and transaction-proof suites passed. Prototype SBF tests verified six retained proofs and rejected seven envelope/point mutations plus each of eight public fields per proof; this CPU-only check is also wired into CI.

The follow-up also verifies pending/zero-effect/challenged SIGKILL recovery and snapshot-fenced post-settlement recovery with the next exact withdrawal payout. It fixes ER clock regression after restart. These service drills are separate from the newer combined GPU proof-to-settlement fixture, which additionally passes snapshot-fenced mid-settlement restart and plan replay. A fresh bounded-runner proof is retained under [`hardened-runner-v1`](../zkvm-replay/evidence/hardened-runner-v1/).

No deployment, verifier default enablement, or Linear updates occurred. Publishing this evidence is not independent production acceptance.
