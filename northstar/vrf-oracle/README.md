# CPU VRF oracle foundation (experimental, offline only)

This private Rust package generates ordinary CPU VRF proofs, verifies them with public inputs, and exercises a durable request lifecycle. It does **not** implement a Solana adapter, application account decoder, instruction builder, transaction signer, callback, HyperFuse API, or network client. `LiveSolana` metadata is always rejected. The validator, Portal, and execution-proof routes are unchanged.

## Cryptographic scope and provenance

- Implementation: [`str4d/vrf-r255`](https://github.com/str4d/vrf-r255), registry version **`=0.1.0`**. Cargo.lock pins the package checksum `ec2e7c083cc7b8fc3f6a4bc68f92af0e4ec0ec83b2d8a912eeced0aafa7a8379` and dependencies (Dalek 4.1.3, SHA-2 0.10.9). No new VRF algorithm is implemented here.
- Suite: **C2SP ECVRF-RISTRETTO255-SHA512**, suite string `0xff || "c2sp.org/vrf-r255"`. The proof is 80 bytes, the public key 32 bytes, and the output 64 bytes. These are not the old draft's custom DLEQ layout.
- Specification and public vector: [C2SP revision `fc7f312aff098ed080f3dd5a3da4fd820be17a2e`](https://github.com/C2SP/C2SP/blob/fc7f312aff098ed080f3dd5a3da4fd820be17a2e/vrf-r255.md#test-vector), retained in `tests/fixtures/c2sp.json`. The vector's scalar is **public test data**, never a deployable key.
- C2SP's ristretto suite follows the RFC 9381 construction but is **not one of RFC 9381's listed ciphersuites**. It is not interchangeable with an Edwards25519 suite or a custom Solana verifier.
- The released implementation's `src/lib.rs` matches upstream revision `20ff4a6bd26c58057eab9e69b020b9c4876f562b` (2024-03-26). Version 0.1.0 is the latest published non-placeholder release observed during this work. GitHub reports the upstream repository as non-archived/non-disabled, last pushed 2025-11-20. This is an existing upstream dependency, not an audit or a promise of active maintenance/SLA; its low release activity needs review before production adoption.

Tests compare CPU generation against the exact published proof and output. A separate test and `verify-vector` command verify the published proof without creating or importing a signer. Both use the pinned upstream implementation: this is public-only independent verification of the fixture, **not a second implementation or an independent cryptographic audit**.

## Run locally

From the repository root:

```sh
CARGO_BUILD_JOBS=2 cargo test -p northstar-vrf-oracle --locked
CARGO_BUILD_JOBS=2 cargo clippy -p northstar-vrf-oracle --all-targets --locked -- -D warnings
cargo +nightly fmt -p northstar-vrf-oracle -- --check

cargo run -p northstar-vrf-oracle --locked -- vector
cargo run -p northstar-vrf-oracle --locked -- verify-vector

# First provision a private directory on a trusted durable local filesystem.
# Set this to that existing directory; this example does not create it.
state_directory=/path/to/pre-provisioned/offline-vrf-demo
test -d "$state_directory" || { printf '%s\n' 'Provision state_directory first.' >&2; exit 1; }
cargo run -p northstar-vrf-oracle --locked -- demo "$state_directory" 10
cargo run -p northstar-vrf-oracle --locked -- demo "$state_directory" 11
cargo run -p northstar-vrf-oracle --locked -- demo "$state_directory" 15
cargo run -p northstar-vrf-oracle --locked -- demo "$state_directory" 16 confirm
```

Directory provisioning, permissions (for example mode 0700), and durable namespace setup are operator prerequisites, not established by this example or the library's convenience directory creation. Temporary directories in tests demonstrate local process behavior only.

Each demo command is a separate process. Tick 10 creates a **Pending intent**, tick 11 does not create another, tick 15 retries the same proof, and tick 16 explicitly simulates confirmation. `reject` simulates a definite rejection and starts bounded backoff. Nothing is sent anywhere. The demo's addresses/digests are visibly synthetic repeated-byte fixture identifiers; they are not a deployment manifest or real program metadata.

Ticks are caller-supplied **monotonic logical fixture ticks**, not Solana slots or seconds. The demo request is eligible at 10 and expires at 1000. There is no wall-clock daemon. No 2-second SLO, throughput, ER RPC, Solana CU, GPU, SP1/Groth16, or production-acceptance claim follows from these tests.

## Frozen binding (local format, not an application ABI)

`Metadata` is mandatory: version 1, `OfflineFixtureV1`, exact suite, and nonzero 32-byte chain, program, application-fixture and consumer-fixture identifiers. Unknown fields, absent fields, incompatible suites, absent identifiers, and live mode fail closed. A digest alone does not establish provenance: the offline operator supplies trusted fixture identifiers. A future adapter must authenticate real account data against approved artifacts.

Metadata digest:

```text
SHA256(domain || version_u32_be || exact_suite_string || chain[32] ||
       program[32] || application_fixture_digest[32] || consumer_fixture_digest[32])
```

Complete VRF input `alpha`:

```text
domain || metadata_digest[32] || request_id[32] || public_key[32] ||
context_digest[32] || not_before_u64_be || expires_at_u64_be ||
input_length_u32_be || complete_public_input[input_length]
```

Here `domain` is `b"northstar/vrf-oracle/offline-v1\0"`; the suite string is the `SUITE` constant, fixed by version. The request binding is `SHA256(alpha)`. All fields, including the fixed public key, enter the proof input. Inputs are 1–4096 bytes; request lifetime is at most 86,400 logical ticks. There is **no oracle-selected salt**, cross-key fallback, key rotation, or mutable callback layout.

`context_digest` is an opaque fixture commitment. It does not magically bind accounts that a future application fails to include. Bilal must provide the canonical application transcript and its account/argument binding. This local envelope must not be substituted for the application's eventual `alpha` without explicit approval and interoperability fixtures.

## Lifecycle and restart semantics

```text
Ready -- durable prepare --> Pending -- explicit fixture receipt --> Confirmed
                               |
                   definite rejection / timeout
                               |
                      Ready / new Pending

Ready or Pending -- expiry --> Expired
attempt cap reached after rejection/timeout --> Exhausted
```

- Request ID is unique within a pinned journal/profile. Exact duplicates return `Duplicate` without resetting state, expiry, or attempts. A changed field under the same ID is rejected.
- `prepare` freezes/caches the proof, increments attempts, and saves Pending **before** returning a public intent. Pending is neither a sent transaction nor successful inclusion/callback consumption. Repeating `prepare` before timeout returns nothing.
- A crash before sending may consume an attempt. A crash after sending may leave an unknown outcome. On restart, `pending` returns the current public reconciliation handle; it is not permission for an immediate resend. Timeout allows another bounded attempt with the **same key/input/proof/output**, not a different random result.
- Attempt IDs are `hex(SHA256(b"northstar/vrf-oracle/attempt-v1\0" || binding || attempt_number_u32_be))`. Retries have different local attempt IDs. Confirmation/rejection must match the entire current intent. Stale, changed, or unrelated receipts are rejected. Duplicate confirmation of the same completed attempt is idempotent.
- Definite rejection uses `retry_delay`; an uncertain outcome must stay Pending until `pending_timeout`. Policy is pinned in the journal: 1–16 attempts, delays/timeouts 1–86,400 ticks. No unbounded retry or fee escalation exists.
- Only explicit fixture confirmation yields Confirmed. No confirmation is accepted at or after expiry. Confirmed requests remain terminal after expiry. A production adapter needs application-specific reconciliation of inclusion-before-expiry, forks, and delayed receipts; these are intentionally not inferred here.
- `prepare` drives expiry/retry transitions for one request. There is no automatic sweep, subscription, queue scanner, or service scheduler. Exhausted/expired requests remain retained. Journal capacity is 1024 requests and the serialized snapshot is bounded to 16 MiB; capacity rejects new requests rather than forgetting duplicate history.

The journal holds **public data only**: metadata, policy, monotonic clock, request fields, proof/output, attempts, and state. Every open validates the pinned profile/policy, bounded JSON shape, frozen request binding, cached proof/output, and state invariants. It never loads a key from journal data.

Persistence uses an exclusive OS file lock, a same-directory temporary file, file fsync, atomic replacement of `public-state.json`, and directory fsync. Mutations are exposed in memory only after persistence succeeds. An I/O failure poisons the instance; reopen to reconcile before doing more work. Corrupt/incompatible/missing established snapshots fail closed rather than resetting retry counts. An interrupted first initialization can leave only `writer.lock`; this also fails closed and requires explicit operator inspection. Never delete `writer.lock` while any process may be alive.

These guarantees assume a trusted, durable, pre-provisioned local directory/filesystem. They do not defend against manual rollback/deletion of the whole directory, filesystem/hardware dishonesty, network-filesystem lock behavior, or multiple hosts. Orphan temporary files are ignored, not promoted to authoritative state. There is no distributed lease/fencing or exactly-once network submission guarantee. A real application must make fulfillment idempotent and distinguish soft ER commitment from final L1 settlement.

Unix subprocess tests use a flushed pipe sentinel after `prepare` returns with the second durable Pending intent. While that child retains the lock, a separate contender must fail to open the journal. Another test sends SIGKILL at that boundary, then a new process reuses the existing `writer.lock` inode and recovers the exact intent, attempt count, proof/output and Pending status. Preparation at ticks 15, 16 and 19 creates no new intent before timeout 20, and no confirmation is inferred. Synchronization uses pipes, not fixed sleeps; a bounded readiness timeout only detects a stuck helper. These tests demonstrate **ungraceful process termination and lock release**, not power-loss recovery or filesystem/hardware durability.

## Secrets

`OracleKey` is separate from all serializable/debuggable public types. It imports a canonical nonzero **scalar**, not a Solana keypair or Ed25519 seed, through `Zeroizing<[u8;32]>`. The CLI has **no real-key file, environment, command-line, or wallet loader**; it can only use the published test scalar. No key generation or provider credentials are provisioned.

The owned scalar buffer zeroizes on drop. The pinned dependency creates transient `Copy` expanded keys and does **not** guarantee their zeroization. Caller copies, allocator copies, registers, swap, and crash dumps are not protected by this wrapper. Do not use this fixture runner with valuable keys. Real secret provisioning, access control, memory handling and operational review are separate release blockers. All request inputs/context must already be public; the journal is not a confidential input store.

## Missing artifacts and decisions

No current matching application/SDK source or versioned fulfillment fixtures were found in this repository. The older draft and historical research are references, not the agreed protocol. Live integration stays unimplemented until the following arrive and are reviewed:

**Bilal (application and SDK):**

1. Pinned program and SDK commits/releases, program ID, target chain/genesis/session, deployed ELF hash and account-layout/instruction versions.
2. Real request account/event/snapshot fixtures: owners, discriminators, sizes, offsets, PDA seeds, queue/request IDs, nonce, selected registry key/epoch, complete canonical `alpha` bytes and all authenticated input sources (including any proposed salt).
3. Exact fulfillment bytes and complete ordered accounts with signer/writable flags, authorities/PDA derivation, callback program/discriminator/arguments and account binding. Include matching positive and rejected transaction fixtures.
4. Atomic proof acceptance/consumer consumption semantics; request completion versus callback failure, duplicate fulfillment, fee/refund/expiry behavior, and requested commitment/fork/reconciliation rules. Specify slot/time meanings and the safe retry boundary.
5. A verifier demonstrably compatible with this exact C2SP suite/key/proof/output, or an explicitly approved different suite with reviewed vectors. This package does not choose an on-chain verifier for Bilal.

**Baixiong (consumer/backend and HyperFuse verification):**

1. Pinned backend/consumer revision and fixture contract for discovery, reconnect/backfill, stable request identity and status reconciliation.
2. Expected independently verifiable proof/output transcript and key registration/authentication; exact HyperFuse verification responsibility, if any. No unsigned-salt endpoint or provider-network behavior is assumed.
3. Acceptance fixtures for success, duplicates, unknown submissions, failed callbacks, expired requests and restart recovery, including authoritative completion evidence and consumer acknowledgement.

**Approval required:** using this exact suite in the application, canonical input/context derivation, key/input registration and trust/fallback assumptions, application completion/expiry/commitment rules, and real key provisioning. The offline envelope and retry policy are reversible internal choices, not a production protocol freeze. Threshold randomness, DKG, rotation policy, HSMs and provider-network integration remain out of scope.

Ordinary VRF validity does not prove publication, fairness under withholding, or an SVM state transition. The existing restricted execution-proof relation still does not cover VRF curves or callback CPI. Production acceptance still requires the separately agreed real Portal resolutions, L40S proof timings, Portal CU, and independent acceptance.
