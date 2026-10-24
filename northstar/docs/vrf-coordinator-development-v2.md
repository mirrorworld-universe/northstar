# Coordinator ER-native development evidence v2

Status: **development candidate; engineering evidence is not human acceptance**.
This document supersedes the target/lifecycle selection in the first-wave handoff,
not the existing generic evidence schema or historical OKR text.

## Dated user decisions: 2026-10-23

The user selected **one g5.xlarge with one A10G** as this effort's measurement target,
stating that it replaced L40S after PR #207. An A10G is not an L40S. The historical
[generic contract](vrf-acceptance-v1.md) and `vrf-evidence-validate.py` continue to
require L40S; they are not silently reinterpreted. No rental or paid proving is
allowed by this contract.

The user selected **ER-native development**: request, verification and callback run
inside a real Northstar EphemeralRuntime. Local synthetic genesis deployment and
funding must be disclosed. A local L1/Portal may support development resolution.
Cross-layer delegation/settlement remains unsupported; the unchanged external
library has request PDAs, not a delegated shared queue. This selection does not
alter ownership/delegation policy, the external application ABI or production.

A separately versioned **actual bounded SBPF interpreter** candidate is authorized.
Do not replace actual execution with a program-specific transition assertion,
trusted syscall/CPI effects or a hash of a trace. Unknown/unsupported profiles,
programs, syscalls, kinds and versions must reject. The old proof routes keep their
meaning and default behavior.

## Source compatibility versus candidate identity

The source compatibility pin remains
`5891551ac67b003986a300e6b5e92089c886c3584c12c83411cf99374227b95d`, covering
[the actual library/IDL at `d1b8c3f`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/tree/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd).
Algorithm remains RFC9381 ECVRF-EDWARDS25519-SHA512-TAI, suite 1, 80-byte proof and
64-byte output. Ristretto CPU fixtures are not compatible.

`northstar/fixtures/vrf-coordinator-v1/candidate-development-v2.json` is the separate
versioned development contract. Its canonical identity is SHA-256 of
`json.dumps(document, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`.
`candidate-not-run-v2.json` is a closed-shape empty packet, not a run.

Candidate selector: **kind 3, version 1**, domain
`FrBytes::er_step_domain_v1(3, 1)`. The 256-byte public encoding retains eight
canonical BN254 fields in their established order. The domain bytes are:

```text
00000000000000006e6f727468737461722d65722d737465702d763100000301
```

The 149-byte session-context encoding/Poseidon commitment stays unchanged. The
candidate profile/runtime/program context belongs in the reviewed candidate
execution-context binding, not a silent change to session context. Exact candidate
profile, guest and verification-key bytes must have explicit hashes and artifacts.
Their null placeholders are missing identities, not approvals or active routes.
Candidate checkpoint/public-root meanings must match the candidate guest and
opt-in development Portal path; this evidence tool does not define those roots.

## Evidence gates

1. **Runtime:** a complete window containing at least 100 consecutive successful
   request/callback cycles on actual local ER. Exactly one callback per request;
   no lost/duplicate callback, omitted attempt or failed attempt. Keep invalid-proof
   and callback-rollback controls in separate complete windows. Each submission and
   observation uses one collector-wide monotonic nanosecond domain.
2. **Authoritative capture:** exact signed input and computed output, atomic Bank
   prestate with explicit absence, all invoked program/loader/ProgramData bytes,
   sysvar account/cache bytes, active/inactive features, recent-blockhash/fee/budget
   policy and producer-bound return data. Bind source/compiler/runtime/binary and
   ER genesis-configuration identity. No L1 genesis is invented when L1 is absent.
3. **Candidate:** explicit kind/version/domain/profile/guest/key, actual interpreter
   execution, authenticated checkpoint/isolation and complete result/effect binding.
   Capture validation alone is not execution verification.
