# Settlement v1 commitments and challenge rules

Status: frozen for the v1 checkpoint/challenge skeleton. Cryptographic verifier details may add proof encodings, but must not change these commitments or outcome rules.

## Checkpoint commitment

A checkpoint binds one ordered ER execution trace:

- `session` and `er_slot` identify the ER and checkpoint position.
- `step_count` is the number of disputed steps. Steps use zero-based indexes and intervals are half-open: `[start_step, end_step)`.
- `previous_state_root` and `new_state_root` commit the trace boundaries.
- `trace_root` commits every intermediate state root in order. A bisection response must provide the canonical midpoint root and its authentication path in the DA payload.
- `tx_effect_root` commits the ordered per-step transaction/effect leaves.
- `readonly_l1_root` commits all L1 values read by this checkpoint, including account identity, owner, lamports, data hash, and observed L1 slot.
- `da_commitment` commits the sealed DA manifest and payload pages.
- `effect_commitment` commits the exact L1 settlement effects consumed by Portal.

All hashes are 32 bytes. Hash trees use domain-separated SHA-256, ordered children, and explicit leaf indexes. Empty trees use a domain-specific empty root; zero is not an implicit empty-tree value.

## One disputed step

One disputed step is transition `i -> i + 1` for one serialized ER transaction/effect leaf. Its pre-state root is trace root `i`; its post-state root is trace root `i + 1`. It includes deterministic transaction sanitization, the declared writable account transition, readonly L1 inputs, execution result/effects, and the settlement effects attributed to that transaction.

The v1 proof does not prove a complete SVM implementation. It proves the ER-shaped transition circuit selected by `proof_kind` and `proof_version`, plus membership of its inputs and outputs in checkpoint commitments. Unsupported SVM behavior cannot be represented by silently weakening the circuit: the checkpoint producer must reject it or use a later proof version. Full SVM proving is out of v1 because it requires stable semantics for every loader, syscall, CPI, feature gate, and compute rule and is far beyond the first circuit's constraint and delivery budget.

## Public input ABI

`northstar-er-step-v1` uses this canonical ordered ABI. Portal hashes the same ordered byte values into `StepProofAccount.public_input_hash`; the Groth16 adapter may pack them into eight field elements without changing their meaning.

1. Protocol domain: `northstar-er-step-v1`, `proof_kind`, and `proof_version`.
2. Session context: Portal program, session pubkey, and checkpoint identity.
3. Position: `er_slot` and `step_index`.
4. `pre_state_root`.
5. `post_state_root`.
6. Per-step transaction/effect root, with membership against checkpoint `tx_effect_root` proved by the witness.
7. `readonly_l1_root`.
8. `effect_commitment`, including the disputed step's settlement-effect membership witness.

Integers are unsigned little-endian. No variable-length value enters public inputs directly; it is length-delimited in the witness and represented publicly by a domain-separated hash.

## Required witness

A valid one-step witness contains:

- serialized sanitized transaction/effect bytes and membership path to `tx_effect_root`;
- pre-state leaves for every writable account, including pubkey, owner, lamports, executable/rent metadata, data length and bytes, plus paths to `pre_state_root`;
- readonly L1 account values and membership paths to `readonly_l1_root`, including the observed L1 slot;
- circuit-specific execution witness: instruction data, ordered account indexes, program/loader identity, CPI/effect transcript, return data, and failure code;
- post-state leaves and paths deriving `post_state_root`;
- settlement-effect leaf and path to `effect_commitment`;
- trace membership paths for both boundary roots;
- DA manifest/page references, codec and dictionary hashes, uncompressed/compressed lengths, and integrity paths to `da_commitment`.

Witness encoding must be canonical and reject duplicate leaves, ambiguous ordering, trailing bytes, and mismatched lengths.

## DA reveal and challenge clock

Checkpoint challenge window is capped at 9,000 L1 slots, approximately one hour at the 400 ms target slot time. `challenge_deadline_l1_slot` is the hard deadline for the whole proposal, not a fresh window granted when a challenge opens. Every turn receives at most 750 slots and is clipped to the hard deadline.

Opening a challenge creates:

- `Challenge`, seeded by checkpoint, holding the interval, boundary roots, alternating turn, and deadlines;
- `DataAvailabilityProof`, seeded by challenge, initially `Missing`, holding the checkpoint DA commitment and revealed manifest/inclusion-proof hashes.

The sole validator is the respondent. Its first response must reveal a payload root equal to the checkpoint `da_commitment` and bind the inclusion-proof bytes by hash. This account is evidence and lifecycle scaffolding; the production DA inclusion verifier remains a separate adapter. Immutable DA pages must remain fetchable through every challenge/response deadline plus operational margin.

## Bisection

1. Validator responds with the canonical midpoint `start + (end - start) / 2`, its state root, and DA evidence.
2. Challenger selects lower `[start, midpoint)` or upper `[midpoint, end)` half.
3. Turns repeat until interval width is one.
4. Validator acknowledges/reveals the isolated transition; challenger submits the sealed one-step proof.
5. Portal verifier result resolves the challenge.

A challenged checkpoint cannot commit, settle, or close its session. Elapsed time never makes it implicitly valid; an explicit timeout or proof resolution is required.

## Defaults, rewards, and recovery

Settlement v1 uses the existing single-validator proposer escrow and no challenger bond.

- Validator misses any response deadline: checkpoint loses by default. Missing DA becomes `Defaulted`, checkpoint becomes `Invalid`, and the full proposer bond is transferred to the challenger.
- Challenger misses a bisect or proof deadline: challenger loses. Checkpoint returns to `Pending`; its proposer bond remains locked until normal checkpoint finalization.
- Verified fraud proof: checkpoint becomes `Invalid`; full proposer bond goes to challenger.
- Proof that establishes the checkpoint transition: checkpoint returns to `Pending`; bond remains locked.
- Unavailable production verifier: no state or lamports change.

After invalidation Portal clears only the active checkpoint pointer. The latest finalized ER slot/root never moves backward or adopts challenged state. Validator may propose a replacement for the same ER slot from that unchanged finalized root, with a new locked bond and reset challenge state. Settlement effects from an invalid checkpoint remain unapplied.

Free challenges can delay one checkpoint for up to the hard deadline. Challenger escrow and anti-spam policy are explicitly deferred; adding them must not alter the proof ABI, DA default rule, or finalized-root recovery invariant.
