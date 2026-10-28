# Fresh bounded-VRF Portal lifecycle (local development)

This harness submits fresh signed L1 transactions to the actual Portal SBF. It does not restore a retained resolver account fixture, inject a challenge outcome, select TestOnly verification, modify the VRF library, or claim hosted acceptance. ER execution is supplied by the separate authoritative runtime capture and actual interpreter binding pipeline. Its absence is a blocker, not permission to invent roots.

The candidate is **kind 3, version 1**, behind `zk-verifier-bounded-vrf`. The existing eight public fields and 149-byte session-context source remain unchanged. Kind 3 binds the candidate transaction-tree root directly to the checkpoint, uses the candidate ER readonly/program/sysvar/profile context (not claimed L1 provenance), and fixes a commitment to no cross-layer settlement. Its CreateStepProof legacy transaction path must be entirely zero. The candidate guest authenticates its own transaction inclusion path. Legacy proofs do not qualify.

## Prerequisites

Run inside the Northstar sandbox (`NORTHSTAR_SANDBOX=1`). Use already available Bun, tmux, curl, jq, flock, the read-only TypeScript SDK with its installed dependencies, a built Northstar `solana-test-validator`, and the actual matching Portal SBF. No package installation is performed.

A **reviewed native canonical validator**, backed by the actual interpreter and candidate binding implementation, must validate capture conversion, complete state continuity, signed wire, pinned programs/runtime/profile, session source and candidate public values. The harness requires its executable SHA-256. The engine's `vrf-candidate-binding` CLI supplies the actual implementation: `produce CAPTURE_JSON SESSION_JSON FRESH_OUTPUT_DIR` creates the bundle, and its two-argument mode validates it. Use a reviewed immutable executable matching the guest policy, not a mutable shared Cargo output. An arbitrary JSON assertion or a binding-only fixture engine must never be substituted. A successful adapter receipt alone is not independent execution evidence; the actual guest/proof/Portal dispatch must agree.

Until a real guest/key/profile is configured, the Portal route remains explicitly unavailable. Use an immutable configured SBF and its reviewed SHA-256 for actual candidate cases; never reuse a ledger after changing the deployed key/artifact. A real SDK-verified preview proof is not evidence for a new session or for the three-case timing gate. Setup can be tested against the existing SBF, but candidate preparation/resolution must not be reported successful through a legacy build.

## Commands

All paths below are caller-supplied artifacts, not checked-in machine locations. Keep `WORK` private: it contains throwaway wallet secrets and a complete validator ledger. Never publish the whole directory.

```sh
export NORTHSTAR_TS_SDK_DIR="$SDK_WITH_NODE_MODULES"
export NORTHSTAR_LIVE_VALIDATOR="$BUILT_NORTHSTAR_TEST_VALIDATOR"
export NORTHSTAR_LIVE_PORTAL_SBF="$ACTUAL_CANDIDATE_PORTAL_SBF"

bash northstar/scripts/vrf-candidate-portal-local.sh init "$WORK"
# Export WORK/public/session.json to the canonical capture/binding producer.
# Produce a manifest, real authoritative captures and signed-wire artifacts.
bash northstar/scripts/vrf-candidate-portal-local.sh prepare "$WORK" \
  --binding "$BINDING_JSON" \
  --binding-validator "$CANONICAL_VALIDATOR" \
  --binding-validator-sha256 "$REVIEWED_VALIDATOR_SHA256"
# Generate the actual candidate proof for these exact public inputs.
bash northstar/scripts/vrf-candidate-portal-local.sh resolve "$WORK" \
  --binding "$BINDING_JSON" \
  --binding-validator "$CANONICAL_VALIDATOR" \
  --binding-validator-sha256 "$REVIEWED_VALIDATOR_SHA256" \
  --proof "$ONCHAIN_PROOF_BIN" --public-inputs "$PUBLIC_INPUTS_BIN"
```

For an already managed loopback validator, invoke `bun northstar/scripts/vrf-candidate-portal-lifecycle.mjs PHASE --work "$WORK" --rpc "$LOOPBACK_RPC" --portal-sbf "$ACTUAL_CANDIDATE_PORTAL_SBF" ...` directly. RPCs outside loopback are refused. SDK session serialization/parsing is reused read-only; challenge instructions use the current Portal Borsh layout.

The shell wrapper starts only its own detached tmux service, polls health/slots, preserves full private logs and exact exit status, waits for a durable snapshot after successful or failed phases, then stops that service. It retains the CLI exit separately from any snapshot/shutdown failure; failed on-chain effects are not silently discarded. The same ledger resumes in subsequent phases. It never resets a ledger, changes a challenge deadline, warps a bank, or passes a native Portal processor instead of SBF. An expired challenge requires a genuinely fresh run. A missing durable snapshot fails the phase; it is not reported restart-safe. Ports default to RPC 18999, faucet 19900 and gossip/dynamic range 20000–20050; override RPC/faucet with `NORTHSTAR_LIVE_RPC_PORT`/`NORTHSTAR_LIVE_FAUCET_PORT` and serialize runs.