4. **Proof/Portal:** three consecutive fresh successful execution proofs and real
   candidate `ResolveChallenge(Production)` resolutions. The three fixtures together
   cover request, fulfillment/verification and consumer callback. Each prove+wrap
   interval is **<=120 s**, each entire resolution instruction **<=130,000 CU**,
   all on the **same single g5.xlarge/A10G device**. No averaging away a failure or
   slow run. Preserve complete attempts, wrapped proof, public inputs, history,
   witness, checkpoint, isolation, prover log, receipt and resulting state/bond
   outcomes. A native fulfillment CU is not a Production-resolution CU.
5. **Rejections/regressions:** every case in the pinned corpus has its expected
   result. Include incorrect witnesses, unsupported paths/profile/kind/version,
   invalid VRF proof preservation, callback rollback and existing-route regressions.
6. **Lifecycle:** startup, cold-start and recovery are separate measured intervals
   with configuration/logs. Never subtract them from prove+wrap or use them instead.
7. **Independent acceptance:** Baixiong integration acceptance, Jonas independent
   reproduction/review, fixture-owner sign-off and three substantive team responses
   remain pending unless actual authenticated records are supplied externally.
   A model, successful build or JSON field cannot supply them.

## Real first-wave data: what is established

`first-wave-capture-receipts.json` retains compact hashes/sizes, not giant logs.
The final first-wave success journal contains 100 actual local-ER callback state
observations and **305 signed transactions**, including one expected 6003 rejection.
The separate failing-consumer journal contains zero delivered callbacks and retains
6003 plus 6001 rollback. These are useful real runtime results, not synthetic
transactions or an execution witness. The embedded rejection means the original
success journal cannot silently become a failure-free capture window.

The first-wave collector has per-operation elapsed time but lacks capture-wide
submission/observation timestamps. RPC before/after account reads are non-atomic.
The adapter preserves these omissions as null/missing; it never reconstructs
monotonic times from journal order, slots or elapsed milliseconds. In particular,
the 12-try cycle 31 consumes 163,968 **native fulfillment** CU, not resolution CU.
This harder case must not disappear from evidence because of an average or threshold.

## Closed phase-2 runtime evidence

`atomic-capture-receipts-v2.json` records the subsequently captured clean window:
**100 callbacks, 304 successful signed transactions and 200 authoritative
sidecars**, with one clock and matching compiled/captured harness source receipts.
The separate invalid-proof window retains 6003; the separate callback-rollback
window retains 6003 and 6001. All three passed the actual adapter. These are
capture-integrity results, not interpreter/proof verification by this tool.

The compact receipt binds each journal/collector, the complete sidecar-catalog
digest and selected exact wire/input/output/sidecar hashes. Raw data is retained
outside the branch and is not published here. The higher-CU retained fulfillment
is cycle 44, **139,097 native transaction CU**; this is not a Portal resolution
measurement. A different slower-slot higher-try diagnostic must retain its own
policy/identity and cannot silently replace a frozen-profile fixture.

The collector records a **693,787,983 ns constructor interval**. It does not establish
operational readiness, cold start or recovery; those remain null. Guest/key/review
profile bytes, g5 candidate proving measurements and three real Production
resolutions are not supplied by these receipts. Human acceptance remains pending.

## Reproduce the actual ER capture

Run inside the approved sandbox with the existing toolchains/dependencies. Freeze
source formatting before building; do not edit source during build or capture.
The harness rejects a compiled-vs-disk source hash mismatch. Use a fresh output
folder for every attempt and retain failures. Set `CARGO_HEAVY_LOCK`,
`CARGO_TARGET_DIR` and `NODE_PATH` to the coordinator's shared lock/target and
already available Noble modules. No installation or public-cluster wallet is used.

