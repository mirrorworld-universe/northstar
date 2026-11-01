# Continuity-v2 devnet rollout preparation

This is a **staging-only draft**, not a native deployment or checkpoint-recovery
remediation. The release workflow stages artifacts, then stops at an explicit hold.
It does not activate v2, change a Portal cursor or replace retained runtime state.

The runtime foundation is merged in [PR #226](https://github.com/mirrorworld-universe/northstar/pull/226).
The oracle worker/controller/consumer bindings are merged in
[vrf-oracle PR #7](https://github.com/mirrorworld-universe/vrf-oracle/pull/7).
Its separate [local Bai report](https://github.com/mirrorworld-universe/vrf-oracle/blob/a22bb1150b89eb2e9bce4faf789039b35c169140/docs/evidence/continuity-v2-er-bai.json)
qualifies owner-authorized ingress/delegation, an ER verifier-CPI callback and worker
journal reopening. It does not qualify hosted rollout, validator restart,
interrupted settlement, L1 payout or SP1/proof acceptance.

## Build without activation

The current release workflow builds Portal/bridge/runtime without the v2 feature
and builds the CLI with `--no-default-features`. Its session bootstrap is legacy.
Changing a program address or reusing old journals cannot fix that mismatch.

From an approved development environment, choose a new absolute staging directory
whose parent exists, outside active runtime/journal directories:

```sh
bash .github/scripts/build-continuity-v2.sh "$NEW_STAGING_DIRECTORY"
```

The helper:

- Builds Portal and token bridge with `checkpoint-continuity-v2`, platform-tools
  v1.56 and SBPFv0 explicitly; the backend must already be available.
- Builds a feature-enabled validator and CLI into a separate build cache, then
  freezes their outputs in the new staging directory.
- Refuses existing directories/symlinks and never removes retained artifacts.
- Publishes a hashed completion manifest only after all four ELF files exist.
- Does not install tools, submit transactions, modify a service or access journals.
  The manifest explicitly says activation, hosted qualification and proof acceptance
  are false. SBF tooling may create build keypair files inside private staging;
  these are not approved deployment identities and are never used for signing.

`NORTHSTAR_CONTINUITY_BUILD_TARGET_DIR` selects the compiler cache, not an active
runtime directory. Failed builds retain partial diagnostics/artifacts without a
completion manifest. Use a new staging directory after inspecting the failure.

## Guarded release workflow

**CD intentionally holds after artifact staging and before the existing VRF stop,
deployment-key generation, chain deployment or validator restart steps.** A release
job fails at that explicit hold while leaving active state untouched. This is not
a working deployment workflow. Do not remove the hold merely to make CI green.

Validate the staging contract and hold ordering with:

```sh
bash -n .github/scripts/build-continuity-v2.sh
python3 -B .github/scripts/test-build-continuity-v2.py
```

The tests use fake compiler outputs. They validate arguments, artifact hashing,
retained-file protection, failure behavior and the activation boundary, not a fresh
native compilation.

## Required before activation

1. Inventory and preserve old program keys, unsettled snapshots/journals,
   checkpoint plans, worker journals and control records separately. Do not reset
   missing or quarantined state to manufacture a healthy cursor.
2. Select fresh native program identities and an isolated runtime/lineage namespace.
   Stage the service configuration so rollback never reuses v1 state as v2.
3. Open SessionV2 with the exact full devnet genesis, explicit nonzero checkpoint
   profile and actual validator identity. Bind its canonical context and nonce
   before any oracle setup transaction.
4. Port the release smoke invocation to supported v2 consumers. The existing
   legacy CLI SOL/SPL smoke steps are not v2 qualification. Require actual hosted
   SOL/SPL payout and restart results with bounded diagnostics.
5. Reconcile retained controls, confirm native health, and run the oracle-owned
   manual deployment/callback workflow against that exact fresh context.

These activation steps remain outstanding. No hosted mutation or funding request
is authorized merely by a successful staging build.
