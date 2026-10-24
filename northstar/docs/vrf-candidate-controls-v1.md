# Bounded VRF candidate control corpus v1

`vrf-candidate-controls` is a host-only transformation tool. It never executes
SBPF, generates proofs or establishes guest rejection. It does not change the
frozen v3 envelope, bindings, profile, guest/key or legacy route.

## Construct

Run inside the sandbox, with two Cargo jobs and the shared heavy-build lock:

```sh
cargo run -p northstar-transaction-proof --bin vrf-candidate-controls -- \
  request-bundle fulfill-zero-bundle authentic-setup.transaction.bin fresh-controls
```

Each source bundle must contain the actual canonical `witness.borsh` and its
256-byte `public-inputs.bin`. Both must use kind 3/version 1, policy
`c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d`,
checkpoint count 1 and effective transaction age 900. The fulfillment must retain
an actual producer with empty return bytes. The setup wire must be authentic,
canonically encoded and signed; its signatures are independently checked and
its shape must be outside candidate admission. No transaction is invented or
resigned. No private wallet bytes are read or emitted.

The destination must not exist. The tool checks every transformation before
creating it and writes `manifest.json` last. It refuses existing files instead
of overwriting a retained corpus. Incomplete output after an I/O failure is not
a frozen corpus.

## Contents

The manifest contains relative paths, SHA-256 identities, two untouched positive
witness/public pairs, fifteen named controls, a source copy of the signed setup
wire, the generator source digest and all four frozen binding-source digests.
It pins the policy/domain, not a guessed ELF or program key. Execution receipts
must identify the exact original and acceleration-candidate ELF/key separately.

Controls cover:

- message signature;
- authenticated input account data and initial request presence;
- authenticated ProgramData ELF byte;
- fixed runtime identity, feature activation and zero-fee profile;
- signed blockhash age outside the fixed policy;
- selected step index and readonly-context count;
- expected complete poststate balance;
- return-data producer, preserving the same actual empty bytes;
- trailing bytes and an oversized vector length prefix;
- an actual signed initialization/setup instruction outside the admitted shape.

For ordinary canonical controls, the constructor changes one named field and
restores that exact field to prove the rest of the source envelope is unchanged.
The balance and return-producer controls then **mechanically rebind derived
checkpoint commitments**. `derived_checkpoint_rebound=true` distinguishes them:
they are internally consistent wrong execution claims that binding-only checks
accept, so only independently recomputed execution can reject them. They are not
single-byte serialized mutations; the semantic mutation remains one named
output field. Source transaction/signature/prestate are unchanged.

The two `canonical_borsh=false` controls intentionally modify raw encoding rather
than pretending to be canonical envelopes. The bounded-length control changes
only the frozen session-vector length prefix; it does not allocate a giant
witness. All other controls decode and re-encode canonically.

A rollback-specific control is explicitly skipped unless a valid, independently
native-verified canonical failed-transaction witness is supplied. A correct
witness of an application rejection or rollback can legitimately produce a
proof. An invalid VRF proof transaction is not, by itself, an incorrect execution
witness. Existing success witnesses are not relabeled as rollback captures.

## What counts as evidence

1. Independently preflight both untouched bundles through the pinned actual
   native verifier. The constructor's claimed-binding checks are not replay.
2. Execute both positive witnesses in the exact selected guest. Require success
   and exact equality with their retained 256-byte outputs.
3. Execute each negative control in that same guest. Count rejection only after
   the actual guest returns a nonzero exit and empty public output. A host read,
   RPC, resource, client or generic executor failure does not count. An expected
   guard string in the manifest is a prediction, not an observed result.
4. Repeat the positive/output and negative matrix for a new acceleration
   candidate before accepting its new key. Passing this corpus is necessary,
   not a replacement for source review or broader execution-validity tests.

The tool always declares `construction_only=true`, `engine_replayed=false`,
`guest_execution_observed=false` and `proof_generated=false`. Native v8 bundle
validation also checks artifact reconstruction: rejecting an edited bundle for
file inequality is not evidence that the guest rejected the modified envelope.
No controls or logs are automatically committed or published.

## Tests

```sh
NORTHSTAR_CONTROL_REQUEST_BUNDLE=request-bundle \
NORTHSTAR_CONTROL_FULFILL_BUNDLE=fulfill-zero-bundle \
NORTHSTAR_CONTROL_SETUP_WIRE=authentic-setup.transaction.bin \
cargo test -p northstar-transaction-proof --bin vrf-candidate-controls -- --include-ignored

cargo test -p northstar-transaction-proof --no-default-features --test vrf_candidate
```

Actual-source tests cover deterministic files/manifest, named-field restoration,
canonical roundtrips, unchanged return bytes, binding-consistent wrong claims,
relative/unique names and output-overwrite refusal. Native/guest observations
remain separate receipts; no human acceptance is inferred.
