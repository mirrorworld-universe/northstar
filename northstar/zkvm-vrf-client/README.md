# Runtime-ELF bounded VRF SP1 client

Development only. This separate workspace never embeds the legacy guest, selects a network prover, installs a key or enables Portal acceptance. The old replay workspace is unchanged.

## Inputs and commands

Every invocation names an explicit `--elf`, reviewed `--profile`, and **nonexistent** `--out` directory. The profile contains the actual ELF SHA-256, `proof_kind: 3`, `proof_version: 1`, the existing ordered eight `public_input_fields`, `production_acceptance: false`, and `guest_status: "authenticated-envelope"`. `policy_profile_sha256` separately names the canonical typed policy digest compiled into the guest. The deployment JSON includes the resulting ELF hash and therefore must **not** be used as the compiled policy digest (that would be circular). Receipt `profile_sha256` is the deployment JSON-byte hash; the policy digest is recorded and checked separately. Extra reviewed policy fields may be present. Labels are not a guest-semantic attestation: independently review the guest, policy and generated key. Never relabel the always-reject compilation guest to get a candidate receipt.

```sh
"$REVIEWED_LAUNCHER" env SP1_PROVER=cpu "$CLIENT" key \
  --elf "$ELF" --profile "$PROFILE" --out "$NEW_KEY_OUTPUT"

"$REVIEWED_LAUNCHER" env SP1_PROVER=cpu "$CLIENT" execute \
  --elf "$ELF" --profile "$PROFILE" --input "$ENVELOPE" \
  --public "$EXPECTED_PUBLIC" --setup-receipt "$SETUP_RECEIPT" --out "$NEW_EXECUTE_OUTPUT"

# One reviewed launcher holds the device lock and bounded memory scope.
"$REVIEWED_LAUNCHER" "$CLIENT" groth16 \
  --elf "$ELF" --profile "$PROFILE" --input "$ENVELOPE" \
  --public "$EXPECTED_PUBLIC" --setup-receipt "$SETUP_RECEIPT" \
  --samples 3 --cache-state verified-present --out "$NEW_PROOF_OUTPUT"

"$REVIEWED_LAUNCHER" env SP1_PROVER=cpu "$CLIENT" verify \
  --elf "$ELF" --profile "$PROFILE" --proof "$SERIALIZED_SDK_PROOF" \
  --public "$EXPECTED_PUBLIC" --setup-receipt "$SETUP_RECEIPT" --out "$NEW_VERIFY_OUTPUT"
```

`key`, `execute` and `verify` require CPU backend (unset defaults to CPU). `groth16` requires explicit CUDA backend and a CUDA-enabled build. Network/hosted/mock/light and unknown backends reject before SDK initialization. The SDK network feature is not enabled. Do not take the same device lock twice around a lock-owning launcher.

The input file contains canonical Borsh envelope bytes consumed with `SP1Stdin::write_vec` / guest `read_vec`. The authenticated guest owns canonical envelope parsing, execution and checkpoint/profile binding. The client does not substitute a host transition model. Expected public bytes come from the canonical envelope/integration prepare record, not a guessed state. They must be exactly 256 bytes, canonical BN254 fields, kind3/version1, with all required commitments present. Every actual execution and wrapped proof is compared byte-for-byte.

Every non-key command independently repeats SDK setup for the given ELF and compares program key, serialized verifying-key hash, ELF hash and profile hash with the supplied setup receipt. `setup-receipt.json` uses `northstar-sp1-setup-v1` and `verifying-key.bin` is real bincode SDK output. A setup receipt is artifact consistency, not proof of execution or authorization to install a Portal key. Portal's key utility and independent review still own the actual key/fixed-fold handoff.

`execute --expect-rejection` is a negative control. It succeeds only after observing a **nonzero guest exit and empty public output**. An executor error, cycle-limit error, report object, or nonempty rejected output is not a successful rejection receipt. CPU execution is not a proof. Inputs, observed output, partial phases and errors remain in the fresh output directory.

Groth16 defaults to one sample; only one or three sequential repetitions of a single input are allowed in the simple mode. Repetitions are explicitly development controls, **not three distinct lifecycle cases**. It first executes the guest as a CPU control, then proves/wraps and SDK-verifies each result. Samples emit serialized SDK proofs, 356-byte Portal envelopes, 256-byte public inputs and hashes. The program key comes from real setup, never an arbitrary caller key. The strict Portal envelope excludes nonzero exit codes, wrong outer roots and TEE envelopes. `verify` accepts only serialized real Groth16 proofs and repeats SDK verification with the derived actual key.

## Three distinct prepared cases with one client/key/server

For the actual series, replace single-input flags with `--batch "$BATCH" --setup-receipt "$RECEIPT"`. Batch schema is `northstar-vrf-sp1-batch-v1`, with exactly three `cases` and an optional separate `warmup` case. Each entry has:

```text
case_id: bounded ASCII letters/digits/dash/underscore
input: relative canonical envelope file
input_sha256: exact envelope SHA-256
expected_public: relative 256-byte public-values file
public_inputs_sha256: exact public-byte SHA-256
prepare_record: relative real Portal prepared-record file
prepare_record_sha256: exact prepared-record SHA-256
```

The three declared cases require distinct IDs and input/public/prepared digests. Portal v1's session PDA is a **singleton**: genuine fresh ledgers can have the same literal session/checkpoint/challenge addresses. Do not falsify those addresses. The client instead recomputes:

```text
session_identity_sha256 = SHA256(
  "northstar-local-portal-session-identity-v1" ||
  genesis_utf8_length_u32LE || genesis_utf8 || actual_149_byte_context)
```

The context is rebuilt from the actual session fields (domain, Portal program, literal session PDA, u64 grid, u128 nonce, validator, version), compared with the declared preimage, and matched to the canonical Borsh envelope's session prefix. The guest still owns complete envelope decoding and authenticated execution. The declared identity must match the recomputation. `session_source_sha256` must match reconstruction of lifecycle v1's exact session JSON source: the closed `SessionSource` field order, two-space indentation and trailing newline. This format matches the real producer, not arbitrary JSON reserialization. The public setup vector in `fixtures/session-source-v1.json` checks byte compatibility with the integration codec; it is not a final-series prepared record.

Window uniqueness requires distinct qualified identities, actual 149-byte contexts and source digests. Merely changing a genesis label cannot count the same context twice. Checkpoint/challenge uniqueness is qualified by session identity; literal addresses are retained in measurements. All three cases must retain the same Portal program/deployed-ELF identity. The integration record must have schema `northstar.vrf-candidate-portal-prepared.v1`, kind3/version1, the matching public digest, full session fields, `session_source_sha256`, `session_identity_sha256` and the real `portal_program` receipt. Unqualified older records fail closed. Artifact paths cannot escape the manifest directory. These checks bind artifacts, not RPC truth or ledger consensus; the integration worker owns signed receipt verification and actual lifecycle outcomes.

One initialized SDK client and derived key persist across all cases. Warmup is outside the declared window and retained with `acceptance_window:false`; it never replaces an attempted case. Every case starts as `not-attempted`, then records started/failed/SDK-verified status, input/output hashes and actual prepared deadline. Failure stops the ordered series, preserving later `not-attempted` entries rather than selecting successes. Slow verified proofs remain in the series with a failed <=120s target. Unverified generated SDK proof bytes are retained separately if verification fails; they are never exported as accepted Portal artifacts.

The client is offline with respect to L1. Its deadline field is an artifact binding, **not a current-slot check**. The lifecycle orchestrator must prepare-before-proof and check the real 750-slot turn deadline before resolve. Do not pause/warp slots or bypass expired turns. If a prepared batch cannot finish within real deadlines, retain that miss and change orchestration, not the recorded timing or acceptance window.

## CPU control corpus with one client/setup

```sh
"$REVIEWED_LAUNCHER" env SP1_PROVER=cpu "$REBUILT_CLIENT" execute-corpus \
  --elf "$ELF" --profile "$PROFILE" --setup-receipt "$SETUP_RECEIPT" \
  --corpus "$CORPUS/manifest.json" --out "$NEW_CORPUS_OUTPUT"
```

This command reuses one CPU SDK client and one genuine ELF/key setup for the frozen `northstar-vrf-candidate-controls-v1` corpus: two positive baselines followed by fifteen controls, in manifest order. It generates no proof. The existing runtime ELF/profile/setup identity checks still apply; corpus policy/domain/count/age must match the supported v1 contract. The v1 generator and four binding-source identities are pinned to the reviewed frozen sources. Those provenance hashes are checked against reviewed constants, not mislabeled as fresh reads of absent source files.

Before SDK initialization, the client validates the complete manifest, unique IDs/paths/input hashes, control basis/public associations, all twenty referenced payload hashes (including the signed source wire), size bounds and canonical baseline public values. Relative paths and resolved symlinks cannot escape the corpus root. Hashes are rechecked immediately before each selected execution. The unchanged source manifest and its exact digest are retained. The public manifest in `fixtures/controls-manifest-v1.json` is a schema vector; its large witness files are not included.

Each case retains its input, expected public values, returned public values when present, actual SDK exit/cycle/syscall data, `cycle_tracker`, strict-observer outcome, error and wall time. A negative passes **only** after a returned nonzero u64 guest exit, empty output and successful strict observation. Host errors, panics, timeouts and missing output are never successful rejection. Positives require returned zero exit and exact canonical 256-byte output. An ordinary failed case does not hide later cases; a panic or 300-second execution timeout stops the sequence and leaves remaining entries unobserved. The timeout is an async wait bound, not proof that backend work was forcibly stopped; no next execution is launched after timeout. The parent launcher must also retain its whole-process timeout/exit.

