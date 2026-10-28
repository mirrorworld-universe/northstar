# CPU execution-proof readiness, v1

Status: **native conformance tooling, not VRF execution-proof support**. No production route, guest relation, proof ABI, accepted key, or Portal program changes are included.

## Run it

From the validator repository root, with the existing Rust toolchain:

```sh
export CARGO_BUILD_JOBS=2
cargo test --locked -p northstar-transaction-proof --no-default-features
cargo test --locked -p northstar-transaction-proof
cargo clippy --locked -p northstar-transaction-proof --all-targets \
  --no-default-features -- -D warnings

# Five retained history-derived witnesses; new output directory required.
bash northstar/scripts/vrf-proof-cpu-conformance.sh /tmp/vrf-proof-cpu-evidence

# Inspect an independently supplied encoded witness and expected public inputs.
cargo run --locked -p northstar-transaction-proof --no-default-features \
  --bin vrf-proof-readiness -- WITNESS.bin EXPECTED_PUBLIC_INPUTS.bin \
  --checkpoint-root CHECKPOINT_TRANSACTION_EFFECT_ROOT_HEX \
  --runtime-profile APPROVED_RUNTIME_PROFILE_SHA256_HEX --require-anchors
```

Both anchor values are 64-character hex strings. Development mode keeps them optional; absence is reported as `Missing`, never as an authenticated match. For automation, `--require-anchors` requires **both supplied pins and every conformance check to pass**. Obtain pins from independent checkpoint/profile evidence, not by copying values from the witness being checked. The expected public-input file is mandatory and exactly 256 bytes.

Exit codes:

- `0`: native relation, canonical host checks, and supplied bindings conform; if `--require-anchors` is set, both anchors are present and match. This does **not** mean VRF readiness.
- `2`: encoding, relation, canonical shape, input/argument, or supplied binding failure.
- `3`: `--require-vrf` was requested, all other requested gates passed, but required VRF support remains missing.
- `4`: `--require-anchors` was requested and native conformance passed, but either or both anchors are absent.

Precedence is independent of argument order: rejection (`2`), then missing required anchors (`4`), then VRF refusal (`3`), then success (`0`). Thus `--require-anchors --require-vrf` returns `4` with missing pins, `2` with a mismatched pin, and `3` with both matching pins. Native reports remain unchanged: `conformant=true` with `all_bindings_anchored=false` can accompany strict-mode exit `4`.

**Pin provenance remains the caller's responsibility.** `--require-anchors` only checks supplied values; it does not authenticate trusted capture, establish independent provenance, or imply production proof acceptance. No anchors are derived automatically from the witness.

Every successful report says `vrf_ready=false` and `proof_generated=false`. `all_bindings_anchored=true` means all supplied host comparisons passed; the tool does not authenticate the origin of those anchors. Reports have deterministic ordering and no invented cycle/gas/timing fields. The input limit is 32 MiB for this local tool only; it is not a production witness admission rule.

The batch script does not read retained proofs, invoke SP1, contact RPC, or change source files. It records source hashes/status, input hashes, toolchain versions and native reports. It requires `--require-vrf` to return `3` for every case and compares repeated reports byte-for-byte.

## Actual extraction path audited

