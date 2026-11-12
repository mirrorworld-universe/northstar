# Guarded same-ID devnet Portal upgrades

A Portal program update is not a native-only upgrade. The ordinary controller still rejects different on-chain artifacts unless an explicitly selected, separately qualified Portal receipt binds the transition.

## Admission

- Use a reviewed master revision containing the continuity-v2 challenge-entry guard.
- Require a qualified running Native binding and an online snapshot of its retained SessionV2 identity and ER genesis.
- Preserve Portal and ProgramData addresses, the upgrade authority, token-bridge bytes, all session accounts and Native journal paths.
- Build with platform-tools v1.56 and SBPFv0. The bridge artifact must equal its retained manifest hash.
- Use only the CI-owned upgrade authority. A personal-wallet fallback is not permitted.
- Bound temporary buffer rent, any ProgramData extension and fee reserve by an 8-devnet-SOL peak cap.

## One submission

Run `.github/scripts/portal_program_upgrade.py --inspect-only` first. Inspection creates no managed namespace, key, transaction or service change. It returns a digest binding source, existing Native/program observations, target artifacts, session snapshot, CI authority balance and peak quote.

Submission requires that exact digest. It records the plan, original buffer key and submission barrier before invoking the retained manifest-bound CLI. An uncertain result preserves all records and permits no automatic resubmission, rollback or runtime restart.

Qualification requires a finalized successful upload transaction at the new ProgramData deployment slot, exact target bytes, unchanged authority and bridge, a closed buffer, and an unchanged Native/session/ER snapshot. The receipt records the net CI-wallet cost so later VRF staging reserves that cost within the combined cap.

The operator workflow remains in the private deployment repository; no credential, private signer implementation or deployment state is published here.

## Native handoff

Set `NORTHSTAR_PORTAL_UPGRADE_RECEIPT_SHA256` to the qualified receipt's canonical digest before release promotion. The release workflow freezes it as `--portal-upgrade-receipt-sha256`. Fresh activation and failed-L1 recovery cannot use this capability.

The Native plan binds the exact receipt and verifies current ProgramData against its qualified after-state. Original activation/upload records remain unchanged. Later ordinary upgrades carry the qualified receipt forward without another program submission. Remove the one-release selector after successful qualification.

This updates an operator-backed continuity-v2 checkpoint flow. It does not qualify a challenge resolver, trustless execution finality, SP1 acceptance, threshold VRF or production key safety.
