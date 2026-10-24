# VRF Coordinator GPU validation plan v1

**Status: bounded actual-SBPF candidate implementation authorized; legacy baseline verification is separate.**

The selected measurement machine is **one g5.xlarge / A10G**. This explicit user decision supersedes the earlier L40S target for this effort; it does not relabel the hardware or the older evidence. No rental or paid network proving is authorized.

The approved candidate uses actual interpreter replay of the pinned Coordinator/reference-verifier/test-consumer composition and an ER-native development lifecycle. Candidate kind **3**, version **1**, a separately bound guest/key and opt-in development Portal dispatch preserve the existing eight public fields/order and 256-byte encoding. Legacy kinds/routes remain unchanged; unknown kinds/versions fail closed. This document does not enable verifier dispatch, deploy a program or claim the candidate is proven.

## Evidence audit

The source baseline is [`34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52`](https://github.com/mirrorworld-universe/northstar/tree/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52). [PR #207](https://github.com/mirrorworld-universe/northstar/pull/207) retains SHA-precompile optimizations for the existing restricted replay guest. It does not introduce Coordinator execution support.

- [Candidate identity](../zkvm-replay/partial-candidate-v1.json): SP1 6.8.0, ELF SHA-256 `e49aef88f69f321331999be2b0a11f021a5d0f158bb2e2c1dc4825498cfffb62`, program key `0x0063a10e31e90625b76d06bfa22cbf0a4a8fa0850ccf71e063d243bd1a7aa31b`. `production_acceptance` is false.
- [Retained corpus](../zkvm-replay/evidence/g5-a10g-sha-v1/README.md): 13 warm samples take 40.996–42.732 seconds **including SDK verification**. Those are not pure prove-and-wrap durations.
- [Final-client measurements](../zkvm-replay/evidence/g5-a10g-sha-v1/final-client/measurements.json): process-cold prove+wrap 93.079 seconds, SDK verification 0.499 seconds, total 93.578 seconds. Warm prove+wrap 41.964/41.177 seconds; verification 0.489/0.487 seconds; totals 42.453/41.664 seconds. Client initialization and setup are separate phases.
- [Portal measurements](../zkvm-replay/evidence/g5-a10g-sha-v1/portal-verification.json): 84,079–84,081 CU for standalone checkpoint-bound verifier instructions. These are **not complete Production resolution costs**.
- [`resolver.rs`](../programs/portal/tests/zk_verifier/resolver.rs) executes complete `ResolveChallenge(Production)` against retained account snapshots. It checks accepted outcomes and preservation of rejected state. Its `coordinator` corpus label refers to Northstar's proof coordinator, not the external VRF Coordinator. Snapshot replay is not a fresh live challenge lifecycle.
- [Earlier L40S evidence](gpu-proof-validation-v1.md) used an older guest/key. Its three complete canonical resolutions do not demonstrate the current guest or VRF support. Current regression tests explicitly reject previous program proofs.

All retained G5 measurements above use an A10G with 23,028 MiB VRAM and about 15 GiB host RAM. This is now the selected measurement class, not a substitute for L40S. No cross-hardware timing extrapolation is claimed. The final-client record also reports approximately 550 MiB observed swap use; the corpus's zero-swap observation must not be applied to that separate run.

## Actual execution candidate and technical gates

The [external library pinned at `d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/tree/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd) implements RFC 9381 ECVRF-EDWARDS25519-SHA512-TAI, suite 1, with an 80-byte proof and a 64-byte output. Verifying that VRF proof does not prove execution of request creation, verification or callback instructions. The offline ristretto255 foundation is a different algorithm and cannot substitute for it.

The preserved legacy [Bank admission](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/runtime/src/bank/er_replay.rs#L33-L48) and [transaction relation](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/transaction-proof/src/lib.rs#L244-L379) restrict account/instruction shape, writes and syscalls. That legacy profile does not bind the Coordinator's curve operations, request PDA lifecycle, CPI privileges/signers, return data or Clock/SlotHashes dependencies. A successful native or ER runtime cycle does not remove that proof gap.

The selected candidate must recompute those semantics through the pinned SBPF interpreter/InvokeContext path, not a program-specific model or trusted trace. Complete authenticated execution prestate, feature/fee context, sysvars, program/ProgramData bytes, exact output/rollback and checkpoint binding remain technical prerequisites. A new SP1 guest must compile, execute and prove this candidate before its real generated key is admitted by the opt-in development verifier. Preserve legacy admission/constants. Missing or incompatible inputs reject.

The lifecycle is request, verification and callback inside a real Northstar ER, optionally with local L1/Portal on the disposable measurement machine. The external VRF library stays unchanged. It has no delegated shared queue; unsupported cross-layer settlement must remain explicitly unsupported. No further user design decision is required for this selected scope.

## Current Portal regression result

Rebuilt the current Portal SBF with the `zk-verifier-prototype` feature, installed platform-tools **v1.56** and **SBPFv2**. SBF SHA-256 is `474a6681fc7cde757e0920bc24ea83abc797094c37613bee25ecf9448f583c8a`, identical to the merged verifier. The final native test harness SHA-256 is `790d9ec968b2543e66764029d3dce57cd757c06e6d48df0dd593d2dc9dc6ad37`.

The complete retained SBF/Production resolver suite passed **8 tests, 0 failed, 1 ignored**. Three fresh legacy proof pairs independently passed the manual SBF test, including rejection after changing a proof byte and, separately, a public-input byte. Standalone verification consumed **84,079 / 84,081 / 84,080 CU**, in generation order. Their A10G prove+wrap times were **93.406 / 40.775 / 40.645 seconds**, with **0.487 seconds** SDK verification each; setup **15.988 seconds** and initialization **1.103 seconds** remain separate.

Full Production resolver instructions consumed **115,326 CU** for the current resolver snapshot and **115,686 CU** for the current proof-coordinator snapshot. Eight previous-program snapshots also reached the expected invalid-proof outcomes with exact state/bond checks. These are retained account-snapshot regressions, not fresh external VRF challenges. Passing the standalone verifier does not prove any request/callback lifecycle.

## Remote staging and candidate-build protocol

The coordinator executes commands remotely; workers never receive SSH credentials. Use fresh staging, build and result directories. Retained baseline results are immutable evidence even though the machine itself is disposable.

1. Inventory before build. Record native Rust/Cargo, `cargo-prove`, guest compiler sysroot, installed GPU server/launcher/environment and circuit cache identities. The measured installation reports native Rust **1.98.1**, guest Rust **1.96.0-dev**, SP1 **6.8.0** (`58c4aea`) and server **6.8.0**. The custom guest toolchain has no Cargo; that is not a missing Rust compiler. Use native Cargo with the guest rustc, not `cargo +succinct`.
2. Bind the complete source. Verify the baseline archive and reviewed overlay hashes before extracting into a clean directory. Record a file hash manifest of the resulting source, both workspace lockfiles, the exact capture/ELF inputs and the build request. Do not assume a worker's uncommitted edits exist remotely. Refuse archive traversal, escaping links and overwriting previous results. The coordinator stages the integrated archive/overlay, not a dirty remote checkout.
3. Qualify the actual compiler path. Pinned SP1's [local builder](https://github.com/succinctlabs/sp1/blob/58c4aeadbc504c274dd9fb82ed8130d0939ab756/crates/build/src/command/local.rs) resolves `rustc` with `RUSTUP_TOOLCHAIN=succinct`; an arbitrary `SP1_RUST_TOOLCHAIN_VERSION` environment variable does not override it. The observed `succinct` toolchain already reports 1.96.0-dev. Confirm `rustc --print sysroot` and compiler hashes; place the installed guest `bin` ahead of rustup proxies if required. Keep native Cargo 1.98.1 on PATH. A private rustup-home link is another reversible option, not a reason to overwrite the installed toolchain.
4. Compile only the separate workspace `northstar/zkvm-vrf`, whose current guest package is `northstar-zkvm-vrf-program`. Verify the manifest name rather than inferring it from the directory. Use two Cargo jobs and an independent target/output directory. Record `cargo prove build --help` and full logs. Use `--locked` once the reviewed standalone lockfile exists. If initial resolution is necessary, preserve that generated lockfile and freeze it before measurement; do not silently remove a lock requirement. A diagnostic `--ignore-rust-version` build may qualify the newer host-MSRV workspace on the available 1.96 guest compiler; label that flag and every source incompatibility. It is not proof acceptance.
5. The initial engine qualification guest deliberately rejects after execution and commits no public values. **Do not prove or admit its key.** Native execution, guest compilation, successful guest execution, wrapped proof and Portal resolution are distinct gates. Only the real authenticated envelope guest may advance beyond compilation qualification.
6. Bind the final guest ELF hash, derived program key, candidate kind 3/version 1/domain and profile to the host client and Portal candidate fold. The legacy client embeds the old guest; passing it a new witness or environment label cannot select the new candidate. The engine must supply a separately built client/interface with canonical input/output validation and SDK verification before GPU submissions start.
7. Run one candidate proof at a time through the reviewed lock-owning GPU launcher, in a unique results directory. Keep the installed server/circuit family only if it matches the exact SDK, recursion root and Groth16 key. Record hashes and any provisioning changes. Keep prove+wrap, SDK verification, setup, upload and resolution timings separate. Preserve failed attempts, cold/cache state and resource observations. Never use a paid network prover or infer measured peaks from limits.
8. Return the complete candidate artifact package to the evidence adapter. Compare every public byte against the authenticated execution/checkpoint, verify negative controls, then run the separately enabled candidate Portal route. Legacy fixtures must continue to pass their original route and fail cross-key admission.

Toolchain inventory confirms the expected client/server hashes. Circuit cache has no retained checksum manifest; its file listing alone does not establish content integrity. Independently hash/review the actual circuit files before claiming the new candidate's cache provenance. No successful candidate guest build/proof is claimed by this protocol.

## Bounded development baseline

Use `northstar/scripts/capture-replay-gpu-baseline.py` only with a reviewed, preinstalled launcher. This legacy adapter explicitly rejects candidate kind 3, unknown versions and other manifest schemas; it is not the new candidate's prover. It performs no installation, provisioning, sysctl changes, wallet activity, RPC transactions or service management. The launcher, not this script, owns the shared device lock and its own GPU-server/resource scope.

Prerequisites for coordinator review:

1. For this legacy baseline only, the preinstalled client/server hashes match the merged [final-client record](../zkvm-replay/evidence/g5-a10g-sha-v1/final-client.json). The fixture hash matches the legacy candidate manifest. New candidate builds use a fresh identified staging/build directory and separately versioned manifests; they never overwrite retained baseline results.
2. Inspect the launcher and its sourced environment. Pin both hashes. Confirm `SP1_PROVER=cuda`, the expected installed server/libraries, verified local circuit cache, one GPU, one shared device lock and one in-flight proof. No paid network prover is authorized. Remote prerequisite installation and provisioning are authorized on the disposable host, but must be explicit and recorded separately from measurement. The script does not independently prove the launcher's dependency closure or cached-circuit provenance; preserve the inventory separately.
3. Confirm the launcher acquires the same lock used by every other GPU entry point, fails if busy, runs within the existing 14 GiB RAM / 32 GiB swap scope, and cleans up only its own scope on exit or interruption. Do not wrap a lock-owning launcher in a second acquisition of that same lock.
4. Use a fresh output directory under an existing approved results parent. Keep full logs and partial failures. Review cached-circuit state explicitly; process-cold does not mean circuit-cold.
5. The coordinator executes remote commands through the approved access boundary. Workers do not receive SSH credentials. Do not duplicate an already running baseline.

Proposed command template, **not an execution claim**:

```sh
python3 -B northstar/scripts/capture-replay-gpu-baseline.py capture \
  --candidate "$REVIEWED_CANDIDATE_JSON" \
  --retained-client "$REVIEWED_FINAL_CLIENT_JSON" \
  --client "$INSTALLED_CLIENT" --server "$INSTALLED_SERVER" \
  --fixture "$REVIEWED_FIXTURE" \
  --launcher "$REVIEWED_LAUNCHER" --launcher-sha256 "$LAUNCHER_SHA256" \
  --launcher-env "$REVIEWED_LAUNCHER_ENV" --launcher-env-sha256 "$LAUNCHER_ENV_SHA256" \
  --output-parent "$APPROVED_RESULTS_PARENT" --execute-reviewed-launcher
```

The template assumes the script and reviewed JSON records have been transferred without modifying the remote source checkout. All paths are supplied explicitly. Copy those reviewed inputs into the retained evidence package with their hashes.

The script hash-checks the client/server/fixture/launcher/environment, derives the installed client's embedded program key before proving, then generates three sequential proofs using one client process. The key preflight uses a separate launcher invocation and scope; it must finish before the proof invocation. The proof process's first sample is process-cold, with locally cached circuits. A 15-minute GNU timeout bounds each launcher invocation; normal interruption requests cleanup through its handlers. No global service is stopped.

The script records the manifest's ELF hash as **not independently extracted**. The embedded key preflight plus retained client binary hash establish the baseline identity used here. They are not a reproducible guest build or a hash measurement of an extracted embedded ELF.

## Offline interpretation and tests

```sh
python3 -B northstar/scripts/test_capture_replay_gpu_baseline.py
python3 -B northstar/scripts/capture-replay-gpu-baseline.py summarize \
  "$CAPTURE/measurements.json" \
  --candidate northstar/zkvm-replay/partial-candidate-v1.json
```

The summary checks measurement schema/key, exact sequential sample count, distinct bounded artifact paths, proof envelope, public-input agreement and integer timing fields. It preserves over-limit timings as failed targets rather than dropping them. It reports prove+wrap, SDK verification, their sum, initialization and setup separately. It never assigns missing phases zero, reports complete resolution CU, or grants VRF/production acceptance. Its historical `l40s_acceptance: false` field is not the current hardware gate. This is a measurement parser, not the handoff manifest validator or a cryptographic verifier. Tests generate explicitly synthetic bytes for parser guards and separately check the real retained final-client measurements.

Retain `capture.json`, `exit`, `gpu.csv`, key preflight and proof logs, key JSON, raw measurements, summary, serialized SDK proofs and every on-chain-proof/public-input pair. Record artifact hashes and launcher/dependency inventory. A partial or timed-out run remains failed evidence, not an omitted sample. The script does not sample resource peaks; do not report limits as measured usage.

A fresh baseline proof may additionally pass the current Portal SBF's manual standalone-verifier test. Build the current Portal into a private output directory first. The existing manual test checks accepted proof/public-input bytes, but does not execute `ResolveChallenge`. Report this verification level separately. The focused resolver regression checks retained snapshot transitions; neither test turns the baseline into a VRF cycle.

## Three-real-resolution gate

Only start the gate when all prerequisites are available:

- The selected separately versioned actual-SBPF guest/profile/key for the Coordinator's request, verification and callback execution, with canonical fixtures and binding/rejection tests. A hash of trusted trace assertions is not execution validity.
- Three consecutive fresh complete Portal `ResolveChallenge(Production)` outcomes, linked to the exact fresh witnesses, proof bytes, public inputs and final account states. Distinguish the VRF callback from the Portal resolution event.
- One actual **g5.xlarge / A10G**; each final wrapped proof takes at most 120 seconds. Record the selected hardware, toolchain, cache and resource profile. Keep SDK verification, setup, transfer, upload and challenge-to-outcome timings separate.
- Each complete resolution consumes at most 130,000 CU, measured on the actual submitted resolution transaction, not a standalone verifier.
- Incorrect witnesses and unsupported cases reject; prior supported-route regressions pass. Preserve unsuccessful attempts and the full ordered series, not a selected set of successes.

Remaining engineering gates are native/zkVM execution qualification, complete authenticated ER-native fixtures, a real generated candidate key and opt-in verifier binding, fresh candidate proofs/resolutions and A10G measurements. The old legacy baseline cannot stand in for those candidate runs. Cross-layer settlement and production rollout are outside this development flow.

Human integration acceptance, independent reproduction, fixture-owner sign-off and substantive team feedback remain external dependencies. Engineering tests do not manufacture those responses.
