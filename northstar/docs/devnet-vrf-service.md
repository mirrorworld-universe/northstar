# CI-owned VRF service on the devnet runner

The existing `CI` workflow's `deploy` job owns this integration. It runs on a push
to `release`, on the same persistent `devnet` runner that deploys/restarts Northstar.
It is not a cross-repository `workflow_run` listener or another job sharing a label.

## Release ordering

1. Build/validate the normal Northstar artifacts.
2. Stop the managed oracle **before** Portal changes or validator restart. Inspect
   its deployment-bound journal with the offline Rust validator. Under the deployment
   lock, validate/reconcile every indexed administrative intent without signing.
   Pending/retryable/unresolved, incomplete program uploads or missing/corrupt state
   blocks deployment and restores a previously active worker. A missing systemd unit
   does not bypass retained state; receipt-cache updates preserve file ownership.
3. Complete existing Portal/token-bridge deployment, session setup, validator restart
   and SOL/SPL smoke tests. The smoke tests themselves restart the validator.
4. If enabled, checkout the approved oracle revision and its pinned Coordinator/Bai
   sources. Run the oracle's full devnet controller on this runner.
5. Require a successful ER callback before enabling the persistent systemd service.
   Upload only its public deployment report. Any failure fails the deploy job.

The separate programs are **CI-owned**. The controller never upgrades shared
Coordinator `FTDEMx…` or shared verifier `2CoJJ…`. Program/admin/VRF/fulfiller identities
persist outside the checkout. The consumer is compiled for this CD run's actual Portal;
its owner-program instruction stages and signs its PDA delegation on L1. Coordinator
requests remain ER-native. No SP1 or L1 settlement qualification is claimed.

## Activation inputs

These source changes do not enable deployments or provision credentials. Configure
in **mirrorworld-universe/northstar**, after reviewing the corresponding oracle commit:

- Secret `NORTHSTAR_VRF_SOURCE_READ_TOKEN`: contents-read access to the private
  `vrf-oracle`, `vrf-coordinator-program-library` and `vrf_verify_contract` repositories.
  The workflow does not persist checkout credentials.
- Variable `NORTHSTAR_VRF_REVISION`: the approved full 40-character oracle commit.
- Variable `NORTHSTAR_VRF_FUNDER_KEYPAIR`: absolute path to an approved, funded,
  regular `0600` **devnet-only** keypair on this runner, outside the checkout.
- Variable `NORTHSTAR_VRF_MAX_FUNDING_LAMPORTS`: explicit admin top-up cap per
  invocation, positive and at most 50 SOL. Missing balance fails; no faucet use.
- Variable `NORTHSTAR_VRF_ENABLED`: set `true` last.

The runner needs passwordless `sudo`, systemd, Rust, Solana CLI, Python 3.12 and
access to the pinned program dependencies. The workflow sets up Python and installs
`cargo-build-sbf 4.5.0` into its own tools prefix. Building the oracle happens outside
the Northstar Cargo workspace.

The release controller reads `--portal` from the validator's installed ExecStart,
uses local L1/ER RPC ports 8899/8910, verifies the full devnet genesis and active
session, checks program authorities and exact deployed bytes, deposits funds,
delegates consumer state, then starts and exercises the worker.

Do not set the enable flag without the credential/funding inputs. Do not push
`release` for a syntax check: the existing job performs real devnet deployment.
Ordinary pushes to the oracle repository do not deploy; update the approved revision
and promote a Northstar release through the normal review process.

## Lifetime and recovery

`northstar-vrf-oracle.service` is installed under systemd, not left in a GitHub Actions
process group. It uses a DynamicUser, persistent private state and only two loaded
credentials: fulfiller and VRF. Admin/program/funder keys remain inaccessible to it.
The worker stops with the validator; CD re-enables it only after the full bootstrap
and callback check. Disabling the flag stops/disables a previously managed worker
on the next Northstar deployment.

Private deployment keys/plans/logs stay under `/solana/northstar/vrf-service`.
Worker journals stay under `/var/lib/northstar-vrf-oracle/journals`; installed public
configuration is `/etc/northstar-vrf-oracle/config.json`. Never remove these to bypass
an unresolved-intent gate. Restore approved backups and reconcile outcomes first.

The controller binds session incarnation before its first mutation, including failed
initial bootstraps. Existing Coordinator governance is checked before funding/upgrades.
A program CLI interruption leaves a durable pending-program barrier requiring explicit
operator reconciliation. Never clear it merely to rerun CD.

Bootstrap failure stops/disables the service without erasing state or rolling back
L1 upgrades. Do not concurrently run a manual bootstrap or restart the validator.
The worker is development-only, with bounded attempts/records and no automatic
archival, key rotation, HA or production SLO.

Full implementation and operational instructions live in the selected oracle commit's
[`deploy/README.md`](https://github.com/mirrorworld-universe/vrf-oracle/blob/main/deploy/README.md).
The source also includes deterministic deployment-guard tests and a local Northstar
qualification profile using the actual Bai verifier. Neither is a hosted CD success claim.
