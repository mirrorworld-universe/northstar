# Oracle-owned devnet deployment

Northstar owns validator deployment and its restart safeguards. The
[`vrf-oracle` repository](https://github.com/mirrorworld-universe/vrf-oracle)
owns Coordinator/Bai/consumer provisioning, the persistent oracle service and its
callback smoke test. A push to either default branch does not activate this flow.

## Release ordering

1. The existing release job builds Northstar artifacts. Before Portal changes or
   validator restart, `vrf-handoff.py begin` acquires the host handoff lock and runs
   the retained-state stop guard. That guard validates the complete worker journal
   and reconciles indexed administrative receipts without signing. Unresolved
   work, incomplete uploads or missing/corrupt state block deployment and restore
   a previously active worker. A missing unit does not bypass inactivity confirmation,
   worker reservations or orphaned state trees.
2. A durable `deploying` handoff invalidates the previous native generation before
   chain/node changes. Northstar deploys Portal/token bridge as needed, opens its
   session, and completes SOL/SPL smoke tests and the existing notification step.
3. The final native step publishes a root-owned `ready` handoff. It binds the source
   run/attempt/commit, runner name, enablement, Portal, session incarnation, validator,
   genesis values and running process identity. Settlement progress is not included
   in the session incarnation digest. Ordinary failure marks the owned generation
   `failed`; a stale finalizer cannot replace another generation.
4. Only after the **entire `CI` workflow succeeds**, `vrf-dispatch.yml` requests
   `deploy-devnet.yml` on the oracle repository's protected `main` branch. Its
   `workflow_run` listener lives in Northstar, not in the receiving repository.
   The dispatch contains public source provenance only.
5. Oracle independently verifies the successful release run and native deployment
   job through GitHub's API. It verifies that its job uses the **same organization
   runner ID**, not merely the same label or name. Under the host lock it compares
   the current handoff with fresh runtime observations before service/key changes.
6. Oracle builds its approved immutable revision and pinned program sources,
   provisions only CI-owned programs, delegates consumer state, starts the worker,
   and requires a confirmed callback with Coordinator/Bai/consumer CPI evidence.
   Its workflow owns the success report and deployment outcome.

The notifier does not assert oracle readiness. If dispatch or bootstrap fails,
Northstar can be healthy while the oracle remains stopped. Monitor both workflows.
Existing local Bai-profile evidence is not hosted workflow/systemd qualification.
No SP1 or oracle L1-settlement qualification is claimed.

## One shared organization runner

Register the devnet runner **once at organization scope**, and grant only the
approved Northstar and oracle deployment workflows access. Give that registration
a host-specific label, such as `northstar-devnet-1`. Configure both repositories
with the same exact runner name and label. Do not register a second agent on the
host, create per-repository registrations, or use a pool behind the label.

A single registration serializes deployment jobs across repositories. GitHub
Actions concurrency groups alone do not: they are repository-scoped. The host
handoff lock adds a local fence; a native `deploying` marker blocks oracle startup
between native workflow steps. Oracle holds that lock through provisioning and
cleanup. This is not distributed fencing or HA. Do not restart the validator or
invoke another provisioner outside this protocol.

## Activation inputs

These changes do not provision credentials, grant runner access, enable variables
or push `release`. Before activation, publish/review the companion oracle workflow
and configure its protected `devnet` environment. Both repositories must have the
new workflow files on their default branches; the native handoff code must also
reach `release` through the normal release process.

Configure in **mirrorworld-universe/northstar**:

- Variable `NORTHSTAR_DEVNET_RUNNER_NAME`: exact shared organization runner name.
- Variable `NORTHSTAR_DEVNET_RUNNER_LABEL`: host-specific label; generic `devnet`,
  `self-hosted`, `linux` and `x64` labels are rejected when enablement is requested.
- Variable `NORTHSTAR_VRF_DISPATCH_APP_ID` and secret
  `NORTHSTAR_VRF_DISPATCH_APP_KEY`: a GitHub App installed on **vrf-oracle only**,
  with Actions write permission. The listener mints a short-lived dispatch token.
- Variable `NORTHSTAR_VRF_ENABLED`: set `true` only after both sides are ready.

Oracle revision approval, private source-read credentials, funding key path and
funding cap now belong to **vrf-oracle**, not Northstar. Its
[`deploy/README.md`](https://github.com/mirrorworld-universe/vrf-oracle/blob/main/deploy/README.md)
lists the receiving workflow's inputs and permissions. The ordinary repository
`GITHUB_TOKEN` is not a cross-repository credential. No broad personal token is needed.

The host needs systemd, passwordless deployment `sudo`, Linux `/proc`, Python 3.10+
for the native observer and the existing Northstar toolchain. Oracle sets up Python
3.12 and its pinned Rust/SBF tools. Update the Actions runner to support Node 24
before using `actions/create-github-app-token@v2`.

## Persistent state and recovery

- `/var/lib/northstar-deployment/current.json`: root-owned public handoff. Its
  directory is the shared advisory lock; do not replace/delete it while a job runs.
- `/usr/local/lib/northstar-deployment`: native observer and retained-state guard.
- `/solana/northstar/vrf-service`: private oracle keys, plans, intents and reports.
  Its `worker-state.json` reserves the current binding/configuration before startup,
  independently of activation success. Missing or mismatched configuration blocks
  restart even when the unit is absent.
- `/var/lib/northstar-vrf-oracle/journals`: deployment-bound worker journals.
- `/etc/northstar-vrf-oracle/config.json`: installed public worker configuration.

The systemd worker has a DynamicUser and only the fulfiller/VRF credentials;
admin/program/funder keys remain outside its access. It stops with the validator.
A successful oracle bootstrap enables it independently of the Actions process.
Disabling either repository flag does not interrupt an already running job or
service. Disabling the native flag stops/disables the worker at the next guarded
native deployment and publishes a handoff that refuses oracle activation.

A failed stop gate leaves the previous handoff intact and precedes native mutation.
A force-killed native job may leave `deploying`; automatic deployment then blocks.
Reconcile that exact native run's node/program outcomes before an operator marks
its handoff failed. Never delete the handoff or private journals to force progress.

An old or delayed dispatch is rejected if the native generation, process, session
or enablement changed. A partial GitHub rerun without a matching native deployment
attempt is also rejected; rerun the complete release deployment. Retry the notifier
only for the current successful native generation. Identical source-generation/
approved-oracle-revision requests reuse the same controller invocation and receipts.

Oracle cancellation performs cleanup inside the host lock when Python receives
SIGINT/SIGTERM; further signals during failure handling do not interrupt cleanup.
Receiver-only retries independently stop the unit and validate retained worker state.
Forced process/runner loss may bypass cleanup. Inspect the unit,
retained control index and worker journal before recovery. Unknown program uploads
remain barriers requiring explicit review. Bootstrap does not roll back L1 upgrades,
delete delegated accounts or discard signed intents.

The service remains development-only: no automatic archival, rotation, HA or
production SLO. Root/runner administrators and colocated RPC are trusted; process
and RPC comparisons are not authenticated execution witnesses.