`inspect` checks that session identity, full session bytes, genesis hash and deployed ELF identity still match after restart. It performs no Portal mutation. Candidate `prepare` resumes a partially submitted matching checkpoint/challenge; already executed transactions are not represented as fresh submissions. Any mismatch fails closed. A partially uploaded matching proof can resume; bytes cannot silently change.

## Artifact interchange

`init` exports `northstar.vrf-candidate-portal-session.v1` in `public/session.json`:

- `program_id`, `session`, `grid_id`, `nonce`, `validator`, `challenger`;
- `session_context_preimage_hex`: exactly 149 bytes, without profile salt;
- `session_account_sha256`, `created_at_slot`, `ttl_slots`, `genesis_hash`.

All u64/u128 values are canonical decimal strings. Hex is lowercase without `0x`. The producer must use actual session identities before guest input/proof generation. It must not choose a session-context hash independently.

`northstar.vrf-candidate-portal-binding.v1` contains:

```text
schema, proof_kind=3, proof_version=1
program_id, session, grid_id, nonce, validator, session_context_preimage_hex
er_slot: decimal string
step_index: integer, 0 <= step_index < state_roots.length - 1
public_inputs_hex: exactly 256 bytes, existing eight-field order
state_roots: 2..17 canonical field hex strings in complete execution order
transaction_tree_root, readonly_context_root, settlement_effect_root: field hex
captures: one per checkpoint step, in the same order
  capture_file, capture_sha256
  transaction_file, transaction_sha256
  role: "request" | "fulfillment_callback"
```

Artifact paths resolve relative to the manifest. Captures must be authoritative atomic sidecars, not first-wave RPC observations. Each signed transaction must be different. A request and its fulfillment-with-callback are distinct signed transactions. Callback execution is atomic with fulfillment, never an invented separate transaction. The producer must replay and authenticate **all** roots, the candidate transaction tree and fixed no-settlement commitment. The harness independently hashes every referenced artifact and checks every public-field/selected-step equality. It derives Portal's trace SHA tree and bisection paths from those canonical ordered roots; it never generates state roots or a candidate transaction root.

The external validator is invoked without a shell:

```text
CANONICAL_VALIDATOR ABSOLUTE_BINDING_JSON ABSOLUTE_SESSION_JSON
```

It must return exit 0 and exactly one JSON object on stdout:

```text
schema: "northstar.vrf-candidate-portal-binding-validation.v1"
binding_sha256: SHA-256 of exact binding JSON bytes
public_inputs_sha256: SHA-256 of exact 256 public bytes
capture_sha256: ordered array equal to binding.captures[*].capture_sha256
execution_verified: true
```

Diagnostics go to stderr. The executable must implement actual interpreter replay; setting this boolean is not execution verification. Complete stdout/stderr are retained privately. Only status, executable digest and input digests enter public receipts. Interpreter failure, unsupported profiles, missing artifacts, timeout, malformed output or binding-only validation stop before checkpoint mutation.

The DA payload is the exact ordered capture bytes, length-prefixed and SHA-256 domain-separated by `northstar-vrf-candidate-capture-da-v1`. The nonzero reveal identity uses `northstar-vrf-candidate-capture-da-reveal-v1`, the payload commitment and ordered capture digests. This is an explicit local-development availability commitment, **not** a DA-layer inclusion certificate. Portal currently checks commitment equality and a nonzero reveal hash; no stronger availability claim is made.

## Submitted lifecycle and outcome checks

1. Fresh throwaway validator/challenger wallets receive local faucet funding.
2. `OpenSession` creates the real singleton session and vault. Existing sessions are refused by `init`.
3. `ProposeCheckpoint` creates the real checkpoint/cursor and locks the actual 1,000,000-lamport proposer bond.
4. `OpenChallenge` creates the real challenge and missing DA proof.
5. Real validator responses authenticate trace midpoints. Real challenger bisections isolate the selected step; the final response enters Prove and reveals the exact capture commitment.
6. `CreateStepProof` creates kind3/version1 metadata with the canonical empty legacy transaction path.
7. `WriteStepProof` uploads the actual 356-byte envelope in chunks of at most 128 bytes. `SealStepProof` fixes its actual SHA-256.
8. `ResolveChallenge(Production)` runs through full current Portal dispatch, with a 130,000-CU transaction limit. The harness measures the whole outer Portal instruction from invocation logs, not an internal verifier marker or transaction-total field.

