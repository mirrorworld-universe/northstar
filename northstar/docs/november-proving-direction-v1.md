# November proving direction v1

**Development snapshot: 2026-10-24. Draft for human review; staffing and production activation are not approved.**

## Recommendation

Continue the **bounded SBPF VRF profile** as November's first de-risker. Retain SP1 as the execution prover and the existing narrow replay route unchanged. Prioritize conformance, rejection coverage, reproducibility and measured proving cost over a second execution architecture or general SVM support.

The separately authorized October prototype now executes the pinned coordinator/reference-verifier/test-consumer composition inside local Northstar ER and reexecutes it in an authenticated SP1 guest. This supersedes the October 23 assumption that only a coverage proposal existed. It does **not** establish hosted/delegated integration, cross-layer settlement, independent reproduction, production approval or completion of every acceptance gate.

This research recommendation authorizes no further implementation, key rotation, deployment, paid proving or staffing. Three substantive human responses and a confirmed accountable owner remain missing.

## What the measurements establish

| Evidence | Observed result | Boundary |
| --- | --- | --- |
| Existing restricted replay on A10G | Historical merged corpus: 13 warm proofs, 40.996–42.732 s, median 41.997 s; process-cold 93.329 s | Not VRF execution; no transfer of its latency to the new composition |
| Local ER lifecycle | A fresh configured-session window completed 100 cycles, 304 successful signed transactions, 200 atomic captures, zero lost or duplicate callbacks; separate rollback controls retained | Local ER characterization, not hosted/delegated or settlement acceptance |
| Bounded candidate baseline | One actual authenticated request guest: 122,290,302 cycles; callback control: 128,356,831 cycles; exact 256-byte public output | CPU execution controls, not proof timings |
| Actual candidate Groth16 on A10G | Initial preview proof: 309.924 s; same-input characterization: 312.347 / 262.307 / 262.271 s; SDK verification passed | Every sample misses 120 s; repeated preview input is not three fresh lifecycle cases |
| Selected integrated candidate | Three real request/callback envelopes: 72,766,349 / 78,599,972 / 82,819,098 cycles, exact native public output; separate real-guest SHA known answers passed | CPU-cycle improvement alone does not establish proving performance |
| Selected-guest calibration | Actual SDK-verified A10G development proof: 282.566 s prove+wrap, 0.498 s SDK verification | Prior-session calibration, outside final three-case lifecycle window |
| Three fresh local Production resolutions | Actual proofs: 245.493 / 204.670 / 210.837 s; whole instruction CU: 111,253 / 109,753 / 111,253 | All miss 120 s; a retained first-batch CUDA failure preceded the successful recovery window |
| Configured candidate Portal SBF | Actual selected proof accepted; 84,262 whole `VerifyErStepProofV1` instruction CU; 17 mutation checks plus historical candidate-key/legacy guards pass | Not whole `ResolveChallenge(Production)` CU and not three fresh resolutions |
| Strict guest-control corpus | Baseline, SHA-only and selected guests each passed all 17 actual cases; selected guest additionally accepts two valid failed-transaction rollback witnesses with exact public output | Earlier per-case timeout/partial results remain retained; infrastructure errors never count as refusals |

The selected development machine is **g5.xlarge/A10G**, not L40S. No L40S-equivalent claim, throughput extrapolation, normalized hardware comparison or cold-cache-only explanation is justified. Record client initialization, setup, execution, wrapping, SDK verification and full Portal resolution separately. Preserve timeout and failed attempts rather than counting them as successful guest rejections.

Current source and boundary references: [guest and build][guest], [runtime handoff][runtime], [profile/fixtures][fixtures], [Portal development key][key], and [fresh lifecycle tooling][lifecycle]. Those documents and final artifact identities must agree before accepting a new key or evidence window. The baseline and experimental guest identities are not interchangeable.

## Options

### A. Continue the bounded interpreter profile — recommended

Reexecute the actual frozen SBPF programs rather than accepting host traces or asserted effects. Keep signed-wire admission, complete account presence/absence, loader/ProgramData/ELF identity, runtime/sysvars, nested CPI/PDA privileges, producer-bound return data, account create/close and rollback semantics inside the relation.

October established a development kind-3/version-1 route using the unchanged eight-field/256-byte public order and unchanged 149-byte session-context encoding. The new route separately binds its execution context and checkpoint cardinality. That is a bounded implementation result, not permission to admit arbitrary programs or relax old-route rules. Every different guest/key requires its own reviewed deployment identity and actual configured-session evidence.

**Next value:** independently reproduce and review the completed local corpus, investigate the retained CUDA backend fault, and attribute the remaining proof cost before another optimization. The selected guest has actual positive/rejection/rollback coverage and three local Production resolutions, but latency, first-attempt reliability and human acceptance remain open. Optimize only measured work while preserving acceptance and public bytes; never replace execution with asserted effects or looser admission.