`measurements.json` contains all seventeen ordered `corpus_cases`, including `not-attempted`/`not-selected` entries. `all_passed` stays false until all seventeen actually pass in this process. Repeat `--case-id ID` to resume a selected subset; source order/digest stay unchanged, prior receipts are never imported, and only `selected_all_passed` can become true for a partial run. Selection is not full-corpus acceptance. The source's explicitly skipped rollback control is still skipped, not fabricated coverage. Integration retains actual lifecycle/deadline ownership.

SDK 6.8.0's actual [`ExecutionReport.cycle_tracker`](https://github.com/succinctlabs/sp1/blob/v6.8.0/crates/core/executor/src/report.rs) is `hashbrown::HashMap<String, u64>`. Both single execution and corpus execution retain this map directly; an empty map is not invented diagnostic data. No new guest markers or relation changes are introduced. Rebuild/typecheck the SDK-enabled client before use; no-SDK parser/observer tests alone do not establish execution support.

## Measurement boundary

`measurements.json` separates initialization, setup, CPU execution, final prove+wrap and SDK verification. The prove+wrap timer stops only after Groth16 wrapping completes and excludes the separately timed SDK verification. The first proof is process-cold **after setup and CPU preflight**, not empty-cache provisioning. `--cache-state` is explicit caller-observed metadata; it is not inferred or independently verified by this client.

Each proof reports the <=120-second A10G target without discarding slower samples. Complete Portal resolution remains a separate <=130,000-CU integration gate. No CU or resource peak is fabricated; an absent resource measurement stays absent. A successful client run never grants production acceptance or claims a fresh Portal resolution. A crash may leave `status: started`; the external launcher must preserve its log/exit rather than treating incomplete output as success.

## Reproducible build

SP1 SDK is exactly 6.8.0, with CUDA and native Groth16 features opt-in through `cuda`. The lock was Cargo-normalized from the reviewed legacy replay seed. The normalized graph retains 48 `sp1-*`/`slop-*` packages at 6.8.0. The remote build independently repeats metadata/version/feature checks before its frozen `--locked` build. Audit **all** `sp1-*` and `slop-*` packages, not only the top-level SDK; all must remain reviewed 6.8.0 versions. Runtime checks also refuse internal-version drift. Keep `Cargo.lock` and use `--locked` after normalization. No SDK source patch is introduced by this workspace.

```sh
cargo test --locked --manifest-path northstar/zkvm-vrf-client/Cargo.toml \
  --no-default-features --test guards
cargo build --release --locked --manifest-path northstar/zkvm-vrf-client/Cargo.toml --features cuda
```

Actual CUDA compilation uses the coordinator-approved g5 host, two Cargo jobs, the existing Icicle headers/libraries, CUDA/Go prerequisites, cached Cargo dependencies and the existing PR #207 server/circuit/runtime prefixes. The reviewed device-lock launcher retains its resource scope and exact server/patch/configuration identity. Do not reinstall those prerequisites or overwrite legacy result directories. Run the checked-in `remote-build.sh SOURCE_ROOT GPU_PREFIX ICICLE_PREFIX FRESH_OUTPUT` through the coordinator. It requires the frozen lock even for metadata, reuses `ICICLE_PREFIX/target`, verifies the installed launcher/server/optimized patch/environment and outer-key hashes, and retains input/final lock hashes, full Cargo metadata, package/version/source audit, client hash, linker paths, logs and exact status. `remote-run.sh GPU_PREFIX CLIENT EXPECTED_CLIENT_SHA OUTPUT_PARENT COMMAND [ARGS...]` creates a unique private job, selects only CPU/CUDA, uses that same lock-owning launcher and retains its exact timeout/exit/log and hardware record; it owns the new `--out` path. Deterministic overlay archives use epoch mtimes, which do **not** invalidate Cargo's existing mtime fingerprints. The recipe therefore explicitly cleans both **release and dev** profiles of only the client package and its tiny first-party zk-types dependency under the same device lock before compiling; SP1/server/circuit caches stay intact. A source-byte/hash check alone does not establish which stale metadata Cargo linked. Raw build metadata/logs are not remote-ready publication artifacts; curate path-free identities separately.

The compiled client source digest includes `src/session.rs` and `src/corpus.rs` as well as the manifests/lock, other client modules and zk-types sources. The qualified-batch implementation is newer than the measured v5 binary; no-SDK tests do not establish SDK/CUDA compilation or three-case proving support. Rebuild and retain a new binary receipt before using it.

Tests marked synthetic use generated bytes only to exercise guards; they are not execution/proof fixtures. Actual request/fulfillment/callback, harder-try and incorrect-witness validation requires the authenticated guest and complete real captures. Those results are not implied by compilation or guard tests.
