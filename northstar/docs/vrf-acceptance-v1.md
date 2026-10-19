# VRF evidence and acceptance handoff v1

Status: **offline development contract, not product acceptance**. No application ABI,
program/SDK revision, VRF execution-proof guest/key or hosted deployment is frozen here.
The [existing narrow transaction relation](transaction-processor-proof-v1.md) does
not prove VRF curve operations or callback CPI.
A CPU VRF proof validates randomness; it is not a Northstar execution proof.

## Run locally

From the repository root, using Python 3.9+ and only the standard library:

```sh
python3 northstar/scripts/vrf-evidence-validate.py \
  northstar/fixtures/vrf-evidence/synthetic-cpu-v1.json --structure-only
# Exit 0: valid structure, NOT acceptance. The report still lists blockers.
python3 northstar/scripts/vrf-evidence-validate.py \
  northstar/fixtures/vrf-evidence/synthetic-cpu-v1.json
# Exit 1: blocked, final_acceptance = not_established.
python3 -m unittest discover -s northstar/scripts -p 'test_vrf_evidence.py' -v
```

The script is also directly executable. `--artifact-root DIR` changes the local
artifact root; the default is the input file's directory. Nothing is fetched or run
from artifact metadata. No credentials, network access, validator or GPU is needed.
Never include oracle secret keys or RPC credentials in evidence artifacts.

Exit codes: **0** only for successful `--structure-only`; **1** for structurally
valid offline evidence requiring live/independent verification; **2** for invalid
structure, identities bound to wrong artifacts, missing files or inconsistent ordering.
Even a packet with every reported gate met exits 1 by default, with status
`ready_for_external_verification`, never an acceptance certificate. Other valid
packets have status `blocked`. Invalid packets have status `invalid`.

## Gates (do not combine them)

- **ER handoff:** at least 100 consecutive request-to-callback cycles on the agreed
  Northstar ER, exactly one successful callback per submitted request, no failed or
  omitted request. Baixiong's recorded SDK/docs-only integration acceptance is separate.
- **Application:** at least 1000 fulfilled requests, no lost/duplicate callbacks,
  nearest-rank p95 request submission to successful callback observation **<=2s**.
  Recompute p95 from all rows; do not supply an asserted percentile. This does not
  replace the 100-cycle ER handoff or the application's agreed rejection tests.
- **Proof/Portal:** exactly three consecutive attempts in a declared capture window;
  all successful, real `ResolveChallenge(Production)`, not a standalone verifier.
  Each execution proof must take **<=120s including wrapping**, on the **same one
  L40S**. Each full successful resolution instruction must use **<=130000 CU**.
  An average cannot hide one slow run. Run fixture IDs must exist in the pinned
  fixtures manifest, and run paths must exactly match that fixture's declaration.
  Across all three attempts, the matched declarations must cover request, VRF
  verification and the consumer callback.
- **Negative/regression corpus:** every case in the pinned corpus must have a result;
  incorrect-witness and unsupported-path cases reject, existing proof-route cases pass.
- **Lifecycle:** startup, cold-start and recovery timings/configuration are separate
  measurements, never subtracted from or substituted for proof time.
- **Independent review:** record Jonas's decision, exact three run IDs, the pinned
  identity, review artifact and independent reproduction capture. A missing, pending
  or rejected review remains a blocker. The tool checks records, not their authenticity.

CPU-only, synthetic, retained-proof, Solana-devnet-only, rejected-resolution and
test-verifier packets cannot satisfy the reported live gates. A devnet L1 used by
an actual Northstar ER is not the same as a Solana-devnet-only application demo.

## Interchange and collection

`northstar/fixtures/vrf-evidence/schema-v1.json` is a closed JSON Schema, version
`northstar-vrf-evidence/v1`. Unknown fields (including `verified: true` or
`accepted: true`), versions, duplicate JSON keys, non-finite numbers and wrong types
are rejected. The stdlib validator evaluates only the schema keywords used in this
file; it is not a general JSON Schema engine. `null` records an unknown identity,
measurement or artifact, rather than inventing one; it remains an acceptance blocker.
Use explicit `missing`, `rejected`, `unsupported`, `pending` and `not_run` states.

The packet contains:

- A bounded capture window with collector, timezone-aware start/end and a complete
  attempt-log artifact. Keep failed attempts; do not export only successful rows.
  Start a new window after a failure and preserve the preceding log for review.
