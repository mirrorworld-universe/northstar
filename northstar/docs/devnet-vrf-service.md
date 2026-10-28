# Manual oracle deployment on the devnet host

Northstar owns validator deployment. The
[`vrf-oracle` repository](https://github.com/mirrorworld-universe/vrf-oracle)
owns a **manual** Actions workflow for Coordinator/Bai/consumer provisioning and the
persistent oracle service. Northstar does not dispatch it automatically.

## Release ordering

1. Before Portal changes or validator restart, `stop-vrf-service.py` stops the
   managed oracle and confirms inactivity. It validates the complete bound worker
   journal and indexed administrative receipts without signing transactions.
2. Pending or unknown outcomes, incomplete uploads and missing/corrupt established
   state block deployment. On inspection failure, a previously active oracle is
   restarted against the old ER so it can reconcile. Missing units do not bypass
   shutdown confirmation, worker reservations or orphaned-state checks.
3. Native CI deploys programs, restarts the validator, opens its session and completes
   its existing SOL/SPL smoke tests. The managed oracle remains stopped/disabled.
4. After native CI succeeds, an operator runs **Deploy VRF on devnet** in the oracle
   repository on `main`, supplying the current Portal ID and other public inputs.
   That workflow rechecks devnet/session identity, provisions only dedicated programs,
   starts the oracle and requires a confirmed callback before reporting success.

## Setup

**No new Northstar secrets or variables are required.** Its existing `devnet` runner
label is unchanged. Make that same persistent runner registration accessible to
vrf-oracle, preferably one organization runner shared by both repositories. Do not
register a second runner process on the same host or place multiple hosts behind
the selected label. One registration serializes native and oracle deployment jobs.

The oracle repository needs only:

- `SOURCE_READ_TOKEN`: fine-grained token with Contents read on the private
  Coordinator and verifier repositories.
- `DEVNET_FUNDER_KEYPAIR`: a funded, disposable devnet Solana keypair JSON secret.

Runner label, Portal ID, loopback L1/ER RPC URLs, funding cap and invocation name are
manual workflow inputs. Oracle/program revisions come from its checked-out `main`
commit and source lock. No GitHub App, cross-repository token with Actions write,
enablement flag, approved-revision variable or CI-run identity input is needed.
See the oracle [deployment runbook](https://github.com/mirrorworld-universe/vrf-oracle/blob/main/deploy/README.md)
for exact inputs, permissions and recovery steps.

## Retained state and recovery

Stable keys, control plans/intents and reports live under
`/solana/northstar/vrf-service`. Its `worker-state.json` reserves the current worker
binding/configuration independently of activation success. Journals live under
`/var/lib/northstar-vrf-oracle/journals`; installed public configuration is
`/etc/northstar-vrf-oracle/config.json`. Systemd supplies only fulfiller/VRF credentials;
admin/program/funder keys stay outside the worker's access.

The service stops with `northstar-validator.service`. After a native deployment,
restart it through the manual workflow, not an automatic release notification.
Never delete journals, reservations or signed intents to force a deployment through.
Unknown upload/transaction outcomes require explicit reconciliation. Failed oracle
bootstrap does not roll back L1 upgrades or remove delegated test accounts.

Do not concurrently run another provisioner or restart the validator outside this
workflow ordering. Repository Actions concurrency is not cross-repository fencing;
one shared runner registration is required. The controller's deployment lock protects
state and cleanup, but no host-handoff/provenance protocol remains. Root/runner/RPC
are trusted. Forced process loss may bypass cleanup and requires operator inspection.

The service remains development-only. Hosted workflow/systemd execution is unvalidated;
local Bai-profile evidence is not hosted success, SP1 or L1-settlement qualification.
