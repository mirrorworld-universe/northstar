# Read-only VRF qualification telemetry

The relay already emits structured `simulation_rejected` and `simulation_unavailable`
events to Native stderr. `.github/scripts/vrf_telemetry.py` exposes bounded observations
of those events without changing the validator, signer, native JSON-RPC, or Geyser.
It does not submit transactions, start or stop services, read credentials, or take a
live journal writer lock.

## Admission and privacy

Each sample requires the selected runtime's qualified retained-upgrade plan, report,
private controls, installed drop-in digest, effective systemd command, runtime artifact
identity, and exact relay profile. The validator must remain active with `Restart=no`
and zero restarts. An explicit full-hash sample additionally checks the executable bytes.
The sampler checks the PID and invocation again before returning its result.

Native and telemetry source revisions are intentionally distinct. Installing this
Python tool does not require a Native restart or a release promotion. The private
qualification workflow accepts only a tooling-only Native tree difference.

The output contains public request addresses, event kind, observed slot, timestamp,
expiry flag, and whether the simulation returned an error. It never contains the raw
error, journal message, command line, account contents, transaction bytes, or credentials.
The public workflow removes the private incremental journal cursor from its artifact.

## Historical correlation

Dispatch **VRF relay telemetry (read-only)** on `master` with:

- The exact already-qualified runtime revision and retained relay profile digest.
- An explicit UTC `since` and `until`, no more than 26 hours apart.
- Optionally a public request prefix of at least six base58 characters.

For example, the reported preflight-isolation interval can be inspected using
`2026-11-17T01:44:00Z` through `2026-11-17T01:59:00Z` and prefix `3s5CG3`.
A matching structured event is positive pre-dispatch evidence. External callback
outcomes alone do not establish that simulation occurred.

The query has fixed byte, entry, event, and elapsed-time limits. An unavailable sink,
failed journal query, or exceeded limit is explicit; unavailable counts are `null`,
not verified zeroes. A zero means no matching event was observed in the retained query,
not that no such event ever occurred. The tool does not prove journal retention or
absence of journal rate limiting. Program log messages are not Native event evidence.

## Coordinated qualification

The private observer combines this public sampler with bounded, credential-free signer
and relay metadata. It records a hash-linked sample stream, cumulative hot-plus-archive
counts, resource observations, checkpoint health, and public relay receipt identities.
No metrics listener or new authenticated signing endpoint is introduced.

Use a single coordinated 1,000-request client campaign, not a parallel load generator.
A separate 24-hour observation begins only after that campaign independently qualifies.
The canary client uses fresh request PDAs at five-minute intervals. The observer never
generates either workload. Its client manifest and finalized receipt checks are separate
from Native-upgrade readiness.

A qualified report requires complete measured coverage, unchanged runtime/signer bindings,
no counter regressions, exact request counts, finalized successful ER receipts, matching
Coordinator/Verifier/Consumer CPI bytes, local public-proof verification, callback output,
and request closure. Nominal ER deadlines remain 24,000 slots under the existing
24,000/24,000/65,535 policy; the nominal 10 ms budgeting model is not a wall-clock SLA.
The report is not an L1 checkpoint-settlement or SP1 qualification.
