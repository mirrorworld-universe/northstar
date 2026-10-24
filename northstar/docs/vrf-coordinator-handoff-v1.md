# Pinned Coordinator compatibility and runtime handoff v1

**2026-10-23 update:** the user selected ER-native development and one
**g5.xlarge/A10G** measurement target after PR #207. See the separately versioned
[candidate development contract](vrf-coordinator-development-v2.md). Historical
L40S/delegation requirements below are not the selected target for that candidate.
The pinned offline bytes and existing generic evidence schema remain unchanged.

**DRAFT — offline characterization, not an approved ABI freeze or acceptance.**
No hosted/delegated ER run, GPU execution proof, Portal resolution, production
verifier deployment, stable-fixture approval, Baixiong acceptance or Jonas review
is supplied by these files. The existing [acceptance contract](vrf-acceptance-v1.md)
and its closed schema remain unchanged. A verified randomness proof does not prove
SVM execution. The current Northstar replay relation does not support this flow.

## Source and byte identities

All library/SDK references below pin
[`d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd`](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/tree/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd).
Package version `0.1.0` alone is not a source pin. The manifest binds the vendored
IDL, source interfaces, SDK builders, reference service prover, lockfiles and MIT
license to individual SHA-256/size records and GitHub permalinks. It records
**source identity, not deployed binary identity**; binary digests remain null.

`northstar/fixtures/vrf-coordinator-v1/compatibility-manifest.json` has identity:

```text
5891551ac67b003986a300e6b5e92089c886c3584c12c83411cf99374227b95d
```

The validator pins SHA-256 of its normalized UTF-8 representation:
`json.dumps(document, indent=2, ensure_ascii=True) + "\n"`. The shipped file already
uses that representation, so its file SHA-256 is identical. The pin includes all
ordered instruction metas, discriminators, coordinator IDL types/layouts,
test-consumer types, PDA seeds, source provenance and fixture hashes. Reformatting
input JSON does not change its semantic pin; changing contents requires a reviewed
new fixture version and validator pin. This is a local consistency contract, not
cryptographic authentication or anyone's approval.

Pinned fixture artifacts:

- `alpha-golden.json`: offline reconstruction of the pinned Rust/SDK golden test.
  Request bytes are `07` repeated 32 times, seed `01` repeated 32 times, request
  slot 100, newest hash slot 99, slot hash `02` repeated 32 times. The 156-byte
  preimage hashes to `85d739f7f781f84ed02f7f3f15be1c87f985a893a464b5355c94501bb15cdefb`.
- `rfc9381-vectors.json`: public key, alpha, proof and output copied from reference
  verifier tests, RFC9381 B.3 examples 16–18. Their alphas have lengths 0, 1 and 2;
  these are **not** request-derived Coordinator inputs or captured fulfillments.
  Only `verify_raw_alpha` accepts these arbitrary-length inputs. Coordinator CPI
  uses `verify`, whose alpha is exactly 32 bytes. No curve verification is run by
  the Python validator.
- `encoding-only.json`: visibly synthetic Borsh shapes for request (Callback and
  Store), fulfill, verify and callback. The RFC example-16 proof deliberately does
  **not** match the alpha golden vector. Never submit it as a valid fulfillment.
  Callback data appends raw `aabb`, not another vector length prefix. These bytes
  demonstrate serialization only, not a supported execution-proof fixture.
- `UPSTREAM-LICENSE.txt`: the upstream MIT notice retained with derived IDL data.

The manifest includes each artifact's authoritative size, SHA-256 and provenance.
The schema and empty `capture-not-run.json` template are interchange definitions,
not captured fixture bytes. The empty template contains no observations.

## Algorithm and call contract

Algorithm: **RFC9381 ECVRF-EDWARDS25519-SHA512-TAI, suite 1**. Public key 32 bytes,
proof 80 bytes (`Gamma[32] || c[16] || s[32]`), output 64 bytes. Existing Northstar
C2SP ristretto255 CPU fixtures use a different algorithm and cannot be converted
by renaming fields or changing lengths.

Program IDs, for the pinned local/default source configuration:

- Coordinator: `FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N`
- Reference Verify: `FUBz1nvSSoHnMqFoK2CA3jF5ckpYPdF3ZT4SoLzDNzh1` (**test only**)
- Test consumer: `HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt` (**test only**)

The source's devnet feature also pins that reference verifier. Mainnet has no
production Verify address and intentionally fails compilation. These are not
Northstar Portal verifier IDs/constants. Do not reuse an older reference-verifier
binary solely because its filename or program ID matches. Build/hash the actual
pinned source with exact toolchain, lockfile and features recorded.

Instruction order below is **instruction account order**, not the compiled
transaction's deduplicated account-key order. `S` means signer, `W` writable;
otherwise an account is read-only/non-signer. The complete manifest also records
all Coordinator administration/expiry/close instructions and their argument types.

```text
request_random  cf0e6f2236380c5a
  payer(SW), fee_payer(SW), requester, requester_identity(S),
  program_config, request(W), slot_hashes, system_program

fulfill         8f0234ceaea4f748
  fulfiller(SW), program_config, request(W), rent_payer(W),
  callback_signer(optional), callback_program(optional), verify_program,
  then stored callback accounts, in order (no SDK-requested signer flags)

verify          85a18d3078c65896
  no account metas; discriminator || suite:u8 || key[32] || alpha[32] ||
  proof_len:u32_le || proof[80]
  CPI AccountInfo includes the verifier executable, but it is not an account meta.

on_random       3fe44c86f31475aa
  callback_signer(S), slot(W), then any additional stored callback accounts
  discriminator[8] || request_address[32] || output[64] || raw callback args
```

The fulfill proof argument is `proof_len:u32_le || proof`, following its 8-byte
discriminator. Request arguments follow the exact vendored Borsh types, including
one-byte enum/Option tags; see the manifest and encoding-only examples.

Test-consumer setup uses `set_behaviour` (`2dbbc1c2fb90295d`):
`payer(SW), slot(W), system_program`. Its `make_request` (`2934b1498c249890`)
uses `payer(SW), slot(W), identity, requester, coordinator_config, request(W),
slot_hashes, coordinator_program, system_program`, then any extra callback
accounts in their supplied order. The manifest records the exact argument types.

The outer fulfill `callback_signer` meta is **not** a signer. The Coordinator
signs its callback CPI with `['vrf_callback', consumer_program_id, bump]` under the
Coordinator. The consumer must bind that signer to itself and bind the delivered
request address to its recorded request. The requester identity instead uses
`['vrf_requester', bump]` under the **consumer**. An off-chain wallet cannot sign
for either PDA. Callback-mode requests therefore originate inside consumer CPI.

The outer transaction may promote a stored read-only callback account to writable
when another instruction/meta needs it writable. The Coordinator accepts that
promotion, but constructs callback CPI metas using **stored** writability. Do not
mistake this for account-order tolerance or permission to widen the callback CPI.
Verifier return data must name the pinned Verify program and contain exactly
64 bytes. Callback failure rolls back fulfillment; no successful delivery is
recorded and the pending request remains available until expiry.

Alpha is SHA-256 of the following ordered bytes, without length prefixes:

```text
"sonic-vrf/v1" || coordinator_id[32] || request_address[32] || seed[32] ||
request_slot:u64_le || hash_slot:u64_le || newest_slot_hash[32]
```

Request PDA seeds are `['request', requester, request_key]` under Coordinator.
There is one open request per requester/key, not an arena queue. Config is
`['config']`. Request version is 1, status sits at offset 9, and account size is
`401 + 33 * callback_account_count + callback_args_length`. Fulfillment requires
`request_slot < fulfill_slot <= deadline_slot`. Callback success pays the fee to
the fulfiller, closes the request and refunds rent to the recorded rent payer.
Store success retains output until requester close. Expiry does not call the
consumer. The library is L1-only today and has **no Portal delegation ABI**.

## Offline commands

From this repository root, Python 3.9+ standard library only:

```sh
python3 northstar/scripts/vrf-coordinator-fixtures.py --structure-only
python3 northstar/scripts/vrf-coordinator-fixtures.py \
  --capture northstar/fixtures/vrf-coordinator-v1/capture-not-run.json
python3 -m unittest discover -s northstar/scripts -p 'test_vrf_coordinator_fixtures.py' -v
```