```sh
flock "$CARGO_HEAVY_LOCK" bash northstar/scripts/vrf-coordinator-runtime-build.sh \
  <PINNED_LIBRARY_ROOT> <NEW_PROGRAM_BUILD_DIRECTORY> > <BUILD_LOG> 2>&1

env -u VRF_RUNTIME_NEGATIVE_CONTROL \
  VRF_RUNTIME_SCENARIO=success VRF_RUNTIME_SLOT_MS=50 \
  VRF_ATOMIC_CAPTURE_DIR=<NEW_CAPTURE_DIRECTORY>/success.capture \
  bash northstar/scripts/vrf-coordinator-runtime-test.sh \
  <PROGRAM_BUILD_DIRECTORY> <NEW_CAPTURE_DIRECTORY>/success.jsonl 100 > <CAPTURE_LOG> 2>&1
```

Run long builds/captures in a detached guest tmux session you own. Save numeric
exit status and full logs, poll completion/readiness, then stop only that session.
The test script serializes Cargo and uses two jobs. This captures actual ER
execution, not GPU proving. Inspect the exit marker, collector-end errors and
complete observations before validating the window.

For a separate invalid-proof control, use fresh paths, one cycle and
`VRF_RUNTIME_NEGATIVE_CONTROL=1` with scenario `success`. For callback rollback,
use fresh paths, one cycle and `VRF_RUNTIME_SCENARIO=callback_failure`. Keep both
windows and their failed transactions. Do not filter them into the clean window.

The frozen 50-ms capture has **transaction admission age 900** and **Bank queue
retention 1800**. They are distinct fields, not interchangeable defaults. A slower
slot/higher-try diagnostic can change the age profile; never silently use it under
the frozen 900-age guest policy. Read actual `runtime_policy` from each sidecar
audit and let native candidate admission enforce the reviewed policy.

## Actual journal and sidecar adapter

From the repository root, Python 3.9+ standard library only:

```sh
python3 northstar/scripts/vrf-coordinator-fixtures-capture.py \
  --journal <CAPTURE_DIRECTORY>/cycles-100-final.jsonl \
  --output <AUDIT_DIRECTORY>/first-wave-audit.json --structure-only
```

Wire files default to the journal's sibling `.transactions` directory. Override
with `--wires-root` only when retaining the same exact bytes. Optional `--receipt`
loads a per-run receipt containing the expected `journal` size/hash. For the compact
checked-in receipt catalog, add
`--receipt northstar/fixtures/vrf-coordinator-v1/first-wave-capture-receipts.json --receipt-run cycles-100-final`. Every wire is
hash/size checked, parsed as canonical legacy wire and compared exactly with the
RPC-returned base64 bytes and signature identifier. This Python tool does **not**
verify Ed25519 signatures or ECVRF proofs; the candidate admission/engine must.

The adapter binds real request seed/key/alpha, 80-byte fulfillment proof, verifier
CPI input bytes, callback request/output/delivery observations and close/fee/refund
records. Its actual journal specimen is explicitly bounded to the test consumer's
one writable callback slot and no appended arguments; other shapes reject rather
than being presented as tested runtime capture support. It checks transaction/submission/wire/account-observation completeness,
expected rejection retention and callback accounting. Unknown source/program/
discriminator/algorithm/layouts and malformed or truncated records reject.

For phase-2 collectors, add the actual atomic sidecar directory:

```sh
python3 northstar/scripts/vrf-coordinator-fixtures-capture.py \
  --journal <CAPTURE_DIRECTORY>/success.jsonl \
  --sidecars-dir <CAPTURE_DIRECTORY>/success-sidecars \
  --output <AUDIT_DIRECTORY>/atomic-audit.json --structure-only
```

Supported sidecar domain is `northstar.er-vrf-candidate-capture.v1`, version 1.
`collector.json` and `collector-end.json` bind clock, identities, capacity, complete
record count and capture errors. Numbered sidecars must be consecutive, have one
known signed-wire identity each and collectively cover every selected request/
fulfillment attempt in the journal. Rejected, unknown, missing or duplicated
sidecars do not qualify.

The strict sidecar decoder checks:

- Atomic lock/recheck flags and original capture start/pre-commit/end times.
- Exact prestate key set, explicit null absence and complete committed output keys.
- Program ownership/loader metadata linkage, fixed 45-byte ProgramData ELF offset,
  deployment visibility, and actual ELF bytes matching the three source-pinned
  fresh build hashes. It does not substitute a program filename for those bytes.
- Required Clock/Rent/EpochSchedule/SlotHashes accounts, all recorded cache/account
  byte equality, active/inactive features and explicit runtime policy data.
- Signed message account order, requested compute/heap/zero-fee policy and signed
  recent-blockhash/age membership.
- Producer-bound return data and failed-state agreement with the captured rollback
  boundary. Nonpayer Bank state is preserved. With zero fees, the payer's balance,
  data, owner and executable flag stay unchanged; only Agave's loader rent-epoch
  normalization may persist, matching the captured rollback account.

**Bank prestate is not the same boundary as loaded execution accounts.** Agave
normalizes rent epochs and creates runtime-only account views. Both boundaries are
retained. The engine must authenticate/recompute normalization and execution; this
adapter never treats observed loaded accounts as an execution oracle. Full ER
checkpoint pre/post maps also retain delegated/touched/current-writable keys,
including explicit absence, separately from the bounded VM account map.

Sidecar `input_sha256` hashes UTF-8 sorted-key compact JSON of `input`.
`output_sha256` hashes the same encoding of the object containing only `execution`
and `committed_accounts`. No ASCII escaping or whitespace is added. The complete
sidecar file hash additionally binds checkpoint maps and commit metadata.
`collector_sha256` binds the original collector bytes. Active-feature digest
encoding is explicitly `JSON-sorted-base58-active-pairs-v1`; the full active plus
inactive feature context has a separate digest and remains in the captured input.
A genesis identity is labeled as SHA-256 of bincode `GenesisConfig`, not invented
as an L1 chain identity.

Limits are 256 MiB per journal, 2 MiB per JSONL record, 16 MiB per sidecar and 512
sidecars. Account maps retain source bounds; malformed/unsupported data rejects.

An adapter report has `execution_verified=false` and acceptance not established.
Capture provenance/atomicity flags are not cryptographic attestations. Actual engine
replay, guest verification and independent source/capture review remain required.
Raw journals, sidecars, secrets, local paths and large logs must not enter the PR.

## Candidate packet validator

```sh
python3 northstar/scripts/vrf-coordinator-fixtures-candidate.py \
  --packet northstar/fixtures/vrf-coordinator-v1/candidate-not-run-v2.json \
  --journal <CAPTURE_DIRECTORY>/cycles-100-final.jsonl
```

The empty packet is structurally valid but blocked. Add `--sidecars-dir` for actual
complete capture and `--artifact-root` for packet artifacts. The separate packet
shape is version `northstar-vrf-coordinator-candidate-evidence/v2`; unknown fields,
including arbitrary approval assertions, reject. `identity` pins library,
algorithm, selector/domain, profile, guest/key, runtime/features and genesis config,
plus the whole journal, original collector, configuration, rejection corpus and
proof-attempt window hashes. `bindings` references local profile/policy/guest/key/
configuration/rejection-corpus bytes; all artifact references have relative paths,
exact size/hash and honest provenance.

`profile_sha256` is the **Portal review JSON file's SHA-256**, matching Portal key
tooling. It is not the guest's policy digest. `execution_profile_sha256` separately
binds raw Borsh `CandidateProfileV1` bytes using the witness implementation's exact
framing: `SHA256("northstar-vrf-profile-v1" || u64_le(byte_length) || raw_borsh)`.
The `execution_profile` binding supplies those raw policy bytes. Review JSON must
repeat that digest and specify kind3/version1, `checkpoint_steps` in 1..16, the exact
eight-field order and `production_acceptance: false`. Native candidate decoding/
verification must still validate the policy's semantics; this Python tool checks
the framed bytes, not Borsh semantics.