- Source/oracle/application/SDK/consumer revisions as exact 40-character Git commits
  (resolve release tags first), Portal program/binary identity; ER/L1 genesis
  identities, runtime commit, feature-set digest, oracle public key
  and captured environment configuration. The configuration must identify deployed
  application/consumer binaries, RPC endpoints without credentials, features,
  hardware UUID/driver, prover/wrapper versions and reproduction commands.
- Profile/relation version, profile/fixtures/guest/verification-key/corpus SHA-256
  identities and captured files bound to those digests. `paths` declares bounded
  coverage, not generic SVM support. Review actual curve/CPI relation coverage.
- Two separate callback series. Each request and callback has a unique ID, sequence,
  status and integer nanosecond timestamp in that series' `clock_id`. Requests and
  callback observations each have ordered consecutive sequence numbers starting at
  1. Request and callback timestamps must each be nondecreasing. Callbacks can arrive
  in a different request order; match by `request_id`, not array position. Count all
  submissions and observations in the agreed window, including late/failed events.
- Three ordered resolution records with a pinned fixture case ID, distinct checkpoints,
  proof bytes and resolution transactions. The validator checks fixture ID membership,
  exact declared path coverage and fixture byte references/digests. Actual execution
  of those fixture bytes still requires external verification. Each run names history,
  witness, checkpoint, authenticated isolation, prover log, final wrapped proof, receipt and checkpoint/bond/state
  outcome artifacts. All timestamps across these attempts share one monotonic clock
  domain. Use a collector clock for remote stages; do not subtract unrelated host
  clocks. Capture logs must justify those boundaries.
- Lifecycle measurements, per-case corpus results and named review records.

The identity digest on each series, resolution, case and review is SHA-256 of the
UTF-8 canonical JSON object containing **only** `revisions`, `profile`, `environment`:
`json.dumps(identity, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`.
`identity_sha256(document)` in the script computes it. A changed guest, key,
environment or program revision requires new bound records and renewed reviews.
This digest is a local consistency binding, not a signature or proof/public-input binding.

Proof timing boundaries are ordered as follows (integer nanoseconds):

```text
witness_start <= witness_ready <= submitted <= core_done <= wrapped
             <= uploaded <= resolve_submitted <= resolved
proof time = wrapped - submitted   (prepared witness submission to final proof)
witness time = witness_ready - witness_start
upload time = uploaded - wrapped
resolution time = resolved - resolve_submitted
```

`compute_units` means the **entire ResolveChallenge instruction**, including
production verification and state effects, not transaction-wide unrelated work or
inner pairing-only CU. Collect instruction-scoped logs/receipt and reconcile the
reported value externally. `startup_ms`, `cold_start_ms`, `recovery_ms` have no
performance threshold here; define their boundaries/configuration in lifecycle notes/logs.

Artifacts have local relative paths, SHA-256, byte size, kind, claimed source
(`captured`, `synthetic`, `retained`), producer, command, capture time and origin.
Paths must resolve to regular files within the root; path escapes and file aliases
are rejected. Limits: 4 MiB JSON, 128 artifacts, 64 MiB each/256 MiB total, 2000
requests and 4000 callback observations per series, 3 attempts, 128 fixture entries,
128 corpus cases, 4 reviews. Export compact evidence; keep larger raw datasets separately with a
reviewable manifest. No remote downloads or cryptographic verification are performed.

The pinned `fixtures` artifact is a JSON manifest validated against `$defs.fixtures_manifest`:

```json
{
  "schema_version": "northstar-vrf-fixtures/v1",
  "fixtures": [{
    "id": "synthetic-fixture-not-real",
    "paths": ["request", "vrf_verification", "consumer_callback"],
    "artifact": "synthetic-input-not-captured",
    "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
  }]
}
```

This is a **synthetic shape illustration**, not application fixtures or live evidence;
the artifact reference/digest are placeholders and cannot validate a real packet.
The manifest contains 1–128 unique fixture IDs. Each entry declares a nonempty unique
path set within the profile and references a catalog artifact of kind `fixture`.
Its SHA-256 must match that artifact and its local bytes. `profile.fixtures_sha256`
pins the entire manifest, including these per-entry references and digests. No
additional identity field is needed: the existing packet identity binds this digest.
The `fixtures` kind identifies the manifest; `fixture` identifies an individual input.

V1 policy: every run must select a known fixture ID and report **exactly** its declared
path set (order is irrelevant; neither extra nor omitted paths are allowed). Count
window coverage only from matched declarations, never unmatched run claims. The
three-run union must include request, VRF verification and callback; repeated fixture
IDs are allowed. Missing metadata blocks readiness; malformed/duplicate metadata is
invalid. These checks establish internal consistency only. They do not establish that
input bytes have the declared application shape or were executed/proved in a live run.
Bilal/Jonas must approve those semantics and the fixture selection; no ABI is defined here.