Exit **0** means only successful explicit `--structure-only` validation. Default
exit **1** means external verification/acceptance is not established, even if all
reported arithmetic checks pass. Exit **2** means malformed/inconsistent input or
missing/mismatched bytes. No flag certifies acceptance. The empty template is
structurally valid and blocked. The validator never fetches or executes metadata.

Optional `--source-root <PINNED_LIBRARY_ROOT>` checks actual local source-file
hashes against the source manifest; it does not execute that source or authenticate
its repository history. To reproduce generated fixture bytes in a new scratch
folder, using a read-only source checkout at the exact pin:

```sh
python3 northstar/scripts/vrf-coordinator-fixtures-generate.py \
  --source-root <PINNED_LIBRARY_ROOT> --output <NEW_SCRATCH_FIXTURE_DIRECTORY>
python3 northstar/scripts/vrf-coordinator-fixtures.py \
  --manifest <NEW_SCRATCH_FIXTURE_DIRECTORY>/compatibility-manifest.json --structure-only
```

The generator copies only data and derives bytes; it runs no library scripts.
It is not an approval tool. Always validate generated output against the embedded
pin before use. Never overwrite a captured evidence folder with generated data.

## Runtime interchange

`northstar/fixtures/vrf-coordinator-v1/capture-schema.json` defines a separate,
closed draft shape. Keep the existing acceptance packet unchanged. Captures bind
the compatibility pin and declare `synthetic`, `native_diagnostic`, `local_er` or
`hosted_er`. Local ER with synthetic genesis/program injection is still diagnostic,
not delegated or hosted acceptance. Record this setup in configuration/logs.

Collection rules:

1. Start one collector-wide monotonic clock **before** the first submission. Use
   integer nanoseconds and a unique `clock_id`. Per-transaction elapsed times,
   slots, wall clocks or independent host `Instant` values are not interchangeable.
   Timestamp actual submission and callback observation, not a later export.
2. Retain every submission, fulfillment attempt (including errors/retries) and
   callback observation in a bounded declared window. Request, callback and attempt
   arrays each have unique IDs and consecutive sequence numbers from 1, with ordered
   timestamps. Callbacks may complete out of request order; match by request ID.
3. Record request addresses, transaction identities, callback output and consumer
   delivery count. Exactly one successful callback per submitted request is required.
   Callback observations must match retained callback attempts and the same successful
   fulfillment transaction. Zero/multiple deliveries, missing callbacks or any failed
   attempt block the consecutive-success window. Preserve preceding failed windows
   and negative-test logs; do not silently filter them out of a claimed window.
4. Bind every row to local receipt/log bytes. Artifacts have unique IDs, relative
   paths, size, SHA-256 and `captured`/`synthetic`/`retained` provenance. The window
   names a complete attempt log. The environment names a configuration artifact.
5. Record actual runtime commit **and binary hash**, ER and L1 genesis identities,
   active feature-set digest, and all three program IDs/binary hashes/build features.
   Configuration must include dirty-source diff identity when applicable, compiler/
   build commands, sanitized RPC/configuration, loader/program-data ownership,
   initialization/crediting method, deployment mode and observation commitment.
   Unknown identities stay null and block readiness. Never include secret keys or
   RPC credentials. A source commit or filename cannot substitute for a binary hash.
6. Use `--capture <capture.json> --artifact-root <CAPTURE_DIRECTORY>` to check a
   capture. Limits: JSON 4 MiB, 128 artifacts, 64 MiB each/256 MiB total, 2000
   submissions, 4000 callbacks, 10000 attempts. Escapes and file aliases reject.
   Hashes prove byte consistency only, not event authenticity or log completeness.

A test suite can fabricate internally consistent `captured` observations. Such a
packet still exits 1 by default and never establishes acceptance. Collector review,
actual RPC/receipt/state inspection and independent reproduction remain mandatory.

