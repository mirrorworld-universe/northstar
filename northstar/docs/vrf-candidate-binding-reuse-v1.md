# Candidate binding: invocation-local validation reuse

The bounded kind-3/version-1 verifier authenticates the signed wire and checkpoint prestate before calling the actual replay engine. Previously its final call to public `bind_candidate_execution` repeated signed-wire admission and prestate/context validation on the same immutable input.

`verify_candidate` now passes its already admitted transaction to a private binding helper after the actual engine succeeds and its complete output equals the supplied expected output. The public standalone binder still admits the wire and validates prestate before calling that helper. There is no public unchecked transaction/context API, persistent cache, asserted-hash cache or supplied execution oracle.

## Unchanged constraints and order

`verify_candidate` still performs, in order:

1. Envelope length and domain/version checks.
2. Fixed policy, signature/wire, feature/runtime/account and pinned program validation.
3. Checkpoint prestate, readonly-context inclusion and no-cross-layer-settlement validation.
4. Actual engine execution and complete expected-output equality.
5. Output account canonicality, fee/CU bounds, readonly preservation and failure/rollback checks.
6. Exact poststate, transaction-effect inclusion, public-root construction and nonzero checks.

Only the redundant repeat of step 3 and signed-wire admission between steps 4 and 5 is removed. The private helper's remaining body is unchanged. Its only two callers establish the same immutable input/checkpoint preconditions first. Standalone `candidate_readonly_context_leaf` and `candidate_readonly_root` retain their own signed-wire admission; checkpoint construction retains its existing public binder call. Independent validation inside the actual engine is not changed.

The optimization does not change curve versions/routines, application SBPF, native engine/runtime, Borsh types/bytes, root algorithms, profile or public order. State leaves remain SHA-based commitments; no bulk Poseidon cache is introduced. Typed policy stays `c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d`, one checkpoint step, effective age 900. All eight public fields remain 256 bytes in the original order. Legacy routes and the v10 guest workspace/lockfile are untouched.

## Focused guards and actual replay

`tests/vrf_candidate_reuse.rs` consumes actual canonical case directories through `VRF_REUSE_CASES_ROOT`. Each directory contains `case-A`, `case-B` and `case-C`, with `binding/witness.borsh` and `binding/public-inputs.bin`.

```sh
VRF_REUSE_CASES_ROOT=/absolute/path/to/actual/cases RUSTUP_AUTO_INSTALL=0 \
  cargo test --locked -p northstar-transaction-proof --no-default-features \
    --test vrf_candidate_reuse -- --ignored
```

The guards check standalone binding/public roots, signature rejection on all standalone helpers, prestate/context rejection before engine dispatch, fresh validation on subsequent calls and unchanged settlement rejection. Their entry-guard engine only returns an error; it never supplies expected execution effects. These are preflight/binding tests, not SBPF execution evidence.

Separately, execute all three canonical envelopes through `northstar-zkvm-vrf-script`, which invokes the actual compiled engine, and compare every one of the 256 resulting public bytes. That path also compares the complete recomputed execution output before binding. Existing authenticated mutation controls must remain rejected; valid application-failure/rollback witnesses are not themselves invalid controls.

Native equivalence is not an SP1 performance result. Parent-owned guest rebuild, actual SDK/corpus execution, proof timing, separately derived key/configuration and fresh lifecycle validation remain necessary. No existing proof/session is relabeled to a new source/ELF. Human acceptance and independent reproduction remain external gates.
