# VRF runtime conformance fixtures

These are CPU-only capability probes, **not a VRF implementation, Bilal's
application/consumer ABI, an execution proof, or hosted ER acceptance**. The
isolated Cargo workspace does not change the validator's workspace manifests,
Portal deployment defaults, or feature activation policy.

## What runs

`programs/vrf-runtime-probe` builds one SBF ELF, loaded at two distinct addresses:
parent probe and callback consumer. All keys, scalars, account images and inputs
are public test fixtures. No oracle secrets or production nonce generation are
involved. Do not use the fixture program in production: its instruction modes
intentionally include unsigned invocation and callback failure.

The curve instruction checks compressed Ristretto basepoint known answers:

- valid generator and rejected `[0xff; 32]` encoding;
- `2 * G = 2G`, `G + 2G = 3G`, `3G - G = 2G`;
- two-term MSM `[1, 2] * [G, G] = 3G`;
- SHA-256 of `northstar-vrf-runtime-probe-v1` against a fixed digest.

This is an operation/conformance workload, not a DLEQ equation or verifier CU
benchmark. Non-Ristretto curves, scalar arithmetic variants, hash-to-point,
MSM maximum lengths and invalid curve-ID behavior are not tested.

The callback fixture checks a pending request nonce, stored consumer address,
account owners/writability, and PDA authority derived from
`[b"vrf-runtime-probe", queue_address]` under the parent program. The consumer
independently binds the queue owner to its stored parent and verifies the PDA
signer. Success clears pending state and records nonce, callback count and
observed Clock slot without changing account lengths, owners or lamports.
The fixture propagates CPI failure after both programs have written data.

The Northstar Bank harness executes the **SBF programs**, simulates for CU, then
processes transactions to assert success effects and complete queue/consumer
rollback on errors. Cases cover unsigned CPI, direct callback without authority,
wrong PDA, wrong consumer, wrong stored parent, read-only accounts, nonce
mismatch, same-slot rejection, propagated callback failure and duplicate request
rejection. These are fixture invariants, not newly discovered validator bugs.
No general runtime source changes were needed.

## Clock and feature boundaries

Clock is passed as a read-only account, with canonical address, owner and
40-byte layout checked on-chain. This avoids accidentally importing the newer,
feature-gated generic sysvar syscall through `Clock::get()`. **Sysvar syscall
availability is not tested.** The program does not import remaining-CU, return
data, or logging syscalls solely for instrumentation. CU comes from Bank/RPC
simulation metadata.

The ELF's undefined imports in the recorded build were:

```text
sol_try_find_program_address  sol_sha256  sol_log_  sol_panic_  abort
sol_curve_validate_point     sol_curve_group_op
sol_curve_multiscalar_mul    sol_invoke_signed_rust  sol_memcpy_
```

The Bank tests construct parent banks with and without the legacy curve feature
account before creating Northstar isolated ER banks. They check inherited feature
metadata and exercise the same ELF in both environments. Agave 4.5.0-alpha.4
registers curve syscalls unconditionally, so both executions must succeed. The
feature-disabled rejection recorded below belongs to the older alpha.2 runtime.
The tests do not change feature activation or `Bank::feature_set` after
syscall-environment construction.

The RPC client reads the three feature records from **the supplied endpoint**:
curve enablement, historical MSM-length compatibility, and invalid-curve
compatibility. A record's presence or absence is only metadata. It does not
establish effective execution availability. `curveExecution` reports the actual
same-endpoint fixture simulation outcome separately; a failure is not by itself
a diagnosis of a missing curve feature. Malformed feature-account results or
encodings remain explicit unavailable/unrecognized observations; they do not
silently become absent accounts or discard the rest of the endpoint report.

The client records processed/confirmed/finalized RPC slots, Clock fields and
SlotHashes contents. It does not assume:

- ER slots equal backing L1 slots, or Clock equals every RPC context slot;
- SlotHashes describe ER transaction history or provide private randomness;
- slots have a fixed duration or imply a wall-clock TTL;
- idle ERs publish empty blocks or keep advancing subscription streams.