The runtime worker's raw account/transaction journal is valuable even when it lacks
this draft shape. Preserve it unchanged. An adapter may map recorded fields, but
must never infer absent submission timestamps, genesis IDs, binary hashes or a
successful callback from a summary counter alone. Missing data is a blocker, not
permission to synthesize evidence.

## SDK-only reproduction and remaining decisions

Use the source-pinned SDK workspace, its vendored IDL and lockfile, not an unpinned
registry package. A Rust consumer CPI dependency can pin:

```toml
vrf-coordinator = { git = "https://github.com/mirrorworld-universe/vrf-coordinator-program-library", rev = "d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd", features = ["cpi"] }
```

Consumer programs must use the Coordinator's Anchor line, or a separately reviewed
IDL-generated client. The pinned JS SDK uses Anchor `0.32.1`, web3 `1.98.4` and
noble hashes `1.8.0`; do not substitute JS and Rust Anchor version numbers. Build
from the pinned workspace/lockfile using already approved tooling. No tooling
installation or library modification is required for these offline checks.

SDK sequence for an approved local runtime test:

- Use `VrfCoordinatorLib.readOnly(connection)` to derive config, request, consumer
  requester identity and callback signer PDAs. Read actual configuration and the
  registered VRF key/suite/fulfillers before submitting a request.
- Call the pinned test consumer's `set_behaviour(Ok)` then `make_request`; do not
  submit a callback-mode `createRequestRandomTransaction` directly from a wallet.
  The caller cannot sign its consumer PDA. Store mode is a separate direct-wallet
  flow and does not satisfy callback handoff.
- The test-grade reference oracle/service uses the **actual RFC9381 prover** and
  verifies locally. Reuse that implementation; do not adapt Northstar's ristretto
  CPU proof generator. Never use production key material in reference services.
- Wait for an observed slot strictly later than the request slot; budget fulfillment
  via `fulfillComputeUnits(request, hashToCurveTries)`. Build fulfillment from the
  fetched request snapshot with `createFulfillTransaction`. The SDK's `prepare`
  supplies fresh blockhash/fee payer/compute budget; builders alone are unsigned.
- Observe transaction logs via `parseEvents` (including nested consumer CPI), inspect
  consumer output/delivery count and request close/refund, and retain observations.
  Poll readiness/slots rather than sleeping a fixed interval. Follow the capture
  rules above for 100 consecutive cycles; keep initialization and negative tests
  outside the declared successful window, with their own retained logs.

The user selected ER-native request/verification/callback development on
2026-10-23. Cross-layer delegation and settlement remain unsupported; that selection
does not change consumer ownership or authorize a delegated queue. Hosted ER identities/access and consumer/backend integration
remain dependencies. Baixiong's SDK/docs-only integration acceptance, Jonas's
independent reproduction/review, agreed stable fixtures and substantive team
responses remain pending. Only humans can supply those records.

The historical generic evidence contract retains its **one L40S** gate. The
2026-10-23 user decision explicitly substitutes **one g5.xlarge/A10G** for this
separately versioned development candidate; never label A10G as L40S. Standalone
Verify or randomness proofs cannot satisfy three fresh real
`ResolveChallenge(Production)` runs. A separately versioned bounded SBPF replay
candidate is now authorized, with legacy route semantics preserved and no
production rollout. The source compatibility manifest itself enables no route.

## Primary source permalinks

- [Vendored IDL](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/sdks/packages/vrf-coordinator/src/idl/vrf_coordinator.json)
- [Alpha and Rust golden test](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/alpha.rs)
- [Request and CallbackSpec](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/instructions/request/request_random.rs)
- [Fulfillment, signed callback and close](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/instructions/fulfillment/fulfill.rs)
- [Verifier CPI wire interface](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/verify.rs)
- [Return-data and callback privilege checks](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/vrf-coordinator/src/utils.rs)
- [Reference verifier/RFC vectors](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/reference-verify/src/ecvrf.rs)
- [Test consumer and callback contract](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/programs/test-consumer/src/lib.rs)
- [SDK transaction factory](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/sdks/packages/vrf-coordinator/src/vrf-coordinator-lib.ts)
- [L1-only architecture](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/docs/01-architecture.md)
- [Reference service limitations](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/blob/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd/docs/06-services.md)