Retain the actual SDK setup receipt, SDK `program_vkey_hash` and generated Portal
key manifest in configuration/setup artifacts. Run Portal's
`tools/bounded_vrf_key.py generate|check` workflow and real candidate verification;
serialized-key file SHA alone does not establish the ELF/key relation or prove that
the configured Portal binary accepts that guest.

Each resolution references a known atomic fixture's exact wire/input/output hashes
and complete sidecar-file hash, which also binds checkpoint maps and commit metadata.
A request fixture declares exactly `[request]`; a successful fulfillment fixture
exactly `[fulfillment, consumer_callback]`. Matching coverage is counted only from
known successful fixtures, never a run's unsupported claim. The three-run union
must cover all paths. Runs bind the packet identity digest, distinct checkpoints,
fresh wrapped bytes, eight canonical fields with the six required nonzero commitments, same device
and one monotonic domain for all proof stages. The submitted wrapped-proof artifact
must have the current Portal's 356-byte SP1 envelope, expected verifier prefix/root,
zero exit code and canonical nonce. These format checks do not verify its pairing
or guest/key binding. Identity digest is canonical SHA-256 of
`{"identity": identity, "contract_sha256": contract_sha256}`.

`proof_window` names one bounded monotonic interval and a complete hash-bound
attempt log (`northstar-vrf-candidate-proof-attempts/v1`). Every attempt ID/status/
checkpoint/wire and submitted/wrapped/resolved timestamp must match the resolution
rows exactly. A retained fourth failed attempt cannot be omitted to claim three
consecutive successes. Start a new window after failure and retain the preceding
window through `retained_failed_windows`; never filter its failed records.

Proof stage order is:

```text
witness_start <= witness_ready <= submitted <= core_done <= wrapped <= uploaded
             <= resolve_submitted <= resolved
prove+wrap = wrapped - submitted
```

The rejection corpus is a hash-bound artifact with version
`northstar-vrf-candidate-rejection-corpus/v1` and exact `cases` membership
(`id`, `category`, `expected`). All results must match that membership. Its owner
must include all agreed bounded-profile cases and the eight application invariants:
proof validity, registered-key binding, input binding, request replay, slot
separation, callback account restrictions, expiry and repeated fulfillment.
Category coverage alone does not establish corpus adequacy or owner approval.
Lifecycle measurements have their own capture artifact and are never folded into
proof time.

Both tools return **0 only with explicit `--structure-only`**, **1 by default** for
valid-but-unaccepted data, and **2 for invalid** bytes/structure/bindings. A completely
fabricated but internally consistent unit-test packet still cannot establish
acceptance. `consistent_as_reported_requires_external_verification` is only local
arithmetic/reference agreement, never verification of a proof, honest measurement,
complete attempt window, actual hardware or reviewer identity. Never use either
evidence report as a verifier, route-admission input or production enablement flag.

## Focused tests and reproduction

```sh
python3 -m unittest discover -s northstar/scripts -p 'test_vrf*.py' -v
NORTHSTAR_VRF_RUNTIME_ARTIFACTS=<RETAINED_RUNTIME_DIRECTORY> \
  python3 -m unittest discover -s northstar/scripts -p 'test_vrf_coordinator_capture.py' -v
NORTHSTAR_VRF_RUNTIME_ARTIFACTS=<RETAINED_RUNTIME_DIRECTORY> \
NORTHSTAR_VRF_CANDIDATE_CAPTURE=<REAL_ATOMIC_SIDECAR_DIRECTORY> \
  python3 -m unittest discover -s northstar/scripts -p 'test_vrf_coordinator_capture.py' -v
```

Real-artifact tests skip explicitly if their environment paths are absent. They
check the sealed first-wave receipts and real wire/CPI/state observations. Mutation
specimens are marked synthetic tests and never saved as real captures. Additional
candidate tests exercise hardware substitution, per-run limits, old-domain/kind
rejection, exact fixture/corpus membership, artifact hashes, provenance and false
approval/certificate claims. Legacy evidence tests remain enabled and unchanged.