The positive callback uses request slot zero; the RPC negative uses `u64::MAX`.
The Bank test separately checks equality with its current Clock slot. This tests
comparison semantics, not the application's eventual request-slot domain.

## Build and local Northstar Bank checks

Run from the validator repository root. Dependencies are pinned by the fixture's
own `Cargo.lock`. Only the Bank harness depends on Northstar path
crates. The harness is a separate workspace member, not part of the SBF package.
Use `CARGO_BUILD_JOBS=2` when sharing the machine. Set `CARGO_TARGET_DIR` for host
commands if you want compatible existing artifacts; it does not change the
fixture build helper's output location.

```bash
export CARGO_BUILD_JOBS=2
manifest=northstar/programs/vrf-runtime-probe/Cargo.toml
cargo test --locked --manifest-path "$manifest" --workspace
bash northstar/scripts/vrf-runtime-build-sbf.sh
BPF_OUT_DIR="$PWD/northstar/programs/vrf-runtime-probe/target/deploy" \
  VRF_RUNTIME_BANK_RESULT="$EVIDENCE_DIR/bank.json" \
  cargo test --locked --manifest-path "$manifest" --workspace -- --include-ignored
cargo clippy --locked --manifest-path "$manifest" --workspace --all-targets -- -D warnings
cargo +nightly fmt --all --manifest-path "$manifest" -- --check
bun test northstar/scripts/vrf-runtime-rpc.test.mjs
bash -n northstar/scripts/vrf-runtime-build-sbf.sh
```

Create `$EVIDENCE_DIR` first. The first Cargo command runs three native tests and
leaves the SBF integration test ignored. The second test command explicitly runs
that integration test too. Neither starts a validator or contacts a chain.
Cargo may fetch normal build dependencies.

The helper requests installed platform-tools v1.56 and SBPFv0; it does not use the
Portal wrapper/defaults. If the CLI wrapper cannot start but v1.56 is already
installed, use:

```bash
VRF_PLATFORM_TOOLS="$HOME/.cache/solana/v1.56/platform-tools" \
  bash northstar/scripts/vrf-runtime-build-sbf.sh
```

This invokes the installed platform Cargo/Rust directly for
`sbpf-solana-solana`, then copies the ELF to the same `target/deploy` directory.
It neither installs nor patches tools. Check the compiler version independently.
The recorded host's `cargo-build-sbf` 4.4.0 tried to inspect an unusable v1.57
compiler before parsing `--tools-version`; the explicit installed-v1.56 path
worked. No persistent toolchain or sandbox settings were changed.

## Read-only qualification path for a supplied ER

First obtain the **actual ER HTTP and WebSocket endpoints**, session identity,
validator revision/configuration, account/delegation setup and access policy from
the operator. `--target-kind er` is an operator assertion, not verified identity.
The client never redirects to backing L1 and never infers ER readiness from it.
Endpoint URLs may contain credentials: reports retain only the host and a URL
SHA-256, not the URL path, query or credentials. Keep command lines and evidence
files private where endpoint access is sensitive.

Metadata-only mode requires no fixture deployment:

```bash
bun northstar/scripts/vrf-runtime-rpc.mjs \
  --rpc "$ER_RPC" --ws "$ER_WS" --target-kind er \
  --output "$EVIDENCE_DIR/er-metadata.json"
```

This explicitly reports execution as untested. To exercise execution, the
operator must already have provisioned **these conformance fixtures** on a
permitted development ER. This client does not deploy, create, fund, delegate,
sign or send transactions. No public-chain setup is authorized by these commands.

Generate the public fixture manifest:

```bash
bun northstar/scripts/vrf-runtime-rpc.mjs --print-fixture > "$EVIDENCE_DIR/fixture.json"
jq '.config' "$EVIDENCE_DIR/fixture.json" > "$EVIDENCE_DIR/config.json"
```

The config has five base58 fields: `probeProgram`, `consumerProgram`, `queue`,
`consumerState`, `payer`. Defaults are deterministic addresses from bytes 81–85.
Replace them with operator-provisioned addresses, then regenerate expected images:

```bash
bun northstar/scripts/vrf-runtime-rpc.mjs --print-fixture "$EVIDENCE_DIR/config.json" \
  > "$EVIDENCE_DIR/fixture.json"
```

Provisioning requirements:

- Load the built fixture ELF under both distinct program addresses. Bind those
  deployed addresses and upgrade authorities to the expected ELF hash in the
  operator's evidence. The client checks executability, **not deployed ELF
  identity**. A matching simulation result assumes the intended fixture code.
- Queue is owned by the parent: 32-byte consumer ID, little-endian `u64(7)`,
  pending byte `1` (41 bytes total).
- Consumer state is owned by the consumer: 32-byte parent ID, then zero nonce,
  zero count, zero observed slot (56 bytes total).
- `--print-fixture` emits exact base64 images with 10,000,000 test lamports each.
  The fixture does not initialize accounts. Arrange actual ER access/delegation
  separately; successful Bank account injection is not delegation acceptance.
- Supply an existing funded system account as payer. Its private key is not
  needed. The authority PDA is virtual; it need not have allocated data.
- RPC must permit signature-disabled simulation with recent-blockhash
  replacement, returned account images and requested inner-instruction metadata.
  Incompatible RPC behavior is reported, not worked around through L1.

Then run:

```bash
bun northstar/scripts/vrf-runtime-rpc.mjs \
  --rpc "$ER_RPC" --ws "$ER_WS" --target-kind er \
  --config "$EVIDENCE_DIR/config.json" --timeout-ms 5000 \
  --output "$EVIDENCE_DIR/er-execution.json"
```

The client builds unsigned legacy transactions with a zero signature and requests
`sigVerify:false`, `replaceRecentBlockhash:true`. It uses only `getVersion`,
`getGenesisHash`, `getHealth`, `getSlot`, account reads and `simulateTransaction`.
There is no `sendTransaction` code path. Every HTTP response must be a single
JSON-RPC 2.0 object with the exact issued numeric request ID, exactly one of
`result`/`error`, and a well-formed error object when present. Malformed or
uncorrelated replies are recorded with `protocolError:true` and cannot establish
observations, fixture preconditions or passed simulations.

It validates initial images, 11 positive and negative simulations, success
post-images and reported CU. Missing CU remains
`unavailable`; no native execution estimate is substituted. Failed simulation
account images are often null: negative RPC cases establish rejection, **not
committed rollback**. The Bank harness establishes fixture rollback separately.

Exit 1 means fixture preconditions failed or a simulation did not match its
expected outcome. Exit 0 is not ER qualification: inspect individual RPC errors,
CU availability and subscription observations. With no config, no execution is
attempted. `productionAcceptance` is always `not_established`.

Subscriptions are tested only when `--ws` is explicit. `slotSubscribe`, queue
`accountSubscribe` and consumer `programSubscribe` acknowledgments and notification
counts are separate fields. Without config, Clock and its owner substitute only
for subscription transport observations. The client unsubscribes/closes after
the bounded observation window. Acknowledgments require the exact outstanding
numeric request ID and the same envelope checks as HTTP; boolean/string IDs are
never coerced into request indices. Notifications must match an acknowledged
numeric subscription ID and its method. Malformed or unrelated frames are
reported as protocol errors, not acknowledgments or delivered notifications.
An acknowledged but idle subscription is neither a delivery pass nor a failure.
Reconnect/reconciliation, load, transaction-driven
account/program notifications and production reliability still need an approved
live write test.

## Optional local JSON-RPC rehearsal

This validates client wire encoding against an **ordinary local Agave validator**,
not a Northstar ER. It needs an already installed `solana-test-validator` and the
built ELF; no full validator build or public network is needed. Starting from the
generated default fixture manifest above:

```bash
jq '.accounts[0]' "$EVIDENCE_DIR/fixture.json" > "$EVIDENCE_DIR/queue.json"
jq '.accounts[1]' "$EVIDENCE_DIR/fixture.json" > "$EVIDENCE_DIR/state.json"
elf="$PWD/northstar/programs/vrf-runtime-probe/target/deploy/northstar_vrf_runtime_probe.so"
probe=$(jq -r '.config.probeProgram' "$EVIDENCE_DIR/fixture.json")
consumer=$(jq -r '.config.consumerProgram' "$EVIDENCE_DIR/fixture.json")
queue=$(jq -r '.config.queue' "$EVIDENCE_DIR/fixture.json")
state=$(jq -r '.config.consumerState' "$EVIDENCE_DIR/fixture.json")
payer=$(jq -r '.config.payer' "$EVIDENCE_DIR/fixture.json")
tmux new-session -d -s vrf-runtime-local "$(command -v solana-test-validator)" \
  --ledger "$EVIDENCE_DIR/ledger" --reset --bind-address 127.0.0.1 \
  --rpc-port 18998 --gossip-port 18996 --faucet-port 18997 --dynamic-port-range 19000-19030 \
  --mint "$payer" --bpf-program "$probe" "$elf" --bpf-program "$consumer" "$elf" \
  --account "$queue" "$EVIDENCE_DIR/queue.json" --account "$state" "$EVIDENCE_DIR/state.json"
```

Use free ports and a new ledger directory. Poll `getHealth` until it returns `ok`,
then run the client with `http://127.0.0.1:18998`, `ws://127.0.0.1:18999`, and
`--target-kind l1`. Capture results and stop the service:

```bash
tmux kill-session -t vrf-runtime-local
```

## Observed CPU results, 18 October 2026

Baseline: `5d38818feca3d757bc39549580968f17d1755dd2` (Northstar Agave
4.5.0-alpha.2). Host Rust 1.98.1; installed platform-tools v1.56 Rust
1.89.0-dev, SBPFv0. Four focused Rust tests passed, including the explicit SBF
integration test (13 result rows plus duplicate-pending rejection). The original
benchmark/review worktree was read only; no licensed reference verifier was
copied.

- Curve known answers: **7,138 CU** in the isolated Northstar ER Bank.
- Same ELF with curve feature disabled in its parent: rejected with
  `ProgramFailedToComplete`; all 200,000 allocated CU reported consumed. This
  failure-path value is not the curve workload cost.
- Successful signed callback: **7,399 CU**; Bank slot and observed Clock slot
  both 1 in this fixture.
- Callback writes followed by propagated custom failure: **7,171 CU**; both
  stored account images unchanged after processing. Other rejection cases and
  stored-parent binding passed too.
- The 52 Bun mock/encoding tests passed, including malformed-response correlation
  and feature-decoding regressions. Mocks are client tests, not ER evidence.
- Local ordinary `solana-test-validator` **4.0.3**: all 11 RPC fixture simulations
  passed; curve/callback CU matched 7,138/7,399. Clock/context slot was 16;
  processed/confirmed/finalized RPC slots were 16/16/0 before the run. SlotHashes
  contained 16 entries with padded account data; this is not an ER observation.
- Local 1,500-ms WebSocket window: all three methods acknowledged, four slot
  notifications, zero queue notifications and zero consumer-program notifications.
  No transactions were submitted; no account/program delivery claim follows.

These CU numbers measure this small fixture's whole instruction simulation, not
real VRF fulfillment, callback latency, Portal resolution, or production limits.
The raw unstripped ELF SHA-256 in this build was
`981fdeb93f50415319218906bac1b8ff57b00cc7ead905db3d0558615ff07f08`.

**No actual hosted ER endpoint was supplied or tested.** Required next evidence:
operator-bound ER/session identity; deployed fixture/program identity; actual
application program/SDK revisions and request/account fixtures; ER access and
Portal delegation; real callback/state/fee behavior; subscriptions under actual
transactions and recovery; settlement/undelegation; history/witness/proof coverage.

Atomic callback propagation is only this fixture's chosen behavior. Production
atomic-versus-separate acceptance, slot domains/TTL, and application authority/
account layouts need interface sign-off. The existing execution-proof relation
still does not cover VRF curves or callback CPI. Nothing here satisfies the three
real Portal-resolution gate, <=120s proof generation on one L40S (including
wrapping), <=130K CU `ResolveChallenge(Production)`, or independent acceptance.