A successful transaction is **not** sufficient: Production can successfully process an invalid proof by slashing. Success here requires Pending checkpoint with `challenge_resolved=true`, ValidatorWon challenge, unchanged Locked bond and checkpoint balance, no challenger payout, and unchanged active cursor identity. Bond release occurs only through later checkpoint lifecycle rules and is not invented by this harness. No cross-layer settlement is attempted.

Use three distinct private ledgers/session identities for the three consecutive fresh resolution samples; the singleton session and active-checkpoint lifecycle are not bypassed. The singleton PDA address is necessarily identical across these ledgers for the same Portal program. Do not falsify `session.session` to manufacture different addresses. Prepared records retain the real PDA and additionally include `session_source_sha256` over exact session JSON, `session_identity_sha256`, and the deployed `portal_program` identity. The qualified session digest is SHA-256 over `northstar-local-portal-session-identity-v1`, the UTF-8 genesis-hash length as u32 LE, the genesis hash bytes and the actual 149-byte context source. Consumers must qualify session/checkpoint/challenge addresses by the real ledger/session identity, not require singleton addresses to differ. Include at least one request and one fulfillment-with-callback across those samples. Keep the actual 12-try fulfillment as an additional corpus/bound case, whether or not selected for the three measured samples. Every proof must bind its corresponding real capture; three copies of one proof/capture do not qualify. GPU prove+wrap <=120 seconds is measured by the real GPU client on the approved g5.xlarge/A10G, not this L1 harness.

## Three-case proving handoff

The local orchestration file `northstar.vrf-candidate-portal-cases.v1` contains `portal_sbf_sha256` and exactly three `cases`. Each case supplies `case_id`, `status: "prepared"`, `work` and `binding_json`; local paths resolve relative to this orchestration file and must remain outside publication-ready source. Runtime can use an earlier status such as `session-ready` while it creates new session-matched captures, but the batch packager refuses unprepared cases.

After all three real preparations:

```sh
export NORTHSTAR_VRF_BINDING_VALIDATOR="$IMMUTABLE_NATIVE_V8_VALIDATOR"
export NORTHSTAR_VRF_BINDING_VALIDATOR_SHA256="$REVIEWED_NATIVE_SHA256"
bun northstar/scripts/vrf-candidate-portal-batch.mjs "$LOCAL_CASES_JSON" "$FRESH_BATCH_DIR"
```

The packager requires a successful wrapper durability receipt whose snapshot covers the prepared record's actual `prepared_slot`, then reexecutes the actual validator for each case. Directly managed validators must retain equivalent real snapshot evidence before using this paused-chain batch path. It checks exact source/deployment/prepared/public/capture identities, distinct actual session contexts, distinct envelopes/public values, and coverage of both request and fulfillment-with-callback. It produces the SDK client's `northstar-vrf-sp1-batch-v1` manifest with three relative-path envelope/public/prepared sets and a separate compact provenance file. No signer, raw private logs, local-only paths or ledger is packaged. `warmup` is null; any separately declared warmup must stay outside the measured three-case window. The output does not claim a proof, Portal resolution or a timing pass. The actual GPU client still owns proof generation and measurements; the actual L1 resolver owns the state outcome.

## Receipts and limits

Each submitted Portal transaction is serialized and retained **before** RPC submission. Receipts include ordered account/instruction bytes, signature, exact returned base64 wire equality, full transaction metadata/logs, transaction CU, whole Portal instruction CU and failure details. Preparation and resolution retain complete account snapshots with their RPC context slot; each snapshot is one `getMultipleAccounts` observation. Failed submissions/uploads/resolutions remain alongside successful ones.

Run `bun northstar/scripts/vrf-candidate-portal-receipts.mjs "$WORK/public"` for an offline structural audit of signed receipts. It verifies transaction signatures, exact RPC wire equality, ordered instruction identities, recorded CU/outcomes, and Production-only resolution mode. It preserves unconfirmed submissions and never turns self-consistent RPC JSON into a consensus or proof certificate; its `candidate_acceptance` is always `not_established`.

Only carefully reviewed `WORK/public` receipt files are candidates for an offline evidence packet. Do not include wallet files, raw private logs, local paths, full ledgers or credentials. Hashes/signatures/compact outcome receipts do not by themselves authenticate an RPC server; independent reproduction and the real proof check remain required. Baixiong acceptance, Jonas reproduction, fixture-owner review and substantive team responses remain pending unless obtained from those people.

Codec-only tests use labeled synthetic values and do not establish execution or cryptographic proof validity:

```sh
node --test northstar/scripts/vrf-candidate-portal-codec.test.mjs
node --test northstar/scripts/vrf-candidate-portal-batch.test.mjs
# Requires the same read-only NORTHSTAR_TS_SDK_DIR dependency directory:
node --test northstar/scripts/vrf-candidate-portal-receipts.test.mjs
bash -n northstar/scripts/vrf-candidate-portal-local.sh
node --check northstar/scripts/vrf-candidate-portal-lifecycle.mjs
```
