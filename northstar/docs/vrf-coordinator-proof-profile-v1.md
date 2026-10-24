# VRF coordinator execution-proof profile v1 — experimental development candidate

Historical host diagnostic identifier: `northstar-vrf-coordinator-proof-profile-v1-draft`.
On 2026-10-23 the user selected bounded actual SBPF replay, authorized a separate
candidate witness/domain/guest/key and ER-native development integration, and
selected **g5.xlarge/A10G** as the measurement target. This supersedes the earlier
L40S wording and the unselected program-specific model below. No production
rollout, changed legacy acceptance, paid proving or external application ABI
change is authorized.

`northstar_transaction_proof::vrf_candidate` implements canonical bounded
Borsh data and binding orchestration without host runtime dependencies. Candidate
magic is `NSVRF001`, envelope version 1, proof kind 3/version 1. It calls the
actual interpreter through `CandidateReplayEngine`; host-provided output is
never execution evidence. The reviewed guest must wire the real Agave engine.
No successful candidate guest execution, proof or Portal resolution is established
by binding-only tests.

## Source and algorithm pins

- Northstar baseline: [`34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52`](https://github.com/mirrorworld-universe/northstar/tree/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52).
- Coordinator library: [`d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/tree/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd).
- The library's application suite value **1** selects RFC 9381
  ECVRF-EDWARDS25519-SHA512-TAI. Its RFC hash domain uses suite string **0x03**;
  these are different identifiers, not interchangeable constants. Proofs are
  80 bytes, outputs 64 bytes. The existing ristretto255 CPU example is not this
  algorithm. See [reference arithmetic](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/reference-verify/src/ecvrf.rs#L1-L217).
- Coordinator: `FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N`.
  Local/devnet reference verifier: `FUBz1nvSSoHnMqFoK2CA3jF5ckpYPdF3ZT4SoLzDNzh1`.
  Test consumer: `HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt`.
  These addresses are not ELF identities. The reference verifier is explicitly
  a test stand-in; the library does not configure a production mainnet verifier.
  See [build-time verifier pin](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/ids.rs#L1-L40).

## Existing gate: preserve it

[Bank snapshot admission](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/runtime/src/bank/er_replay.rs#L32-L58)
requires legacy wire, three message keys, one signature and one instruction with
program index 2 and account indices `[1]`. It also restricts fee structure and
requires an upgradeable-loader program. Snapshot presence is not a proof.

[Guest replay](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/transaction-proof/src/lib.rs#L206-L430)
checks a matching signed transaction, exactly two mutable account witnesses,
success only, a fixed first-byte write and exactly one `sol_memcpy_` syscall.
The frozen wire parser independently enforces the same narrow message shape.
[Witness assembly](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/transaction-proof/src/fixture.rs#L96-L112)
requires exactly one VM trace.

These checks reject the real coordinator account shape before curve or CPI
execution can be considered. Removing the wire guard would not implement
execution verification. In particular, existing load/store row checks establish
PC/register preservation rather than a general authenticated memory transition;
memcpy row checks establish return-register behavior, not arbitrary syscall
memory semantics. Runtime/program hashes in a supplied witness do not, by
themselves, authenticate loader identity or authorize a new runtime profile.
Do not reuse those assumptions as a generic VM proof.

The [current guest](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/zkvm-replay/program/src/main.rs)
only decodes this witness, calls this replay relation and commits its 256-byte
public inputs. The experimental candidate has separate types/domain and cannot
be decoded or admitted by that legacy route. Legacy Portal constants and keys
remain unchanged. The [Portal verifier](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/programs/portal/src/instructions/verifier.rs#L15-L69)
pins guest verification-key hash
`0063a10e31e90625b76d06bfa22cbf0a4a8fa0850ccf71e063d243bd1a7aa31b`
and uses a fixed-input Groth16 verifying key. A new guest would require reviewing
both program-key binding and the specialized verifier material, not merely
renaming a host profile or copying a new hash. Neither is changed here.

## Proposed bounded specimen

The initial candidate is **one local reference composition**, not arbitrary SVM:
coordinator + reference verifier + test consumer. Configuration, program loading
and `set_behaviour` are authenticated setup, outside the two challenged steps.
A deployment, delegation or cross-layer settlement policy is not selected here.
The library is L1-only; no shared delegated request queue is assumed.

Implemented envelope bounds and remaining runtime constraints are explicit:
these are resource/admission limits, not measured proving capacity.

- Legacy transactions only; one payer signature, no address lookup tables,
  durable nonce, additional signers, precompiles or arbitrary extra instructions.
- Exactly one `ComputeBudget::SetComputeUnitLimit` prefix and one application
  instruction. No compute-unit price, heap-request or extra instruction.
  Signed compute limit must equal runtime context and stay at most 1,400,000 CU.
  Heap is 32 KiB. The initial ER-native candidate permits only zero transaction
  and prioritization fees; the positive application escrow still executes via CPI.
- At most 10 unique message keys and 4 writable keys; duplicate logical account
  roles must resolve to the frozen key/index map. Permit payer/fee-payer alias
  in the request CPI, but no unspecified aliases. Keep fulfiller distinct from
  rent payer and callback state in the initial specimen.
- One callback account: the writable consumer slot. Empty callback arguments.
  Callback mode only, `Behaviour::Ok`; add `Behaviour::Fail` as a rollback specimen.
  No Store, Burn, Forward, expire, close-request or config-update execution.
- One request PDA starts absent (zero lamports/data), so initialization uses the
  captured System create-account path, not an alternate prefunded-PDA path.
  Use a positive fixed escrow fee and a frozen rent schedule.
- Request has at most four program invocations: consumer, coordinator, System
  creation, System fee transfer. Fulfillment has at most three: coordinator,
  verifier and consumer callback. Maximum invocation depth is three, counting
  the top-level program as depth one. No callback-initiated CPI.
- Bound each account's data, including ProgramData ELF bytes, to 4 MiB; all
  encoded envelope bytes to 16 MiB; account inputs to 64; full checkpoint state
  sets to 1024 values; feature lists to 1024 entries each; transaction wire to
  1232 bytes; checkpoint inclusion to 16 leaves. Borsh vector bounds are checked
  before allocation. Full interpreter execution replaces trusted trace rows.
  Captures that exceed limits reject; never truncate. The RFC try-and-increment
  loop remains bounded at 256 attempts; never assume a fixed successful count.
- Pin loader version, SBPF version, feature set, VM config, compute schedule and
  exact syscall inventory from actual builds/captures. SBPFv2 cannot be relabeled
  as the frozen SBPFv0 witness. Unknown syscall or opcode rejects the candidate.

Known pinned payload lengths for this specimen are: `fulfill`, 92 bytes
(8-byte discriminator + u32 vector length + 80-byte proof); verifier CPI,
157 bytes (8 + 1 + 32 + 32 + 4 + 80); callback, 104 bytes (8 + 32 + 64,
with no appended arguments). These are source ABI facts, not validations
performed by the host shape diagnostic. Request vectors/options must be frozen
byte-for-byte from the signed capture, including the callback account list.

The smallest request path is
`test_consumer::make_request -> coordinator::request_random -> System`.
The smallest fulfillment path is
`coordinator::fulfill -> reference_verify::verify`, then
`test_consumer::on_random`. Fulfillment and callback are one atomic transaction,
not two independently committed checkpoint steps. Request and fulfillment are
separate signed transactions, potentially in different checkpoints.

## Authenticated inputs and independently verified semantics

| Boundary | Required binding and computation |
| --- | --- |
| Signed message | Canonical full wire and all signatures; exact key/index order, writable/signer flags, program indices, instruction bytes and compute prefix. Authenticate blockhash eligibility, fees and runtime context independently; a signature alone supplies none of these. |
| Accounts | Authenticate pre-state membership, including nonexistence for request creation, every readonly observation, executable status, ownership, lamports and complete data. Canonicalize repeated instruction indices without granting new privileges. Verify unchanged accounts as well as all declared effects. |
| Program/loader | Bind coordinator, verifier, consumer, System and compute-budget semantics to exact approved artifacts/builtins, Program-to-ProgramData linkage, ELF hash, deployment slot and upgrade policy. Reject a different ELF at the same address. |
| Request derivation | Recompute requester identity under the consumer, request PDA under coordinator and all bump checks. Verify config status, suite/key snapshot, deadline limits, callback target/list and positive fee. Execute account allocation, assignment, rent debit and escrow transfer with checked arithmetic and ownership rules. |
| Clock/SlotHashes | Authenticate Clock fields used by instructions, sysvar owner/key, raw SlotHashes bytes and newest entry. Recompute `SHA256("sonic-vrf/v1" || coordinator || request || seed || request_slot_LE || hash_slot_LE || slot_hash)`. Bind ER slot and observed L1 provenance separately; do not treat a supplied slot as authenticated chain history. Freeze how ER obtains these sysvars. |
| ECVRF arithmetic | Verify canonical point encodings, key subgroup checks, Gamma, canonical scalar, SHA-512 software operations, every try-and-increment iteration, curve validate/multiply/multiscalar outputs and errors, challenge and beta. Bind input/output memory bytes and compute charges at syscall call time. A supplied result or successful native proof check is not an execution proof. |
| CPI privileges | Authenticate parent/child IDs, order, instruction bytes, account translation, per-call writable/signer subsets, duplicate handling, PDA seeds/bump/caller program, stack depth, errors and memory synchronization on entry/return. A requester PDA signature must originate in consumer execution; callback PDA signature must originate in coordinator execution. No privilege escalation or additional child invocation. |
| Return data | Verify set/get operations, producer program ID and exactly 64 bytes returned by the pinned verifier at the consumption point. Model clearing/overwriting through nested calls; final transaction return bytes alone are insufficient. |
| Fulfillment/callback | Verify registered fulfiller, Pending state, `request_slot < clock.slot <= deadline_slot`, exact rent payer and stored callback privileges. Serialize Fulfilled state before callback, bind callback discriminator/request/output, and verify consumer's request equality, output, slot and delivery-count update. |
| Effects/rollback | On success: fee to fulfiller, remaining rent to recorded rent payer, canonical request close/absence, exact consumer write and fee-payer debit. On invalid proof/callback failure: no successful callback/close/escrow effects persist; preserve Pending request and consumer state, except independently computed transaction fee/rollback accounts. Bound logs, CU and errors to execution, not annotations. |
| Checkpoint | Authenticate transaction inclusion/index, pre/post roots, readonly root, complete transaction effects, deletion/nonexistence and settlement effects under the session domain. Request state consumed by fulfillment must link through authenticated checkpoints, not a shared request key alone. Freeze how newly created/closed ER-native accounts appear in settlement; this draft does not authorize settlement or Portal ownership changes. |

Request source: [account shape and execution](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/instructions/request/request_random.rs#L57-L242).
Fulfillment source: [verification, callback and close](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/instructions/fulfillment/fulfill.rs#L24-L222).
Return binding: [producer/length check](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/utils.rs#L439-L454).
Consumer: [request and callback](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/test-consumer/src/lib.rs#L81-L179).

## Implemented candidate bindings

`vrf_candidate.rs` exports data-only Borsh types, bounded decoding, strict signed
legacy admission, profile/program/loader checks, complete effect comparison and
`verify_candidate(envelope, policy, engine)`. The engine crate depends on these
types with default features disabled; no runtime crate is imported here. The
policy is constructed from guest-reviewed constants, not witness-deserialized
hashes. Every program and ProgramData state is additionally authenticated in
state/readonly commitments, including loader ownership, executable flags,
Program-to-ProgramData linkage, deploy slot and full ELF bytes. Profile identity
includes the canonical program-identity digest, features and runtime policy.

Authoritative input is Bank prestate with explicit `None` absence. Loaded SVM
accounts are not trusted inputs: even with zero fees, the actual loader can
normalize writable rent_epoch and absent-account defaults. The engine must
recompute this from authenticated Rent/features, execute all bytecode/syscalls
and apply exact committed rollback. Captured loaded arrays are differential
observations only. Bindings permit failed transactions to change only fee-payer
rent_epoch to MAX, with all lamports/owner/data unchanged, and still demand exact
actual-engine output. Other tentative effects must not persist.

The public field order and 256-byte encoding remain unchanged, with explicit
kind-3 domain semantics:

1. `domain`: `FrBytes::er_step_domain_v1(3, 1)`; legacy wrapper rejects it.
2. `session_context`: unchanged Poseidon commitment to the existing 149-byte
   canonical session encoding. Profile binding stays in guest policy and the
   candidate readonly execution-context root; no session salt is added.
3. `slot_step`: bank ER slot and transaction index.
4. `pre_state_root`: canonical full pre-state values, authenticating absence.
5. `post_state_root`: that state with complete recomputed writable effects.
6. `tx_effect_root`: **candidate checkpoint transaction-tree root**, unlike
   kind 2's selected leaf. The inclusion leaf binds signed wire, profile,
   complete output/status, CU, fees, producer-bound return data and logs.
7. `readonly_l1_root`: candidate ER readonly accounts/programs/sysvars and full
   profile/runtime context. This is not a claim of authenticated L1 provenance.
8. `settlement_effect_root`: fixed commitment to no cross-layer settlement.

Candidate Merkle domains are separate from legacy. Transaction and readonly
context trees each have a bounded inclusion path; both bind the same step index
and leaf count, at most 16, equal to `profile.checkpoint_steps`. The reviewed
guest policy fixes that count (initial actual profile: one). Candidate Portal
creation must require the same checkpoint step_count for the selected guest key;
a root alone does not expose its private path count. Different request/fulfillment
contexts can share one checkpoint context root under a separately reviewed
multi-step profile. `build_candidate_checkpoint` constructs complete effects
and both roots; it is not execution verification. Full state sets reject duplicate addresses, zero-lamport
present entries and omitted changes. Unknown/unsupported interpreter context
returns `Err(Unsupported(...))`, not a successful proof of program failure.
Candidate v1 failure code 1 means `InstructionError::Custom`, with its full
u32 custom payload. It is not the legacy serialized enum discriminant. All other
instruction/transaction error variants are Unsupported in this profile; future
support needs an explicit schema revision, never silent truncation.

Tests in `tests/vrf_candidate.rs` exercise bindings with an explicitly synthetic,
non-executing test engine. They cannot establish curve/CPI validity. The real
engine integration must supply those mutation/differential tests. Canonical
message signature/shape admission has separately passed on actual retained ER
request and fulfillment bytes. No complete candidate envelope is manufactured
from non-atomic first-wave account observations.

## Trace completeness: historical proposal, not trace-v2 support

Full interpreter execution is the selected approach. The following trace gaps
explain why a trusted-trace shortcut is not an alternative; diagnostic trace-v2
is not required to replace actual interpreter execution.

[Trace v1](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/zk-types/src/trace.rs#L124-L180)
contains VM rows, syscall argument addresses/results, invocation IDs, memory
summaries and transaction outcome. It does not encode authenticated syscall
input/output bytes at each call, CPI account privilege/seed transitions, or
return-data producer transitions. Invocation and memory summary hashes cannot
supply the missing semantics.

The host collector additionally synthesizes CPI boundary events from inner
instructions, with zero CU and transaction-wide success/failure; those events
are diagnostics, not independently verified per-child outcomes. See
[collector](https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/replay-harness/src/trace.rs#L337-L383).
Its transaction summary copies return bytes without the producer ID. A proposed
trace-v2 would need call-time memory reads/writes, authenticated code/memory,
exact invocation linkage, privileges/seeds, per-child outcomes, sysvar reads,
return producer and rollback journal. Adding fields alone would still not prove
their truth; the selected relation must recompute every transition.

## Rejection matrix

“Current” below names the earliest relevant existing guard or an explicit
unsupported boundary. The new host diagnostic is never a replacement guard.

| Invariant/case | Current behavior | Candidate requirement; evidence level |
| --- | --- | --- |
| Actual request/fulfillment account shape | Bank snapshot returns `None`; frozen wire parser returns `Wire` | Retained signed captures must show exact observed rejection; synthetic shape tests establish only structural rejection. |
| Two payer signatures / extra compute instruction | Frozen wire `Wire` | Initial candidate rejects extra signer; only fixed approved compute prefix. |
| Invalid signature / changed signed bytes | Existing route rejects supported shape; host diagnostic checks all signatures | Reject before execution; tests mutate each synthetic signature and message. |
| Versioned, malformed, trailing or noncanonical wire | Frozen wire rejects; diagnostic rejects or reports unsupported version | Reject; no implicit conversion to legacy. |
| Curve/CPI syscall row in old witness | `Trace`, even after recomputing trace hash | Recompute semantics in a separately approved candidate; never allow by name/hash alone. |
| Initialization, close or rollback in old witness | `Accounts` | New candidate must verify creation/nonexistence, close and atomic rollback. |
| Wrong program ID / ELF / loader | Wire/program/checkpoint restrictions are not a complete new-profile identity policy | Match approved identities and authenticated loader state; same address is insufficient. |
| Wrong verifier return producer/length | No current CPI-return relation | Candidate rejects at consumption; add capture/invariant tests, not enabled here. |
| Wrong alpha/key, noncanonical curve/scalar, invalid proof | No current VRF relation | Candidate verifies arithmetic and preserves rollback effects. |
| Wrong callback PDA, account order, signer/writable escalation | No current CPI privilege relation | Candidate derives PDA, verifies privileges and rejects changed binding. |
| Same-slot, expired or unauthenticated Clock/SlotHashes | Snapshot carries sysvars; current guest has no VRF sysvar relation | Authenticate source and recompute deadline/alpha; test boundary slots. |
| Callback failure | Existing success-only relation rejects account/outcome shape | Candidate proves failure and exact fee-only rollback; no closed request. |
| Omitted account change/deletion or checkpoint path mismatch | Existing checkpoint authentication rejects inconsistent old effects | New candidate must use complete recomputed effects and authenticated absence. |
| Native verifier success or matching trace hash only | Does not authorize new execution shape | Never classify as execution proof, even if signed/profile hashes match. |
| Existing retained narrow witness | Existing replay must continue to pass unchanged | Native regression only, not fresh GPU/Portal acceptance. |

## Implementation selection and retained comparison

1. **Selected: bounded SBPF candidate.** Extend verification only for the frozen program
   composition and observed opcodes/syscalls. Implement authenticated memory,
   ELF/loader identity, nested CPI and curve/sysvar/return/rollback semantics.
   Relative effort: high, multi-stage runtime/guest work; principal risk is
   incomplete VM/syscall semantics and proving cost. Requires the implemented
   authenticated input schema, a new interpreter guest/key and explicit profile
   binding. Diagnostic trace-v2 is optional, never execution evidence. The
   eight-field ABI/order is preserved with the explicit kind3 root semantics
   above. Candidate changes are authorized for development; production rollout
   and external human acceptance are not established.
2. **Not selected: program-specific transition candidate.** Independently compute request,
   exact verifier algorithm and one frozen consumer transition in a separate
   relation. Include signed transaction parsing, program/build identity and
   configuration, sysvars, fees, PDA/CPI equivalence, close and rollback. Relative
   effort: medium-to-high but smaller than bounded VM verification; principal
   risk is drift between the native model and actual program/Anchor semantics.
   It proves the specified program transition, not arbitrary SBPF execution.
   Requires its own witness schema, guest/key and explicit acceptance decision;
   it cannot be presented under the existing relation merely because public
   inputs have the same byte length. Trace-v2 might be diagnostic rather than
   authoritative for this option.

These are relative engineering estimates, not schedules or GPU measurements.
Bounded execution reuses the actual pinned Agave interpreter, loader, memory,
syscalls and CPI machinery rather than asserting trace fields. Human acceptance
remains external; it does not block the authorized development implementation.
Freeze the reviewed fixture and acceptance details with **Bilal/Jonas**: intended execution-validity claim, exact program
composition/builds, callback specimen and negative corpus, runtime/sysvar source,
account/alias limits, fees/rent, errors/rollback, profile commitment, checkpoint
creation/deletion/settlement representation, public ABI/domain and guest/key
review plan. Production verifier identity also needs its owner's confirmation.
No human approval or independent reproduction is claimed here.

## Fixture handoff and diagnostic use

Capture IDs must distinguish request success, fulfillment success, invalid-proof
failure and callback failure. Retain raw signed transaction wire; exact pre,
post, readonly and rollback accounts; program/ProgramData and ELF bytes/hashes;
runtime revision/features/VM/syscall/fee/rent policy; Clock/SlotHashes; full
nested trace and call-time return-data producer; errors, CU, account effects,
checkpoint paths/anchors and observation logs. Include source/build flags and
SHA-256 inventory. Label native, Bank diagnostic, local ER, hosted ER, GPU and
real Portal resolution separately. Never manufacture runtime captures from this
document's synthetic test shapes.

Host-only tool, after building package `northstar-transaction-proof`:

```sh
cargo run -p northstar-transaction-proof --bin vrf-coordinator-shape -- signed-transaction.bin
cargo test -p northstar-transaction-proof --test vrf_coordinator_profile
```

Run commands inside the sandbox with the shared heavy-build lock and two build
jobs, as required by the worker contract. The tool verifies message signatures,
reports exact transaction/message SHA-256, message keys/privileges, blockhash,
instruction account indices and pinned program/discriminator labels, and calls
the unchanged frozen wire parser. It does not deserialize
application arguments, observe nested CPI, execute the program, authenticate
blockhash freshness or validate checkpoints. An exit status of zero means the
**diagnostic completed**, not that the transaction is admitted. Successful output
always states `execution_verified=false`, `vrf_ready=false`, `proof_generated=false`.
A frozen-wire success still does not assert native replay success.

`northstar/transaction-proof/fixtures/vrf-coordinator-profile-v1.json` is source
metadata plus the handoff checklist, not captured execution data. Tests use
explicitly synthetic signed shape specimens and a retained old-relation witness.
An ignored test, `invariant_captured_request_and_fulfillment_remain_rejected`,
consumes real raw captures through `NORTHSTAR_VRF_REQUEST_TRANSACTION` and
`NORTHSTAR_VRF_FULFILL_TRANSACTION`. Run it explicitly with `--ignored --nocapture`
once both exist. This test was run successfully against the actual first-wave
request/fulfillment pair on 2026-10-23: 1 passed, exit 0. A second candidate-wire
test also verified signatures and the new bounded outer shape, exit 0; neither
is actual interpreter execution. The legacy test verifies signed labels, rejection and
rejection by the authoritative replay function using the captured transaction
bytes. Its retained old witness is only a carrier to reach the unchanged wire
guard, not a VRF execution witness or proof.

Acceptance remains separate: 100 consecutive request-to-callback cycles on an
actual Northstar ER with no lost/duplicate callbacks; three consecutive fresh
real development Portal resolutions covering request, verification and callback;
each final wrapped proof at most 120 s on the user-selected **g5.xlarge/A10G**;
each complete resolution at most 130,000 CU. The hardware change was explicitly
approved on 2026-10-23; A10G is never labeled L40S. First-wave runtime evidence
already records 100 local ER callbacks, but does not establish a candidate
execution proof or Portal resolution. Hosted/delegated settlement remains
unsupported; independent human reproduction and acceptance stay pending.
