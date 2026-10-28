# Opt-in Succinct Network checkpoint prover

`northstar-network-prover` implements the existing external adapter contract:

```text
northstar-network-prover preflight
northstar-network-prover groth16 WITNESS MEASUREMENTS PROFILE
```

It replaces the CUDA machine for the frozen checkpoint replay relation, not the
validator or Solana verifier. It uses SP1 SDK 6.8.0 and selects its network client
explicitly;
`SP1_PROVER` cannot select a different backend. The default is Succinct's mainnet
auction network. A separate loopback-only hosted mode supports fundless transport
tests against a real CUDA backend. CUDA adapters remain unchanged. Public reserved
capacity and the separate VRF guests are not included.

## Build without CUDA

Run inside the development sandbox. From `northstar/zkvm-replay`, embed the retained
candidate rather than rebuilding a guest with a different program key:

```sh
mkdir -p target/elf-compilation/riscv64im-succinct-zkvm-elf/release
gzip -dc evidence/g5-a10g-sha-v1/candidate.elf.gz \
  > target/elf-compilation/riscv64im-succinct-zkvm-elf/release/northstar-zkvm-replay-program
SP1_SKIP_PROGRAM_BUILD=true cargo build --release --locked \
  -p northstar-zkvm-replay-script --features network --bin northstar-network-prover
```

The build and local replay need a CPU, not CUDA or `cargo prove`. With a custom
`CARGO_TARGET_DIR`, place the ELF under that directory instead. The adapter checks
its embedded ELF digest, program key, and baseline fixture against
[`partial-candidate-v1.json`](../zkvm-replay/partial-candidate-v1.json) before it
can request a proof. No guest, public-input schema, Portal key, or verifier changes
are needed.

## Configuration

Set these variables on the validator or adapter process:

- `NORTHSTAR_NETWORK_PRIVATE_KEY_FILE`: absolute path to a regular, owner-only
  file containing a 32-byte secp256k1 key as hex, optionally prefixed with `0x`.
  Use a dedicated funded Succinct requester, never a Solana signing key.
- `NORTHSTAR_NETWORK_STATE_DIR`: absolute path to a durable, private directory.
  Existing directories must exclude group/other access. Retain it across restarts.
- `NORTHSTAR_NETWORK_MAX_CYCLES`: positive cycle limit per request.
- `NORTHSTAR_NETWORK_MAX_GAS`: positive limit in SP1 prover gas units (PGU).
- `NORTHSTAR_NETWORK_MAX_PRICE_PER_PGU`: in auction mode, positive maximum PROVE
  base units per PGU; PROVE has 18 decimals. Maximum accepted value is 18,446,744,073 because
  SP1 6.8.0 multiplies it by 10^9 internally. This is a unit-price cap, **not a
  total spending budget**. Network base fees also apply.
- `NORTHSTAR_NETWORK_ALLOW_PAID=1`: explicit authorization for proof submission
  and witness upload in auction mode. Not required for `preflight` or local hosted
  tests.
- `NORTHSTAR_NETWORK_MODE`: `auction` (default) or `local-hosted` (test only).
- `NORTHSTAR_NETWORK_RPC_URL`: required only for `local-hosted`; must be an
  `http://` origin with a numeric loopback IP and nonzero port, such as
  `http://127.0.0.1:50051`. Auction mode rejects overrides.

There are no default spending or execution limits in auction mode. Local hosted
mode requires execution limits but does not use a price cap or paid authorization.
Choose limits using measured execution and the current network quote; use an account funded only for the
intended experiment. The adapter checks local SP1 execution against the configured
cycle and gas limits before submission. Limits do not guarantee that an auction
will find a prover or meet Northstar's deadline.

`preflight` checks native and SP1 baseline execution and the frozen key. Auction
mode also requires nonzero requester balance and a read-only Groth16
request-parameter query. Local hosted mode accepts a zero balance. It does not
submit a proof or upload a witness. It is a readiness check, not proof-generation
or latency evidence.

For the [checkpoint coordinator](proof-coordinator.md), set
`NORTHSTAR_PROOF_PROVER` to the absolute path of the built binary and retain the
existing coordinator configuration. Its child environment forwards only the eight
network settings listed above. Raw `NETWORK_PRIVATE_KEY` and validator signing
configuration are not inherited. The live runner can select the same executable
using `NORTHSTAR_LIVE_PROVER`.

## Proof checks and deadlines

The adapter requests Groth16, enables the SDK's private-stdin option, waits for
fulfillment, compares all 256 public-input bytes with native replay, and verifies
the proof locally using the SDK. The coordinator independently verifies the
returned proof before upload. Private stdin restricts artifact access; Succinct's
execution service and assigned prover still receive the witness. This is not a
claim that outsourced proving keeps witness data secret from those parties.

Output matches the existing adapter contract:

- `northstar-sp1-groth16.bin`: SDK proof artifact.
- `northstar-sp1-groth16-onchain.bin`: unchanged 356-byte Portal envelope.
- `northstar-sp1-public-inputs.bin`: canonical 256-byte public inputs.
- The requested measurements file, including backend and network request ID.

