# Agave 4.4 / SP1 6.8: relevant changes and messaging

Research scope: Northstar's previous Agave 4.3.0-alpha.2 base through upstream commit `07f3bf47f5092d350f69e02649aa652c392d0e06` (4.4.0-alpha.5), and SP1 6.1.0 through 6.8.0. Agave is a prerelease snapshot, not a stable-release or mainnet-activation announcement.

## Measured numbers suitable for qualified messaging

All proof measurements below are individual compatibility runs on one NVIDIA L40S, not throughput benchmarks, percentiles, or controlled before/after comparisons. [Retained evidence](../zkvm-replay/evidence/sp1-v6.8.0/README.md) includes the guest ELF, program key, measurements, and proof bytes.

| Result | Defensible interpretation |
|---|---|
| 42.795 s baseline / 40.728 s resolver Groth16 generation + wrapping | Approximately 41–43 seconds for these two reference witnesses with warm circuit caches; excludes client initialization and approximately 15 seconds of program setup. |
| 184.969 s cold generation + wrapping | Initial circuit download/initialization matters; do not advertise warm numbers as cold-start latency. |
| 97,113 CU standalone Portal verification | 32,887 CU, or 25.3%, below the project's 130,000-CU target. This is headroom against a target, not a measured speedup. |
| Full resolver tests within a 130,000-CU transaction budget | Includes authenticated challenge state transitions, not just standalone proof verification. Do not apply the standalone 25.3% headroom to the full resolver. |
| 356-byte on-chain proof envelope; 256-byte public inputs | Compact proof payload. Not the entire transaction: the standalone instruction is 613 bytes and tested signed transaction is 783 bytes. |
| 23,798,044 guest cycles / 883 ms execution | Execution of the baseline witness, not proof generation or arbitrary application latency. |

The baseline witness contains 208 VM trace rows and reports 218 executed SVM units. The resolver uses a different, retained checkpoint-bound witness. The measurements do not establish general transaction capacity or multi-transaction throughput.

**Suggested copy:** “Northstar's upgraded proof prototype generated reference replay proofs in roughly 41–43 seconds on a single L40S with warm caches. Its 356-byte proof envelope verified on Solana's SBF runtime in 97,113 compute units, with full resolver compatibility tests fitting a 130,000-CU transaction budget.”

Keep “prototype,” “reference,” and “warm caches.” Default production verification remains disabled, `production_acceptance` remains false, and these tests are not a live-validator deployment or end-to-end finality benchmark.

## Agave changes relevant to Northstar

