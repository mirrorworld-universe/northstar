# A10G bounded-VRF development evidence v11

Three genuinely new local L1 sessions and matching ER captures produced SDK-verified proofs and **three valid `ResolveChallenge(Production)` resolutions**. This is local development evidence, not production rollout, hosted/delegated acceptance or completion of every KR.

| Case | Operation | Prove + wrap | SDK verify | Whole Production instruction |
| --- | --- | ---: | ---: | ---: |
| caseA-request | Request | 245.493 s | 0.488 s | 111,253 CU |
| caseB-callback | Verification/callback | 204.670 s | 0.492 s | 109,753 CU |
| caseC-hard100 | Maximum-try callback selected from a fresh 100-cycle window | 210.837 s | 0.487 s | 111,253 CU |

**130,000-CU gate: met locally. 120-second proof gate: not met.** The machine is g5.xlarge/NVIDIA A10G, not L40S. Initialization/setup, proof time and SDK verification remain separate. Ledgers were durably paused between phases; these are not live-network end-to-end latency measurements.

The final runtime window contains 100 consecutive callbacks, 304 successful signed transactions, 200 authoritative atomic captures, zero failed transactions and zero lost/duplicate callbacks. The hardest observed fixture used six encode-to-curve tries; no particular try count was fabricated or guaranteed. See the per-case `runtime.json`.

## Identities

- Guest ELF: `b0531ecbe24a339b60407673181274a83aeafeb2d3cc04057369ff5992829a92`.
- SDK program key: `0x00f8e1033f6031fff7297eadacd389d84d78850c1bda406914e771bb69282abe`.
- Typed policy: `c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d`.
- Actual configured SBF: `174001333ad0126a676953f52f244fb6755355eeab0d181391e20509b2fd728e`.
- SP1 SDK 6.8.0, outer circuit v6.1.0; kind 3/version 1, one step, zero transaction fees, admission age 900, eight public fields/256 bytes.

`source.json` records the compiled source snapshot and matching guest-relevant repository file digests. `deployment.json` and `setup.json` distinguish the deployment JSON digest from the typed policy digest. `sdk-verifying-key.bin` is public verification material, not a secret.

## Retained failures and controls

The first fresh batch stopped in an ICICLE/CUDA outer-prover memory fault after successful guest execution; no proof was produced and the other two cases were unattempted. A device-context health check preceded one explicit recovery batch using the same immutable preparations, guest/key and hardware. All three recovery proofs SDK-verified and then resolved through their actual local Portal sessions. **The failed attempt is not hidden or counted as a guest refusal.** See `attempts.json`; this does not establish first-attempt prover reliability.

The selected guest's complete corpus observed two exact-output positives and fifteen actual nonzero-exit/empty-output controls. Two additional valid failed-transaction witnesses executed with zero guest exit and exact expected rollback public output. Infrastructure errors, missing reports and timeouts never satisfy the rejection observer. `guest-controls.json` and `valid-failed-transaction-controls.json` retain those observations without proof-acceptance claims.

A separate earlier selected-guest calibration took 282.566 s prove+wrap and 0.498 s verification, outside this three-case window. Standalone SBF verification measured 84,262 CU for that calibration. Independent checks of each published fresh proof measured 84,263 CU and rejected all eight changed proof positions and eight changed public fields. Those standalone metrics are distinct from the complete resolution numbers above.

## Files and checks

Each `cases/` directory contains the actual proof/public bytes, compressed canonical envelope, public session/preparation, signed RPC receipts, before/after resolution state, SDK proof record, receipt audit and runtime aggregate. Private signers, ledgers and raw operational logs are excluded. The raw guest executable is omitted because it contains compiler build-tree paths; its digest, source/lock/build recipe and public verification artifacts are retained.

From this directory:

```sh
sha256sum -c SHA256SUMS
```

From the repository root, with the documented read-only SDK dependencies available:

```sh
node northstar/scripts/vrf-candidate-portal-receipts.mjs \
  northstar/zkvm-vrf/evidence/g5-a10g-v11/cases/caseA-request
```

The receipt checker verifies signed-wire/RPC/instruction/CU consistency. It is an offline structural audit, **not a consensus certificate**. SDK proof verification and actual SBF/L1 resolution are separate observations. To reexecute an envelope, decompress its `witness.borsh.gz` and use `vrf-sbpf-replay verify-envelope` with the typed policy above. The existing external-artifact SBF verifier test can independently verify each proof/public pair against the supplied compressed SBF after decompression.

Human integration approval, independent human reproduction, hardware acceptance amendments, production secret handling and hosted/cross-layer integration remain separate pending work. No paid proving or public-cluster transaction is represented here.