The corpus artifact is JSON with `schema_version: northstar-vrf-rejection-corpus/v1`
and `cases: [{"id": "agreed-case", "category": "incorrect_witness"}]` (illustration,
not an agreed corpus). Allowed categories are `incorrect_witness`, `unsupported_path`,
`existing_route`. IDs/categories must match packet results exactly. Require at least
one case in each category. The signed-off manifest must enumerate **all** agreed
cases, including Bilal's eight application invariants and profile-specific witness,
key, authority, invocation and effect checks; this tool does not decide their adequacy.

## Trust boundary and external acceptance

Arbitrary JSON can describe convincing timings and label arbitrary bytes `captured`.
Hash agreement establishes local byte integrity only. It does not establish capture
provenance, cryptographic validity, fresh execution, an honest complete attempt
window, actual hardware, or a reviewer's identity. The test suite intentionally
constructs such a fully populated simulated packet; it still cannot establish acceptance.

Do not use this report as a production verifier or route-enablement input. There is
no `--trust-json`/`--accept` switch. `met_as_reported` means arithmetic and references
are consistent, not that the event occurred. Full acceptance additionally requires
trusted capture/reproduction, verification against the real guest/key and checkpoint
public inputs, chain receipt/state inspection, and authenticated independent review.
Jonas must reject missing coverage or inaccurate claims even when structure is valid.

## Sign-off packet still needed (not supplied here)

- **Bilal:** exact application and matching SDK commits/releases and program ID;
  deployed binary digests; versioned request/fulfillment bytes and account layouts;
  signer/writable/owner/PDA constraints; oracle public-key registration and rotation;
  VRF algorithm/transcript/domain/input/output encoding; callback account/order/CPI
  contract; queue/delegation, slot/sysvar, expiry and RPC/subscription semantics;
  expected state/results; supported/rejected bounded shapes; all eight application
  invariant fixtures (proof validity, registered key, input binding, request replay,
  slot separation, callback account restrictions, expiry, repeated fulfillment).
  Approve the 1000-request/p95 measurement boundary and oracle interface with Ivan.
- **Baixiong:** pinned consumer/backend revisions and HyperFuse re-check interface;
  exact consumer ownership/split; expected callback/state and subscription behavior;
  runnable SDK-only integration steps, capture window and results; recorded acceptance
  within one working day with zero VRF-program changes. Confirm what HyperFuse verifies:
  randomness verification must not be presented as execution-proof verification.
- **Collins:** exact hosted ER/RPC/genesis/runtime/feature/deployment identities and
  access/availability dates; one identified L40S with driver/prover configuration;
  cold/warm/recovery procedure and timing captures; reproducible access and capture
  retention. Resolve the Oct 16 ER gate versus Oct 17 handoff and confirm development
  GPU access before claiming infrastructure validation. No rental/deployment is authorized here.
- **Jonas:** approve the bounded relation, guest/key/version upgrade policy and
  rejection manifest; reproduce all three fresh resolutions with per-run timers,
  full Production CU, checkpoint/bond/state outcomes and regressions; publish an
  authenticated accepted/rejected review tied to the exact identity and run IDs,
  with findings resolved or explicitly blocking. Demo sign-off is separate.
- **Ivan (this effort):** CPU oracle generation/service **and** Northstar runtime,
  history/witness and execution-proof substrate. Confirm key custody, oracle
  restart/idempotency/standby behavior and fulfill/callback responsibilities with
  Bilal. This supersedes older OKR text excluding oracle ownership; no remote OKR
  was changed. Bilal still owns the application/SDK; Baixiong consumer/backend and
  HyperFuse; Collins hosting/GPU; Jonas independent acceptance; Jack frontend.

Application ABI, profile freeze, production proof-route enablement, deployment and
key-management choices remain explicit approvals, not decisions made by this tool.

## Sources and local verification

The October prototype acceptance contract and the agreed CPU-first ownership
split define these gates. Planning requirements are not completed acceptance.
Actual program/SDK fixtures and the final profile still require the explicit
sign-offs described above.

Tests exercise synthetic/offline packets only. They cover inclusive limits, wrapping,
per-run failure hidden by an average, CU/hardware, callback accounting/order/p95,
identities, pinned fixture membership/path coverage/digests and corpus completeness,
artifacts, independent-review records and false passing claims. No ER RPC, live oracle, GPU proof or Portal resolution is
performed. The shipped sample is visibly synthetic and reports blocked acceptance.
