# Bounded actual SBPF replay (experimental)

The interpreter engine reuses pinned Agave program loading, memory translation,
account serialization, CPI, signer/privilege checks, System instructions, curve
syscalls, return data and SVM post-transaction rent checks. It does not import a
Bank, AccountsDB, protobuf or a supplied execution trace. Native qualification
forces the interpreter instead of silently measuring a JIT.

Only the pinned Coordinator, reference verifier and test consumer ELFs are
allowed. The development feature profile enables only the curve25519 feature.
The signed transaction must contain a compute-limit prefix followed by one
consumer request or Coordinator fulfillment. Alternate loaders/programs,
lookup tables, durable nonce, nonzero transaction fees, unsupported runtime
configurations and unencoded failures reject. This is not generic SVM support.

`AgaveReplayEngine` implements the separate candidate envelope's execution
interface. The envelope authenticates signed wire, account absence, program and
sysvar bytes, profile/runtime context and checkpoint inclusion. The engine
recomputes loading defaults and complete transaction-atomic account effects.
The final guest requires a reviewed profile digest at build time; it never reads
its policy from the witness. Kind 3/version 1 uses the unchanged eight-field
public-input order and a distinct domain/key. The legacy route is unchanged.

Optional native utility (`--features cli`):

- `vrf-sbpf-replay identities`: compiled config and pinned program identities.
- `vrf-sbpf-replay execute-input INPUT_BORSH OUTPUT_BORSH`: native diagnostic
  execution only; no checkpoint acceptance and no generated proof.
- `vrf-sbpf-replay verify-envelope ENVELOPE_BORSH EXPECTED_PROFILE_SHA256`: native
  execution plus complete envelope binding, still not a generated proof.

`vrf-candidate-binding` provides the fresh Portal harness's canonical one-step
producer/validator. `produce CAPTURE_JSON SESSION_JSON FRESH_OUTPUT_DIR` replays an
authoritative atomic sidecar, compares all observed effects, builds the witness
checkpoint with the shared binding library, verifies it again using the real
engine, and writes the envelope, public inputs and binding manifest. Its validator
mode is exactly `BINDING_JSON SESSION_JSON`; it rebuilds and compares every artifact
before returning the integration harness's digest receipt. It refuses a capture
without the matching real ER session/grid association. It never relabels a capture
or builds roots from an asserted trace. The profile fixes one checkpoint step,
zero transaction fees, 32-KiB heap, the curve-only feature set and transaction age
900 (not the queue-retention bound 1800).

The output remains native execution/binding evidence, not a generated proof or
independent capture-provenance certificate. Captures with synthetic development
genesis/funding do not establish delegation or L1 settlement.

Cross-layer settlement and production rollout are unsupported. Native results,
SP1 execution, proving performance and Portal verification are separate gates.
