# October VRF development evidence v1

**2026-10-24 development snapshot. Not production acceptance or a declaration that every October KR is complete.**

## Delivered boundary

The prototype runs the unchanged coordinator/reference-verifier/test-consumer composition from library commit `d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd` inside local Northstar ER. It uses RFC 9381 ECVRF-EDWARDS25519-SHA512-TAI suite 1: 80-byte proofs and 64-byte outputs. The earlier Ristretto CPU foundation is not a compatible replacement.

An opt-in atomic capture records signed wire, complete account presence/absence and effects, loader/ProgramData/ELF identity, features/sysvars/fee policy, invocation/return producer and rollback outcome at the execution boundary. RPC observations alone are not witnesses. A canonical producer rejects mismatched actual Portal session, manager, nonce, genesis and source identity rather than relabeling captures.

The separately keyed kind-3/version-1 SP1 relation reexecutes the actual SBPF through the bounded Agave interpreter. It authenticates input and checkpoint context and compares the complete recomputed outcome; it does not trust asserted traces, effects or curve answers. The legacy kind-2 route remains separate. The eight public fields/256-byte order and 149-byte session-context encoding are unchanged.

The development policy is `c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d`: one checkpoint step, admission age 900, zero transaction fees and no cross-layer settlement. Its digest is not the ELF-containing deployment JSON digest. Program/runtime/profile changes need their own reviewed guest/key and evidence window.

## Observed results

| Dimension | Actual observation | What it does not establish |
| --- | --- | --- |
| Local ER runtime | One fresh configured-session clean window: 100 cycles, 304 successful signed transactions, 200 atomic captures, zero lost or duplicate callbacks | Hosted/delegated ER acceptance or cross-layer settlement |
| Failure semantics | Separate real-session invalid-proof and callback-failure captures preserve rollback; both valid failed-transaction witnesses execute in the selected SP1 guest with zero guest exit and exact expected public output | Successful callbacks or final-three lifecycle samples |
| Selected guest | ELF `b0531ecbe24a339b60407673181274a83aeafeb2d3cc04057369ff5992829a92`, real SDK key `0x00f8e1033f6031fff7297eadacd389d84d78850c1bda406914e771bb69282abe` | An approved production key or arbitrary SVM coverage |
| Real request/callback CPU checks | 72,766,349 / 78,599,972 / 82,819,098 cycles; all three exact 256-byte native public outputs | Equivalent proof-time reduction |
| Strict selected-guest corpus | 17/17 actual cases: two zero-exit exact-output positives and fifteen nonzero-exit/empty-output controls; separate real-guest SHA known answers also pass | Refusals inferred from host errors, missing reports or timeouts |
| Selected-guest calibration | One SDK-verified prior-session proof: **282.566 s prove+wrap**, **0.498 s SDK verification** | A final-window sample or the 120-second gate |
| Three fresh local lifecycles | Three SDK-verified proofs and valid real Production resolutions: **245.493 / 204.670 / 210.837 s**, **111,253 / 109,753 / 111,253 whole instruction CU** | The 120-second gate, L40S equivalence or production rollout |
| Selected-key Portal SBF | New artifact `174001333ad0126a676953f52f244fb6755355eeab0d181391e20509b2fd728e` accepts the actual selected proof at 84,262 whole standalone verification CU; 17 mutation checks reject, historical candidate-key rejection and legacy regressions pass | Full Production resolution CU or final-three lifecycle acceptance |

The calibration row uses a prior configured-session input outside the final three-case lifecycle window. The three fresh proofs instead bind their genuinely new configured sessions and exact native outputs; their complete Production resolutions are independently recorded. The selected guest is built from the integrated source snapshot with SHA acceleration and invocation-local reuse of already authenticated work; public standalone validation still checks its own inputs. Native candidate interpretation is scoped per program entry, preserving ordinary validator JIT behavior under workspace feature unification.

Historical baseline proofs took 309.924 s initially and 312.347 / 262.307 / 262.271 s for same-input characterization. Those are not three distinct acceptance cases. The old per-process control runner timed out after partial success; its failure remains retained separately from the later complete observed corpus. Neither timeout nor SDK infrastructure failure is counted as a guest refusal.

## Gates still open

- **Latency:** all three fresh proofs miss 120 seconds. The complete resolution CU gate is met locally; no CPU-cycle or hardware-normalized projection substitutes for measured proving time.
- **Backend reliability:** the first fresh batch stopped in an ICICLE/CUDA memory fault after valid guest execution. One explicitly recorded recovery batch succeeded; this is not a demonstrated backend fix or clean first-attempt reliability.
- **Hardware and review:** A10G is the selected development machine. The historical one-L40S requirement is not measured or silently satisfied. Independent human reproduction, fixture-owner/integrator acceptance and substantive team responses remain pending.

## Explicit limits

This is a local, synthetic-genesis-funded development lifecycle, not a deployed oracle service. There is no claimed hosted/delegated integration, production secret custody, continuously operating oracle, private execution or cross-layer account settlement. The reference verifier/consumer remain test-grade. Other programs, upgrades, suites, request branches and runtime contexts are not qualified by this corpus. General SBPF conformance is not claimed.

Internal scripted/model review is useful validation, not a formal audit or external human acceptance. Research drafts propose a November direction and privacy boundary; they do not manufacture owner commitments or team agreement.

## Review entry points

- [Actual three-case proofs, signed receipts, runtime aggregates and failure history](../zkvm-vrf/evidence/g5-a10g-v11/README.md).
- [Runtime characterization](vrf-coordinator-runtime-v1.md), [compatibility/fixture contract](vrf-coordinator-development-v2.md), [proof profile](vrf-coordinator-proof-profile-v1.md).
- [Actual interpreter](../vrf-sbpf-replay/), [authenticated binding](../transaction-proof/src/vrf_candidate/binding.rs), [invocation-local validation reuse](vrf-candidate-binding-reuse-v1.md).
- [Guest/build recipe](../zkvm-vrf/), [SDK client and strict corpus observer](../zkvm-vrf-client/), [Portal key/proof tooling](../programs/portal/keys/bounded-vrf-v1.md).
- [Fresh lifecycle and receipt checks](vrf-candidate-portal-lifecycle-v1.md), [November proving decision draft](november-proving-direction-v1.md), [privacy assessment](privacy-market-assessment-october-v1.md).
