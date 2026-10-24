# Pinned Coordinator runtime diagnostic

This opt-in harness characterizes the unchanged VRF library at
[`d1b8c3ff`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/tree/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd).
It does not add ER delegation support, production verification, execution proofs,
or Portal challenge resolution. The library explicitly limits v1 to L1 and has
[no delegation instruction](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/docs/01-architecture.md#L29-L33).

## Program identity and interface

The three local builds use these exact IDs, without `devnet` or `mainnet` features:

| Program | ID | Role |
| --- | --- | --- |
| Coordinator | `FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N` | Request/config accounts and fulfillment |
| Reference Verify | `FUBz1nvSSoHnMqFoK2CA3jF5ckpYPdF3ZT4SoLzDNzh1` | Test-grade RFC 9381 verification |
| Test consumer | `HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt` | Request CPI and callback state |

The Coordinator pins its verifier in
[`ids.rs`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/ids.rs).
The reference verifier and consumer are explicitly test-only; neither substitutes
for an approved production verifier.

- [`initialize`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/instructions/program_management/initialize.rs)
  creates the rent-funded 360-byte Coordinator-owned singleton `['config']`.
  The payer, upgrade authority, and main authority sign. The upgradeable loader's
  ProgramData PDA must record that upgrade authority. Initially the suite is unset;
  the harness configures a real suite-1 key, registered fulfiller, and 5,000-lamport fee
  through ordinary instructions.
- [`set_behaviour` and `make_request`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/test-consumer/src/lib.rs)
  create the consumer-owned `['slot', request_key]` account and file the request by
  signed CPI. `['vrf_requester']` under the consumer signs the Coordinator call.
  The slot records the expected request, output, delivery slot, and count.
- [`request_random`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/instructions/request/request_random.rs)
  creates a new Coordinator-owned `['request', requester, request_key]` PDA with
  System Program signed CPI. The rent payer and fee payer sign; the fee transfers
  into the request. Callback mode requires an executable requester and consumer PDA
  signer. One writable callback account makes the request 434 bytes; Store mode is
  401 bytes. Existing open requests at that address are rejected.
- [`alpha`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/alpha.rs)
  reads `Clock::get()` and raw SlotHashes account bytes. It rejects an empty history.
  The domain is exactly `sonic-vrf/v1`, regardless of project naming. Alpha binds the
  Coordinator ID, request PDA, seed, request slot, newest hash slot and hash. The
  harness recomputes this hash from the observed request bytes.
- [`fulfill`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/instructions/fulfillment/fulfill.rs)
  requires a registered fulfiller signer and `request_slot < Clock.slot <= deadline`.
  The request's snapshot fixes its suite/key/alpha/fee/rent payer. Remaining callback
  accounts must match stored order and writability. The consumer and callback signer
  are Anchor optional accounts, present for Callback mode.
- [`verify_by_cpi`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/verify.rs)
  sends no account metas to the pinned verifier. Instruction bytes are Anchor
  discriminator, suite `u8`, key `[32]`, alpha `[32]`, proof length `u32 LE`, proof.
  Return data must come from that exact program and be exactly 64 bytes. Reference
  Verify implements RFC 9381 ECVRF-EDWARDS25519-SHA512-TAI: library suite field 1,
  RFC suite string `0x03`, 80-byte proof and 64-byte output. It uses Edwards curve
  syscalls and software SHA-512, not Northstar's offline Ristretto implementation.
- Successful Callback fulfillment writes Fulfilled state before the callback,
  invokes the consumer with Coordinator PDA signer
  `['vrf_callback', consumer_program_id]`, then pays the fee and closes the request
  to its recorded rent payer. The consumer verifies signer derivation and request
  identity. Callback failure rolls the transaction back. Store fulfillment instead
  retains the output; [`close_request`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/instructions/request/close_request.rs)
  later refunds rent to the recorded payer, authorized by the Store requester.
  An unanswered request can instead be expired by any caller after its deadline:
  the recorded fee payer receives the fee unless that would violate rent exemption,
  in which case it goes to the rent payer; the request then closes to the rent payer.
  The harness inventories but does not exercise Store close or expiration.

The vendored [SDK IDL](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/sdks/packages/vrf-coordinator/src/idl/vrf_coordinator.json)
and [SDK builders](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/sdks/packages/vrf-coordinator/src/vrf-coordinator-lib.ts)
encode this same ABI. The harness uses narrow Rust byte builders, not a regenerated
IDL. Its proof helper imports the unchanged
[reference service](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/sdks/packages/vrf-services/src/ecvrf.ts)
from the staged source, using the public RFC Example 16 test secret and its own
reference verification before submission. It does not exercise the full polling
[oracle service](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/sdks/packages/vrf-services/src/oracle.ts)
or the SDK's subscription/retry behavior.

## Selected ER-native development lifecycle

Northstar's [Portal delegation instruction](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/programs/portal/src/instructions/delegate.rs#L17-L33)
requires a pre-staged Portal-owned account, a matching buffer owned by the original
program, and the delegated account's signature. The pinned Coordinator and test
consumer do not expose the owner-program instruction needed to stage and sign this
transition for their PDAs. A wallet cannot substitute for the owning program.
No account injection in this harness satisfies that invariant.

On 2026-10-23, the selected development flow became ER-native request, verification
and callback with the external library unchanged. New request and consumer-slot
PDAs are created inside ER; this does not invent a delegated shared queue or an L1
ownership transfer. Cross-layer materialization, rent/fee settlement and imported
consumer ownership remain unsupported. Existing access/delegation rules stay intact.

The same decision selected **g5.xlarge/A10G**, following PR #207, as the measurement
target for this effort, replacing earlier L40S wording. An A10G is not an L40S.
Local VM capture and native replay are not GPU proving or Portal acceptance.

## Reproduce

Run inside the Northstar development VM (`NORTHSTAR_SANDBOX=1`). Required existing
tools: Cargo/Rust 1.98.1, `cargo-build-sbf`, installed platform-tools v1.56, Bun,
`flock`, and existing JS modules containing compatible `@noble/curves` and
`@noble/hashes`. No Anchor CLI or standalone host Rust 1.89 installation is needed.

Set `LIBRARY_SOURCE` to a clean clone at the exact revision above. Set
`CARGO_HEAVY_LOCK` to the shared build lock and, where appropriate,
`CARGO_TARGET_DIR` to the shared compatible Northstar target directory.

```sh
export CARGO_BUILD_JOBS=2
export NODE_PATH="$EXISTING_JS_MODULES"
export CARGO_HEAVY_LOCK=/tmp/northstar-cargo-heavy.lock
ARTIFACTS="$PWD/target/vrf-coordinator-runtime"
flock "$CARGO_HEAVY_LOCK" bash northstar/scripts/vrf-coordinator-runtime-build.sh \
  "$LIBRARY_SOURCE" "$ARTIFACTS" > "$ARTIFACTS-build.log" 2>&1
bash northstar/scripts/vrf-coordinator-runtime-test.sh "$ARTIFACTS" \
  "$ARTIFACTS-100.jsonl" 100 > "$ARTIFACTS-test.log" 2>&1
```

Use a fresh artifact directory and journal for each attempt. Preserve failed logs.
For a separate valid-proof / failing-consumer rollback probe, run one cycle with
`VRF_RUNTIME_SCENARIO=callback_failure` and a different journal path. That probe
requires the consumer's named `DeliberateFailure` error (6001), unchanged Pending
request and balances, zero deliveries, and no close. Its summary reports zero
completed callbacks; it is not a successful request/callback cycle.
The build stages a `git archive` of the clean pinned revision and overrides only
host Cargo selection explicitly with `RUSTUP_TOOLCHAIN=1.98.1`; SBF compilation uses
the already installed platform-tools compiler, `--skip-tools-install`,
`--no-rustup-override`, `--tools-version v1.56`, `--arch v2`, and `--locked`.
It hashes all three fresh ELF files in `program-sha256.txt`. It never uses an older
`vrf_reference_verifier.so` or writes into the reference checkout.

## Evidence boundaries

The ignored integration test starts an actual `EphemeralRuntime`, submits all
application instructions through its RPC, and reads actual ER transaction history.
A one-cycle default run is a smoke diagnostic. An explicit `100` run requests 100
consecutive cycles and checks request output, exactly one callback, request close,
rent conservation, and one fee payment per cycle. Legacy diagnostic runs also submit a
changed-challenge proof in the first cycle. The new atomic feature keeps the default
success window free of negative controls; `VRF_RUNTIME_NEGATIVE_CONTROL=1` explicitly
selects a separate control window requiring error 6003 and state preservation. The journal records every signed
submission, actual transaction metadata (logs/CPIs/compute/return data where
provided by ER history), request bytes, reference proof/output and callback bytes.
Raw signed legacy transactions are written beside the journal in a new
`<journal-stem>.transactions/` directory, with SHA-256 identities in the journal.
The journal also records deployed ELF identities, active runtime features, the
zero-fee model, and account observations before and after each submission.
Those RPC observations remain explicitly non-atomic and are not replay inputs.
The separate opt-in candidate stream below captures the authoritative boundary;
legacy history and snapshots are not reinterpreted or widened.
A final summary exists only after all requested cycles pass. Failed attempts have
no successful summary and must not be counted as completed runs.

An empty test `GenesisConfig` has zero active features. The pinned Coordinator's
first `update_vrf_key` then reaches an unregistered curve syscall and fails with
`ProgramFailedToComplete` / `unsupported BPF instruction`. The harness therefore
activates only `curve25519_syscall_enabled`
(`7rcw5UtqgDTBBv2EcynNfYckgdAaH1MAsCjKgXMkN7Ri`) through a normal genesis feature
account, then asserts that ER inherits it. See the
[feature-gated syscall registrations](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/syscalls/src/lib.rs#L408-L425).
This is explicit test setup, not a feature activation or policy change on a hosted
cluster. A native regression guards the parent feature requirement.

Genesis upgradeable program accounts, the explicit curve feature, and
`credit_deposit` funding without L1 events are synthetic setup. There is no injected request, alpha, proof result, or consumer
state. This is a **local ER runtime diagnostic**, not a real L1 delegation lifecycle,
Bank-only result, hosted ER handoff, production oracle integration, independent
reproduction, or execution-validity proof. A successful 100-cycle result alone does
not settle ownership of new request/slot PDAs, fee/rent settlement, or entropy
freshness across L1 reanchors. Those paths remain unsupported despite the selected
ER-native development lifecycle. No runtime access guard, legacy replay relation,
legacy guest/key, Portal constant, library ABI, or deployment changes are included.

## Authoritative candidate capture (opt-in)

Build feature `northstar/vrf-candidate-capture` adds a separate observer, disabled
by default and installed explicitly with `EphemeralRuntime::set_vrf_candidate_capture`.
It never changes `er_replay_snapshot`, persisted legacy history or accepted proof
admission. The harness enables it when `VRF_ATOMIC_CAPTURE_DIR` names a fresh output
directory:

```sh
VRF_ATOMIC_CAPTURE_DIR="$ARTIFACTS/atomic-100" \
  bash northstar/scripts/vrf-coordinator-runtime-test.sh "$ARTIFACTS" \
  "$ARTIFACTS-atomic-100.jsonl" 100
VRF_ATOMIC_CAPTURE_DIR="$ARTIFACTS/invalid-proof" VRF_RUNTIME_NEGATIVE_CONTROL=1 \
  bash northstar/scripts/vrf-coordinator-runtime-test.sh "$ARTIFACTS" \
  "$ARTIFACTS-invalid-proof.jsonl" 1
VRF_ATOMIC_CAPTURE_DIR="$ARTIFACTS/callback-rollback" VRF_RUNTIME_SCENARIO=callback_failure \
  bash northstar/scripts/vrf-coordinator-runtime-test.sh "$ARTIFACTS" \
  "$ARTIFACTS-callback-rollback.jsonl" 1
```

For capture-bound Portal integration, configure the **actual runtime**, rather than
relabeling an existing capture:

```sh
VRF_PORTAL_SESSION_JSON="$PUBLIC_SESSION_JSON" \
VRF_PORTAL_SESSION_SIGNER="$LOCAL_THROWAWAY_VALIDATOR_SIGNER" \
VRF_PORTAL_SESSION_OPEN_WIRE="$SIGNED_LOCAL_OPEN_SESSION_WIRE" \
VRF_ATOMIC_CAPTURE_DIR="$ARTIFACTS/matching-session" \
  bash northstar/scripts/vrf-coordinator-runtime-test.sh "$ARTIFACTS" \
  "$ARTIFACTS-matching-session.jsonl" 3
```

Only disposable local keys are appropriate. The harness verifies the signed
OpenSession bytes, derived Portal session address, grid/TTL/fee cap/validator and
unchanged 149-byte session context. It constructs ER with that configuration and
calls existing `set_session_pda` before activation, enabling normal session access
checks. Collector metadata retains the public session snapshot/hash and actual
manager/program identities; no private key bytes or paths are exported. This
associates new ER-native execution with the selected local session, without
claiming delegation or settlement. A different ledger/validator/nonce requires
fresh matching captures, not JSON metadata substitution.

The versioned sidecar domain is `northstar.er-vrf-candidate-capture.v1`. Capture
requires a single canonical signed legacy transaction with one explicit compute
limit and pinned consumer request or Coordinator fulfillment. It retains exact
submitted wire, authoritative presence/absence of message and context accounts,
all three Program/ProgramData/ELF bindings, raw sysvar-cache bytes, active/inactive
features, reserved keys, full recent-blockhash ages/fees, and zero-fee/default-budget
policy. Missing/unknown/oversized/noncanonical inputs reject capture, never become
legacy proof input. Pinned ELF hashes match the fresh platform-tools v1.56 builds.

The observer runs inside the existing serialized ER bank-operation boundary after
access policy and before execution. It rechecks Bank prestate in the existing
pre-commit callback, retains actual loader-normalized InvokeContext inputs,
tentative effects/rollback accounts and raw return-data producer (including empty
data), and reads actual committed accounts before releasing the boundary. The old
history return-data filter stays unchanged. A drift rejection prevents a non-atomic
snapshot from being labeled complete. Loader rent-epoch normalization is retained
separately from Bank prestate and must be independently recomputed by replay.

Journal and sidecar timestamps use one collector monotonic nanosecond domain.
Collector files bind runtime binary/source-diff/genesis/features and program
identities; guest/key identities remain explicitly missing until actually built.
The sidecar records `execution_verified=false`: full interpreter replay and
candidate envelope/checkpoint binding remain separate operations, not trusted
trace assertions. New candidate proof kind 3/version 1 is separate from this
capture-format domain and the existing proof route.

For a separately retained harder fixture, `VRF_RUNTIME_MIN_TRIES=12` performs
bounded local seed selection against observed Clock/SlotHashes before submitting
an ordinary request. Use `VRF_RUNTIME_SLOT_MS=5000` to give that preparation a wider
slot window. The actual request alpha and reference try count must match; drift
fails the run and its artifacts must be retained. No request state, sysvar or proof
result is injected. This slower-slot diagnostic is a separately identified runtime
configuration, not part of the default 100-success window. Input selection reads
the current working Bank, not a possibly older RPC commitment view; actual alpha
agreement remains mandatory. The default 50 ms run admits transaction age 900
and retains Bank queue age 1800. A 5000 ms diagnostic instead admits age 9 and
retains 18. Do not relabel that slower fixture as the frozen 900-age guest policy;
its distinct context must be admitted explicitly or rejected.