| Area | Upstream change | Northstar relevance / limitation |
|---|---|---|
| Timing and compatibility | [SIMD-0525 clock update](https://github.com/anza-xyz/agave/pull/15236): default L1 slots 400→300 ms; hash age 120→90 seconds | Nominal slot duration is 25% shorter (33.3% more nominal slots per second), not measured TPS/finality. ER slot duration remains 50 ms; default processing/history windows become 45/90 seconds instead of 60/120. Clients must not assume the old expiry window. |
| Runtime parity | [Replay transaction views](https://github.com/anza-xyz/agave/pull/14356) replace full transaction deserialization and align replay with block-production representations | Northstar's Bank-private replay adapter now uses upstream transaction views. Reduces representation divergence; no Northstar speedup measured. |
| RPC consistency | [minContextSlot](https://github.com/anza-xyz/agave/pull/15091) for getTransaction/getSignatureStatuses; commitment selection for status queries | Lagging nodes can reject insufficient context rather than returning ambiguous absence. ER getTransaction preserves this check before its custom history lookup. |
| Subscription payloads | [dataSlice respected](https://github.com/anza-xyz/agave/pull/15241) by account/program subscriptions | Clients requesting slices receive the requested binary payload, including empty slices. Savings depend on requested slice and account size; no measured bandwidth percentage. |
| RPC scan cost | [Bounded getBlocks scan](https://github.com/anza-xyz/agave/pull/15131) | Native history queries stop beyond their requested range. ER history remains separate; do not attribute native Blockstore optimizations to ER history. |
| Account storage | [Conditional account loads](https://github.com/anza-xyz/agave/pull/14540), [bounded index reservation](https://github.com/anza-xyz/agave/pull/14924), [coalesced bucket growth](https://github.com/anza-xyz/agave/pull/15175) | Avoid unnecessary account loading/reservation and overlapping resize failures. No end-to-end memory percentage measured. |
| Program cache | [Prune stale tombstones/unloaded entries](https://github.com/anza-xyz/agave/pull/14392), [reject retracted LoaderV4 programs](https://github.com/anza-xyz/agave/pull/14838) | Preserve Northstar's isolated builtins and sparse-ER missing-program handling while adopting deployment-slot-aware cache APIs. |
| Native ledger | [Reuse RocksDB pinnable slices](https://github.com/anza-xyz/agave/pull/14489) | Reduces native Blockstore allocation/page-fault churn. Requires the Anza RocksDB fork rather than an arbitrary system library. |
| Scheduling | [Slot-specific scheduling](https://github.com/anza-xyz/agave/pull/14739), [cost-pacer reset on Bank replacement](https://github.com/anza-xyz/agave/pull/15056) | Native block-production accounting improvements; not a benchmark of the separate ER execution path. |
| Networking | [Bound packet-accumulator retention](https://github.com/anza-xyz/agave/pull/15123) | Bounded buffering for fragmented incoming streams. Northstar retains ordinary-TPU forwarding with TPU-forwards fallback. |
| Snapshots | [Incremental interval 100→200 slots](https://github.com/anza-xyz/agave/pull/14350) | Fewer snapshot triggers per slot during native catch-up. With the new 300 ms default, nominal cadence is 60 seconds versus 40 seconds under the old combined defaults; not “2× faster recovery.” |
| Conformance | [Refactored transaction harness](https://github.com/anza-xyz/agave/pull/15076), [new transaction runner](https://github.com/anza-xyz/agave/pull/15220) | Northstar extracts replay/trace/proof fixtures into its own crate, keeping upstream churn away from the proof harness. |

Additional upstream consensus, vote/reward, and feature-key work is present, but inclusion is not feature activation. In particular, do not announce Alpenglow finality or reward-policy changes for Northstar from this merge alone. Reverted upstream optimizations are not counted as delivered improvements.

## SP1 release changes relevant to this upgrade

| Versions / source | Relevant changes | Qualification |
|---|---|---|
| [6.2.0](https://github.com/succinctlabs/sp1/releases/tag/v6.2.0) | Memory-bounded worker backpressure; typed execution errors; EnvProver verification delegation; local/distributed resource handling | Useful correctness/resource-management foundations; distributed-service features are not automatically enabled in our local CUDA path. |
| [6.2.1](https://github.com/succinctlabs/sp1/releases/tag/v6.2.1)–[6.2.4](https://github.com/succinctlabs/sp1/releases/tag/v6.2.4) | GPU interpolation/evaluation work, smaller memory-event tracking, per-proof bookkeeping release, runner failure handling; toolchain and gas-estimation updates | Includes gRPC reuse/retries and hosted prover capabilities, but those network features were not exercised here. Guest compiler remains the explicitly recorded 1.93.0-dev build. |
| [6.3.0](https://github.com/succinctlabs/sp1/releases/tag/v6.3.0)–[6.3.1](https://github.com/succinctlabs/sp1/releases/tag/v6.3.1) | DAG-native GPU zerocheck, device-resident folding metadata, idempotent task handling and build-info reporting | Upstream improvements; no isolated Northstar attribution benchmark. |
| [6.4.0](https://github.com/succinctlabs/sp1/releases/tag/v6.4.0) | Poseidon2/compression/field-arithmetic GPU optimizations; lower-memory sumcheck look-ahead; atomic circuit installation; GPU alignment and verifier no_std fixes | Do not include the scan-kernel change reverted within the same release. Blackwell fixes are not L40S performance measurements. |
| [6.5.0](https://github.com/succinctlabs/sp1/releases/tag/v6.5.0)–[6.7.0](https://github.com/succinctlabs/sp1/releases/tag/v6.7.0) | Dalek Edwards arithmetic; bounded guest output; SDK authentication/status-reporting and release packaging updates | Network authentication features remain outside the validated local GPU path. |
| [6.8.0](https://github.com/succinctlabs/sp1/releases/tag/v6.8.0) | Optional cuPQC NTT backend; serializable proving keys; blocking LightProver integration; toolchain child-exit reporting | Our server enables `groth16-cuda`, not `nvidia-ntt`: do not attribute these results to cuPQC. |

The wrapper verifying key and recursion root stay byte-identical. The rebuilt Northstar guest key changes, so old guest proofs are deliberately rejected by the current prototype. Historical proof evidence remains untouched.

## Crypto dependencies and migration scope

Arkworks 0.5→0.6 moves the circuit code to `gr1cs`; ed25519-dalek 2.2→3.0, sha2 0.10→0.11, and rand_chacha 0.3→0.10 modernize direct dependencies. Small adapters preserve Circom Poseidon commitments and the ChaCha20 byte stream where upstream libraries still require older field/RNG traits. This is compatibility work, not evidence of stronger cryptographic security or faster proving.

Validation: full workspace clippy; 260 targeted Northstar/native tests; two Northstar SVM regressions; seven prototype SBF tests plus the explicit checkpoint-proof test; 13 GPU runner/worker tests; provisioning guards; SP1 host/guest compilation and fresh SDK-verified CUDA proofs. No new production throughput, latency, or finality claim is supported by these checks.