**Unsupported:** arbitrary consumers, program upgrades, other VRF suites, unlisted branches/syscalls, Store/expiry/admin paths, address lookup tables, durable nonces, nonzero cross-layer settlement and inputs outside the reviewed profile. General SBPF or confidential execution is not claimed.

### B. A program-specific transition relation

Prove the same frozen composition's transitions directly rather than interpret SBPF. It could reduce represented execution work, but no implementation, performance advantage or equivalence result exists. It duplicates application semantics and increases maintenance/review work on upgrades. RFC VRF verification alone would still omit authorization, CPI, rent/fees, state effects and rollback.

Choose B only through a separate evidence-based architecture decision if A fails its bounded feasibility gate. It needs a separate guest/key/profile and independent semantic review; sharing eight public fields does not make the relations interchangeable.

### C. Retain narrow replay and improve operations only

Lowest semantic risk: improve cold start, recovery and reproducibility without new guest admission. The historical A10G corpus supports this operational lane, but it adds **no VRF execution coverage**. Keep it as a fallback and regression baseline, not as a replacement for the VRF acceptance corpus.

**Decision proposed for November:** continue A's bounded validation rather than build B or expand C alone. The October development choice does not approve November capacity or production use.

## One bounded November follow-up

**Deliverable:** a reviewed development profile and independently reproducible request/callback/rollback corpus, with honest proof/resolution measurements and a continue/narrow/stop decision. No automatic rollout.

**Accountable owner:** TBD; a named proving lead must explicitly accept. Runtime/consumer and independent-review owners must confirm availability. Proposed initial capacity remains two proving engineers plus 0.5 runtime/evidence engineer for two weeks (**5 engineer-weeks**); this is a planning request, not assigned or measured effort.

1. **By November 6:** confirm owner/capacity and three human responses; review exact source/profile/guest/key/fixture/runtime identities and unsupported cases. Resolve remaining evidence and provenance gaps before reusing a candidate key.
2. **By November 13:** require zero approved native/guest mismatches, all selected-guest rejection controls observed, preserved old-route regressions, and actual cost attribution. Publish wrapped proof timing, memory, initialization and full resolution CU separately. Review effort and choose continue, narrow or stop.
3. **By November 27, only if separately approved:** obtain independent reproduction and human integration acceptance of the observed local lifecycles, or publish a blocked decision and revised bounded estimate. No general SVM expansion follows by default.

Acceptance remains separate by dimension:

- **Runtime:** 100 consecutive actual ER request-to-callback cycles with complete captures and no lost/duplicate callbacks; human integration acceptance still required. Local and hosted/delegated results stay distinct.
- **Proof/lifecycle:** three fresh real `ResolveChallenge(Production)` cases jointly covering request, verification and callback; exact source/key/session association and complete successful resolution CU <=130,000. This is now observed locally, not independently human-accepted or hosted/delegated qualification. Standalone verification is not resolution.
- **Performance/hardware:** report the selected A10G development target honestly against 120 s. The historical one-L40S gate is neither measured nor silently satisfied by A10G; any hardware/acceptance amendment needs explicit stakeholder agreement.
- **Correctness/review:** actual nonzero guest exit plus empty public output for every approved rejection control; infrastructure errors do not count. Preserve failures, old-route regressions, independent reproduction and semantic review. Metadata alone is not a certificate.

## Human review record

All three slots remain **pending**. Agent analysis, internal scripted review and silence are not substantive team feedback.

| Reviewer role | Required response | Status |
| --- | --- | --- |
| Proving/semantic reviewer | A/B/defer, largest remaining invariant, cited fixture/source and a blocking validation case | Pending |
| Runtime/consumer integrator | Accept/reject the bounded lifecycle, missing account/CPI/rollback inputs and available capacity | Pending |
| Independent reproduction/performance reviewer | Reproduce a named run or identify the missing run; assess hardware, timing and whole-resolution boundaries | Pending |

Record reviewer/date, evidence, choice/rationale, requested change, owner/capacity confirmation and disposition. The accountable owner must resolve disagreements and confirm one bounded follow-up. Do not claim an agreed November direction or create a remote tracking item without permission.

[guest]: ../zkvm-vrf/README.md
[runtime]: vrf-coordinator-runtime-v1.md
[fixtures]: vrf-coordinator-development-v2.md
[key]: ../programs/portal/keys/bounded-vrf-v1.md
[lifecycle]: vrf-candidate-portal-lifecycle-v1.md

Actual selected-guest three-case proof/receipt/failure artifacts: [A10G development evidence](../zkvm-vrf/evidence/g5-a10g-v11/README.md).
