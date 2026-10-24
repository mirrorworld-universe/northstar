# Add ER-native VRF development execution and bounded replay proofs

- Run the pinned coordinator/verifier/consumer inside ER, with atomic captures, authenticated session provenance and versioned success/rollback fixtures.
- Add bounded interpreted SBPF replay, an authenticated SP1 guest and additive kind-3 Portal key/profile bindings; preserve ordinary JIT execution and legacy kind-2 behavior.
- Add runtime-ELF proof tooling, strict guest controls and restartable fresh-session Production resolution; retain actual proofs, signed receipts and failure history.
- Document the privacy boundary and a November proving-direction proposal without claiming team approval or production readiness.

## Validation

- Whole-workspace clippy, configured Portal strict clippy, nightly formatting and diff checks pass.
- Native replay/binding, JIT-retention, legacy Portal and actual-SBF regressions pass; real-input Python suites pass 74 + 55 tests.
- Selected guest: two positives, all fifteen rejection controls and two valid rollback witnesses pass.
- Fresh 100-cycle ER window: 304 successful transactions, 200 atomic captures, zero lost/duplicate callbacks.
- Three fresh SDK-verified proofs resolve valid through actual local Production: **111,253 / 109,753 / 111,253 CU**. Published copies pass exact native reexecution, signed-receipt auditing and SBF proof/mutation checks.

## Limits

A10G prove+wrap takes **245.493 / 204.670 / 210.837 s**, missing 120 s. The initial CUDA backend failure and explicit recovery are retained, not presented as a backend fix. L40S qualification, independent human reproduction/integration approval, production oracle operations and hosted/cross-layer integration remain open. The configured profile is development-only.

Review: [evidence and boundaries](october-vrf-development-evidence-v1.md), [actual three-case artifacts](../zkvm-vrf/evidence/g5-a10g-v11/README.md).
