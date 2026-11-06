# Continuity-v2 devnet activation

The release workflow now has an isolated activation controller instead of the
legacy key replacement, session bootstrap and SOL/SPL smoke steps. This is an
**unqualified hosted-deployment candidate** until an actual devnet run completes.
Local tests and local RPC evidence do not establish a hosted result.

The runtime is merged in [PR #226](https://github.com/mirrorworld-universe/northstar/pull/226).
The oracle bindings are merged in
[vrf-oracle PR #7](https://github.com/mirrorworld-universe/vrf-oracle/pull/7).
Oracle deployment remains a separate manual workflow; this workflow never starts
or dispatches the oracle.

## Activation boundary

`build-continuity-v2.sh` builds Portal and token bridge with
`checkpoint-continuity-v2`, platform-tools v1.56 and SBPFv0 explicitly. It also
freezes a feature-enabled validator, CLI, and `northstar-geyser-events` shared
library from the same checkout and pinned Rust toolchain in a fresh private
staging directory. Its five-ELF manifest binds hashes and the caller-provided
checkout revision; it is not an independent build attestation. Older four-artifact
stages are rejected as new targets; rebuild into a new stage instead of editing
retained manifests. Native-only upgrades may verify an unchanged, manifest-bound
four-artifact stage as the original deployment, never as the new target.
Staging itself performs no activation. Build-generated program keypairs are never
deployment signing identities.

`prepare-continuity-sdk.sh` checks out public NorthStarSDK revision
`24030ce9fc3b80aa21738db2bb9f5a5fbea030d8`, checks its dependency-lock hash,
installs frozen dependencies without lifecycle scripts and builds the bindings.
No private-source credential is needed for this SDK checkout. Both CI validation
and deployment pin Node 24.21.0/npm 11; the helper rejects npm 10 before checkout
because that resolver rejects the unchanged pinned dependency lock.

`continuity_devnet.py run` then:

1. Checks the full devnet genesis, SDK/source/artifact identity, existing validator
   process and private retained metadata.
2. Establishes fresh retained Portal, bridge and upload-buffer keys. It reports a
   rent/funding estimate before any upload or service change.
3. Reconciles/stops the managed oracle with `stop-vrf-service.py`, then deploys the
   isolated programs and verifies their finalized bytes and upgrade authority.
4. Stops the old validator, fingerprints its retained state and records the new
   activation specification. It does not overwrite the old executable, base unit,
   program keys, checkpoint directory, unsettled state or L1 ledger.
5. Installs an isolated validator and event plugin, a read-only plugin config, and
   the systemd drop-in. It replaces executable and Portal arguments and adds
   `--er-geyser-plugin-config`, preserving other service arguments. Existing ER
   plugin flags require explicit review before uploads or service changes.
   Checkpoint and unsettled directories point to the new namespace. Plugin and
   configuration hashes are checked on retries; changed files are not overwritten.
   Directory entries and key/configuration files are flushed before activation.
   Startup must reach the finalized program deployment slots, catch up to the
   current devnet head, and pass the ER event listener health check on every restart.
6. Opens and pins canonical nonce-1 SessionV2 with the full L1 genesis, explicit
   profile and actual validator identity. ER genesis is pinned separately; it must
   not be assumed equal to L1 genesis.
7. Runs the versioned SOL/SPL qualification below, verifies retained old state is
   unchanged and publishes only the public result report.

The durable namespace is
`/solana/northstar/continuity-v2/<source-revision>`. Its control files and signing
keys belong to the runner; runtime data belongs to the validator service user.
The default checkpoint directory follows the retained ledger unless explicitly
overridden. The production validator now honors that override.

Without `--upgrade-existing`, this controller supports only first activation and
retries of that same recorded activation. An existing managed drop-in still fails
closed. Release CD explicitly enables the native-only path below; it never falls
back to a fresh activation when retained upgrade metadata is incomplete.
There is no protocol migration, cursor rewriting or automatic v2 rollover.

## Guarded native-only upgrades

`--upgrade-existing` permits a reviewed **non-protocol** validator/plugin update.
It is not authorization to reuse an old profile under changed execution semantics.
The guard requires the existing managed v2 plan, original activation, upload
receipts, private controls, live arguments, native hashes and journal bindings.
Unknown/manual installations or an unqualified previous native upgrade require
operator review. Both on-chain program ELF hashes must be byte-identical in the
original and target stages, and their finalized accounts must still match the
original deployment receipts. A changed program requires a separate upgrade plan.

The upgrade:

1. Uses the pinned SDK without a signer to verify the current SessionV2 identity,
   nonce, authority, profile, validator, genesis and synchronized ER binding.
   Checkpoint quarantine blocks admission; no quarantine or pending control is cleared.
2. Records an immutable upgrade plan under
   `/solana/northstar/continuity-v2/upgrades/<source-revision>` before stopping the
   service. Existing program IDs, session context/profile and both journal paths
   remain unchanged. No program key, upload or on-chain transaction is created by
   the upgrade controller, and it does not read the deployer keypair.
3. Stops the original service and fingerprints retained journals and original
   private controls. It installs only new validator/plugin copies and their config
   in the upgrade namespace. Only the executable and this controller's ER plugin
   argument change; Portal, L1 plugins and other service arguments are preserved.
4. Checks frozen hashes, ownership, effective systemd arguments and the unchanged
   L1 session through a read-only upstream query before recording a durable start
   barrier. Its L1 recovery floor includes the old local processed/confirmed/finalized
   slots and the upstream processed head immediately before startup. A separate
   durable progress record makes missing established barriers fail closed.
   No retained activation record or program manifest is rewritten.
5. Verifies confirmed/finalized L1 and ER synchronization have reached that floor,
   local RPC/catchup, the same execution session and ER genesis, unchanged
   protocol controls and SSE `/health`. A qualified same-plan retry does not restart
   the runtime; a later upgrade must bind to the previous qualified upgrade record.

A pre-start interruption resumes only with the same plan/artifacts and unchanged
frozen state. A failed initial fingerprint can restart the unchanged original
service before configuration changes. Once native startup was attempted, there
is **no automatic rollback, journal restore or blind restart**. Qualification
failure stops only the known new runtime. An inactive/uncertain attempted start
requires operator reconciliation before retrying; never delete its barriers.

The original profile remains the protocol identity, **not an attestation of the
new native build**. The new source/artifact hashes are bound separately in the
upgrade plan. These checks cannot prove arbitrary native-code compatibility;
review execution and persistence compatibility before enabling this path.
The public report/Slack scope is `native-upgrade`: read-only readiness and retained
identity, **not fresh SOL/SPL payout, oracle, SP1 or proof-acceptance qualification**.
It does not start, stop or reconfigure an existing oracle.

## Required host configuration

- The shared `devnet` runner and existing `northstar-validator.service` are on the
  same host. Do not run independent provisioning commands concurrently.
- Initial inventory requires a running validator with explicit `--ledger` and
  `--portal` arguments, an unchanged executable and the expected devnet identity.
- The runner can use noninteractive sudo for the existing service guard and the
  controller's inventory, file installation and systemd operations. Privileged
  Python calls use `-B` and never write bytecode into the checkout.
- For first activation, the established native deployer is the private regular key file
  `/home/ubuntu/.config/solana/id.json`. It is not the separately configured oracle
  funder. Neither its balance nor any additional funding requirement is implied
  by the oracle wallet's balance.
- Control RPC endpoints are colocated loopback roots. The upstream default is
  `https://api.devnet.solana.com`; the exact complete genesis must match.
- The ER event plugin binds **`0.0.0.0:9101`** with confirmed delivery. Allow TCP
  9101 in the host firewall/security group for external consumers; CD does not
  modify those rules. The stream has **no authentication or TLS**: anyone with
  access can subscribe and consume validator resources, subject to plugin limits.
  The native JSON-RPC binding is unchanged. The local `/health` probe does not
  prove external reachability.

The default `--max-estimated-cost-lamports` is 10,000,000,000. The conservative
estimate includes program rent, the largest upload buffer and a 500,000,000-lamport
operational reserve. It is **an estimate limit, not an enforced wallet-spend cap**.
Completed uploads are excluded from the remaining funding requirement. Insufficient
funding stops before uploads or service changes and reports the public payer and
required amount. There is no automatic airdrop or additional funding transfer.

## Control recovery

SDK bootstrap, deposits, mint/setup, delegation, ER transfer and withdrawals use
`continuity-control-journal.mjs`. Each operation binds its semantic request,
session/network identity and exact signed wire before submission. Saved receipts
bind to that same operation. Missing, changed, aliased or malformed established
state fails closed. No SDK retry may request a second transaction or replace an
expired signature. A retry can rebroadcast only identical valid bytes and never
rebroadcasts after an inclusion observation.

SBF upload is a separate, explicitly recorded **CLI batch**, not an SDK wire
journal. A fixed buffer key and started marker precede each invocation. A retry
can accept the finalized expected program and consumed buffer, but cannot invoke
the uploader again for an unresolved batch. Retain all keys, markers and private
logs for operator reconciliation; do not delete state to force another attempt.

Before funding or execution admission, readiness checks confirmed/finalized L1
slots against the highest validated saved L1 receipt and checks ER synchronization
against that floor. Immutable session equality alone is insufficient: after restart,
a snapshot bank can contain the old session while later mint/registry accounts are
not yet visible in the confirmed view.

If the initial retention snapshot fails before configuration changes, the controller
attempts to restart the unchanged original service. After activation begins there
is no automatic fallback to old protocol state. Uncertain uploads, expired control
transactions or checkpoint quarantine require review; they are not repaired by
clearing journals or copying a Portal root.

## SOL/SPL qualification

The controller uses `devnet-continuity-smoke.mjs` and the pinned SDK, not the legacy
Portal CLI smoke scripts:

- Bootstrap and verify the immutable SessionV2 context.
- Credit a test SOL payer with 10,000,000 lamports, restart the validator, verify
  recovery and require a finalized L1 payout of 1,000,000 lamports.
- Fund the runtime manager and SPL fee payer, register a test vault, delegate two
  owner accounts, credit 600,000,000 token units and transfer 250,000,000 on ER.
- Restart again, verify the 350,000,000/250,000,000 ER balances, withdraw 200,000,000
  units and verify finalized L1 payout, 400,000,000 remaining vault custody,
  50,000,000 remaining ER destination balance and Portal ownership on L1.

The release result includes program identities, profile/context and public
qualification receipts. CI uploads only that result file, never keys, signed-wire
journals, private configuration or command logs. Slack reports success only after
this gate passes. The oracle remains stopped until its manual workflow is run
against the exact new context.

## Local evidence and validation

[The retained local report](../evidence/continuity-v2-activation-local.json) records
fresh non-default Portal/bridge identities, matching loaded ELF hashes, both
validator restarts and SOL/SPL payouts. It uses the feature-built local
`solana-test-validator` at runtime revision `34d642a070`, not a hosted systemd
activation. It retains its explicit genesis feature-deactivation setting.
It does not qualify hosted devnet, an oracle callback, controlled interruption of
partial checkpoint metadata, graceful shutdown, SP1 or proof acceptance.

CPU-only operator checks need no chain, service or deployment credential:

```sh
python3 -B .github/scripts/test-build-continuity-v2.py
python3 -B .github/scripts/test-continuity-rollout-state.py
python3 -B .github/scripts/test-continuity-devnet.py
python3 -B .github/scripts/test-continuity-devnet-model.py
python3 -B .github/scripts/test-native-upgrade.py
bun test ./.github/scripts/continuity-control-journal.test.mjs ./.github/scripts/devnet-continuity-smoke.test.mjs ./.github/scripts/native-upgrade-snapshot.test.mjs
bash -n .github/scripts/prepare-continuity-sdk.sh
```

The host model checks complete activation, same-plan retries, included/unobserved
upload recovery, changed running services, missing plans and pre-activation cleanup.
Native upgrade models additionally cover pre-plugin retained stages, unchanged
program/session/journal bindings, no-funder operation, interrupted retries,
quarantine, uncertain starts, qualified retries and subsequent native upgrades.
They do not execute systemd or deploy programs. A real pinned SDK checkout/install/
build and the local RPC adapter lifecycle have also passed separately. Hosted
execution is still required before claiming this rollout or oracle deployment.