Output files are created exclusively; existing artifacts are never overwritten.
The adapter bounds the entire local invocation to 120 seconds. Its
`prove_and_wrap_ms` includes setup, simulation, network submission, queueing,
fulfillment, and SDK verification, unlike the CUDA runner's narrower phase timer.
Retries use the original request deadline and completed timing. No challenge or
protocol deadline is extended. A local timeout does not guarantee cancellation
or a refund for accepted remote work.

## Restart and payment safety

A private journal is keyed by network endpoint, requester, frozen program key,
and witness digest. A file lock rejects concurrent calls for the same request.
The adapter fsyncs a submission intent **before** invoking the SDK request builder,
then records the returned request ID. Application-level retries poll that ID rather
than creating another request. SP1's internal RPC retry behavior still applies;
this is not an exactly-once payment guarantee. Completed proofs are cached and cryptographically reverified before reuse, including after
the original request deadline.

If submission fails or the process stops before the ID is durable, the adapter
fails closed with an unknown-outcome error. Reconcile that requester in Succinct
before changing the journal. Expired or failed requests are not automatically
resubmitted. Do not delete journals to clear errors: doing so permits another
paid request for the same witness. Journal retention and disk monitoring remain
operator responsibilities. This guard applies to one retained state directory;
it cannot deduplicate requests made from another directory or machine.

## Fundless network transport test

[`network-test-gateway.py`](../scripts/network-test-gateway.py) is a test-only,
wire-compatible subset of SP1 6.8.0's reserved API, **not an official Succinct node**.
It serves gRPC requests and HTTP artifacts only on loopback, checks the pinned
program, and runs an existing CUDA prover through an explicit wrapper. It has no
signature authentication, auctions, billing, or production privacy guarantees.
Use a dedicated, unfunded development key. Do not expose either port publicly.

On the CUDA host, prepare the network client as above and start the gateway in a
detached tmux session. Its inline Python metadata declares `grpcio`, `grpcio-tools`,
and `zstandard`; an isolated environment or `uv run` can provide them:

```sh
uv run northstar/scripts/network-test-gateway.py \
  --state /absolute/fresh-gateway-state \
  --prover /absolute/northstar-zkvm-replay-script \
  --wrapper /absolute/with-gpu-server
```

Poll `http://127.0.0.1:50052/healthz` for readiness. Configure the client with
`NORTHSTAR_NETWORK_MODE=local-hosted`,
`NORTHSTAR_NETWORK_RPC_URL=http://127.0.0.1:50051`, the unfunded key file, private
client state, and measured cycle/gas limits. Leave paid authorization unset. Run
`preflight`, then `groth16` in a fresh output directory. The SDK's hosted/reserved
path performs actual program/stdin uploads, signed request construction, status
polling, proof download, and local verification. The gateway generates a fresh
proof using CUDA rather than returning a fixture. Stop the gateway after testing.

Use a [warmed GPU worker](proof-performance-v1.md#opt-in-warm-worker) to avoid
spending the network request deadline on CUDA setup and a cold Groth16 key load.
For that backend, export `NORTHSTAR_GPU_WORKER_SOCKET`, point `--prover` at
`gpu-worker.py`, and use `/usr/bin/python3` as `--wrapper`. The worker supervisor
must already have the correct CUDA environment and frozen replay candidate.
Stop both gateway and worker after a focused test run.

The gateway's requests and program registrations live only for its process
lifetime; use a fresh server state directory for each run. The client's completed
proof cache remains durable. This validates transport and proof compatibility,
not public-network authentication, price discovery, payments, or availability.

The retained [A10G evidence](../zkvm-replay/evidence/succinct-network-local-v1/README.md)
records a fresh fundless network-path proof in 54.981 seconds, followed by verified
cache reuse with both gateway and GPU worker stopped.

## Offline validation and remaining evidence

With the retained ELF prepared as above:

```sh
SP1_SKIP_PROGRAM_BUILD=true cargo test --locked -p northstar-zkvm-replay-script \
  --features network --bin northstar-network-prover --test network_adapter
SP1_SKIP_PROGRAM_BUILD=true cargo clippy --locked -p northstar-zkvm-replay-script \
  --features network --bin northstar-network-prover --test network_adapter -- -D warnings
```

These tests use local execution and retained proofs, not paid network requests.
They cover the frozen key, canonical inputs, proof integrity, permission checks,
limits, authorization, journal recovery, concurrency, and artifact preservation.
The validator's `proof_process` tests cover the credential-path allowlist.
`python3 -B northstar/scripts/test_network_gateway.py` checks the gateway's
version-specific key packing, stdin decoding, and artifact ownership.

Public SP1 6.8.0 service compatibility, auction cost, queueing latency, a fresh
public-network proof, and end-to-end Portal resolution remain unvalidated.
An authorized funded run is required before treating this adapter as operational
proof-generation evidence. Production acceptance remains disabled.