1. [`Bank::er_replay_snapshot`](../../runtime/src/bank/er_replay.rs) admits only a legacy, one-signer transaction with three static keys, one outer instruction, program index `2`, and instruction accounts `[1]`. It requires an upgradeable executable program and the supported fee structure. It captures program/programdata, transaction accounts, Clock/EpochSchedule/Rent/SlotHashes, feature activation, blockhash queue, fee policy, processing age and epoch stake. The **current code bound is 256 KiB**, including serialized overhead; older runbooks describing 128 KiB are stale.
2. [`EphemeralTxClient`](../src/ephemeral_tx_client.rs) captures before execution when a session is configured, compares committed execution data, and records replay values in [`ErHistoryStore`](../../rpc/src/er_history.rs). Capture retention is immutable, commitment-aware, in memory, and limited to 256 entries plus slot retention. This is not restart-durable witness storage.
3. [`extract_replay_witness_v1`](../src/replay.rs) verifies the checkpoint artifact, requires finalized history and a bounded snapshot, checks captured pre-state, then performs native SVM re-execution. It compares committed post-accounts, execution success, units, loaded-data size, transaction/prioritization fees and account count.
4. [`assemble_replay_witness_v1`](../transaction-proof/src/fixture.rs) uses that real execution to collect writable and readonly accounts, VM rows/words/call targets, memory snapshots, event tags and outcome metadata. It rejects more than one VM trace. The temporary fixture checkpoint is replaced by the actual DA page and authentication paths before extraction returns. Synthetic fixture neighbor leaves are not final history-derived bindings.
5. Extraction injects caller-supplied session/build context, checks the unchanged native relation and compares **all eight** expected public inputs. The proof coordinator supplies its frozen profile and authenticated checkpoint/session bindings. Caller-supplied provenance is not independently established by this tool.

A VRF transaction outside the snapshot/account/invocation shape can fail **before an encoded witness exists**. A missing encoded application witness is not evidence that its curve or callback semantics passed.

## Current relation and host-only preparation

[`replay`](../transaction-proof/src/lib.rs) still enforces its existing bounded checks:

- Witness v2, full-transaction kind `2` / version `1`, trace schema `1`, SBPF v0.
- Canonical short-vector legacy wire shape, one strict Ed25519 signature, three keys, one instruction and blockhash-list membership.
- Two pre/post writable accounts, no rollback accounts, instruction `[1]` or `[1, u32-le]`, configured signature fee and no priority fee. The target must be program-owned; its first data byte becomes `100`, with the remaining data and target lamports unchanged. This is not the request/fulfillment state machine.
- Supplied program-word/row agreement, supported register/control-flow rules, call/return consistency, final exit, trace hash and **exactly one `sol_memcpy_` row**. Other syscall keys remain rejected. Internal SBPF calls are not callback CPI.
- Canonical checkpoint state/effect serialization; transaction hash, step and units binding; pre/post transition consistency; transaction-effect, readonly-L1 and settlement authentication paths.
- Eight canonical BN254 public fields and the full-transaction domain/nonzero commitment checks.

This is not a complete SVM relation. In particular, hashing supplied memory snapshots is not general memory replay; supplied program words are not a replacement for executable decoding. No new claim of arbitrary memory/syscall/ELF semantics is made.

The new [`readiness`](../transaction-proof/src/readiness.rs) module is excluded under `target_os = "zkvm"`. It calls the existing checks and authoritative `replay` unchanged, while independently reporting checkpoint/public-input consistency even when another stage rejects. Observed counts and syscall names describe the supplied witness; they are not trusted execution outcomes or application classification.

Two reusable **host-only** guards are added:

- Canonical 149-byte session-context shape, domain prefix and settlement-policy version. The current guest accepts other context bytes when public inputs are changed accordingly; the diagnostic host guard intentionally rejects those shapes without changing guest acceptance.
- Optional runtime-profile SHA-256 anchor over fixed-width revision hashes, feature hash, signature-fee policy, SBPF version, VM/syscall hashes, program/programdata identifiers, entry PC, ELF SHA-256 and program hash. Blockhash history and observation slot remain transaction-specific and are not part of this profile. The guest does not enforce every metadata field; regressions explicitly demonstrate that distinction.

The checkpoint transaction-effect **tree root** is an optional external anchor because the public `tx_effect_root` is the selected transaction-effect **leaf commitment**, not that whole tree root. A self-consistent Merkle path must not be confused with independent authentication of its root.

## Measured CPU evidence

Native batch conformance passed against the retained `witness-v2.bin` and corresponding `northstar-sp1-public-inputs.bin` in:

- `evidence/hardened-runner-v1`
- `evidence/combined-settlement-v1`
- `evidence/proof-performance-v1/fast-live-1`
- `evidence/proof-performance-v1/fast-live-2`
- `evidence/proof-performance-v1/fast-live-3`

Each has **365,709 encoded bytes, 208 VM rows, 218 SVM executed units, two pre/post writable accounts, one readonly account, one VM invocation tag, one instruction-data byte and one `sol_memcpy_` row**. None contains observed curve/CPI rows. These are replays of retained executions, not five new ER transactions or new proofs. Input hashes and complete reports are produced by the batch script.

Reference retained witness SHA-256 (`hardened-runner-v1`):
`b34167e22b805890147237a0d3a3eb8e03ce32559b00959fcc0600f00f78b025`.

Fresh CPU unit tests also passed the existing paid/gasless ER-client history extraction tests and partial-checkpoint history reproduction. Those execute local Banks/clients and native SVM, **not a hosted ER RPC or Portal resolver**:

```sh
cargo test --locked -p northstar --features replay sbf_checkpoint_
cargo test --locked -p northstar --features replay \
  partial_sbf_checkpoints_reproduce_history_public_inputs
```

The conformance regressions cover all eight public fields, noncanonical field encodings/domain/zero commitments, checkpoint paths/roots/state/effects/observation slot/units, session shape, metadata-profile changes, extra/aliased accounts, owner binding, rollback/failure/versioned shapes, absent blockhashes, and zero/extra/curve/CPI/unknown syscall exclusions. Process-level tests also cover strict-mode missing/mismatched/matching pins and combined-flag precedence. Their fixed fixture pins test comparison behavior, not independently authenticated capture. Syscall exclusions are rehashed mutations of supported traces, **not successful curve/CPI fixtures**. Existing deterministic, partial-checkpoint, benchmark-row and gasless-route regressions remain intact.

SP1 dependencies are pinned to **6.8.0** in the nested workspace. The current host has neither `cargo-prove` nor the SP1 Rust toolchain nor a prebuilt replay runner in the checked paths. No tools were installed; no fresh SP1 `execute`, cycles, gas, CPU proving, wrapping, Portal CU or proof verification is claimed. Previously retained proofs are never reused as evidence for this source or a different guest.

## Missing VRF evidence and approval boundaries

| Required evidence | Current CPU result |
| --- | --- |
| Bilal's versioned request fixture, program/SDK revisions and account/instruction ABI | **Missing**; no ABI inferred from the account-write fixture |
| Matching fulfillment fixture and consumer/callback revision | **Missing**; no successful fulfillment trace invented |
| Request/fulfillment account transitions and required readonly/sysvar values | **Unsupported by the current relation**; actual application shape remains unpinned |
| Curve validation, group operations and multiscalar multiplication | **Unsupported**; named keys are diagnostic only and rejected in tests |
| Callback CPI, nested program/account/privilege/return-data behavior | **Unsupported**; single-invocation witness extraction is not CPI replay |
| Application slot/time semantics | **Unpinned/unsupported**; `runtime.slot` currently binds readonly L1 observation slot, not ER bank time |
| SP1 CPU execute for the current source | **Not run**, installed toolchain missing |
| Real VRF proofs and independent acceptance | **Not demonstrated** |

Keep the next boundary explicit. First freeze actual application fixtures, account/compute/trace limits and rejected cases with the application and integration owners. Then seek supervisor/user approval before changing witness encoding, multi-invocation relation architecture, public-input/runtime-profile commitments, guest/key lifecycle or accepted-key policy. A bounded implementation must verify curve inputs/outputs, memory effects, CPI privileges/accounts and nested execution; it must not trust supplied success/results. Merely lifting the syscall allowlist or accepting a native outcome is not a permissible implementation.

Any new guest must be rebuilt with its own recorded ELF/key, independently executed and later proved/accepted under the approved upgrade policy. Existing proofs cannot establish new guest semantics. Production acceptance still requires three consecutive real Portal resolutions, each proof plus wrapping at most 120s on one L40S, successful `ResolveChallenge(Production)` at most 130K CU, and independent acceptance. This CPU work satisfies none of those production gates.
