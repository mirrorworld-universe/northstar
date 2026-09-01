# Full-SVM proving architecture decision v1

Status: **hybrid selected**.

This closes the August proof-architecture objectives. Northstar should keep a zkVM as the semantic orchestrator and add bounded coprocessor boundaries for the costs that measurements show are dominant. It should not fund a standalone custom full-SBPF circuit from the current spike.

## Decision summary

- Keep SP1 replay as the reference path for transaction, runtime, trace, result, and eight-field public-input semantics.
- Prioritize a BN254 Poseidon/hash coprocessor boundary. At 100K rows, Poseidon commitments and trace hashing consume 93.55% of zkVM cycles; signature verification consumes 0.006%.
- Add a versioned memory-event extension before claiming general SBPF memory soundness. Trace v1 remains frozen and continues to use invocation-boundary images.
- Keep the custom execution table as a scaling lower bound and coprocessor test bed. Do not treat its fixture-fixed Groth16 relation as a production SBPF circuit.
- Require the gates below before expanding semantic coverage or committing to production full-SVM proving.

## O1 closure: frozen contract

Proof kind `2`, version `1`, is frozen with this public ABI order:

1. `domain`
2. `session_context`
3. `slot_step`
4. `pre_state_root`
5. `post_state_root`
6. `tx_effect_root`
7. `readonly_l1_root`
8. `settlement_effect_root`

Trace schema v1, commitment rules, supported/unsupported matrix, canonical corpus, and fixture hashes remain defined by `transaction-processor-proof-v1.md` and `trace-fixtures/manifest-v1.json`. Scaled fixtures add rows only. They do not change proof kind, proof version, ABI order, trace schema, or fail-closed behavior.

Scaled canonical traces are stored in `trace-fixtures/scaled-v1/`; `manifest.json` binds their revision, row count, trace hash, compressed artifact hash, witness hash, and public ABI.

## O2 closure: shared restricted fixture

Both spikes use the same signed legacy transaction fixture, account ordering, runtime commitment, VM rows, result, and public inputs.

- SP1 independently checks the bounded replay and emits the eight public inputs.
- The custom track builds the same execution table and emits a real BN254 Groth16 proof.
- Mutation tests reject changed wire bytes, signatures, ELF/program hash, instructions, registers, memory boundaries, accounts, fees, compute, outcome, unsupported opcodes/syscalls, extra rows, and trailing witness bytes.

Neither implementation changes as a result of the scaled sweep, apart from benchmark instrumentation and dynamic scaled-fixture input.

## Benchmark protocol

Classification used below:

- **Measured:** observed on the benchmark host.
- **Host-bound:** observed behavior whose current implementation is not a production proof claim.
- **Projected:** regression or compatibility estimate, not an observed transaction.

All measured runs use:

- benchmark source revision `2d26a8b13c753bd2e507304c77e8278b77c65285`;
- Northstar runtime revision `1fddab640a68ee7a43be264470a89c5302581cfe`;
- proof kind 2/version 1, trace schema v1, and the frozen eight-field ABI;
- AWS `g6e.4xlarge`: AMD EPYC 7R13, 16 vCPU, 128 GiB RAM, NVIDIA L40S 46,068 MiB, driver 580.173.02;
- Ubuntu 26.04, kernel 7.0.0-1010-aws, Rust 1.97.1, SP1 6.1.0;
- local proving only; no network prover;
- one warmup followed by three successful measured runs per profile and phase;
- medians below; ranges and raw fields are in `benchmarks/transaction-proof-scaled-v1.json`.

The fixtures contain 999, 9,999, and 99,999 VM rows and execute 1,009, 10,009, and 100,009 CU. Their transaction, signer count, account count, syscall count, runtime, and public ABI are fixed. Only bounded loop rows change.

## Measured results

### Common tracing and witness generation

| Rows | Trace | Witness | Native replay | Trace bytes | Witness bytes |
|---:|---:|---:|---:|---:|---:|
| 999 | 6 ms | <1 ms | 8 ms | 119,543 | 422,911 |
| 9,999 | 8 ms | 2 ms | 10 ms | 1,055,543 | 1,394,911 |
| 99,999 | 33 ms | 21 ms | 27 ms | 10,415,543 | 11,114,911 |

Tracing and witness generation are not bottlenecks on this fixture.

### SP1 zkVM replay

| Rows | Execute | Cycles | Core prove | Core verify | Groth16 prove + wrap | Groth16 verify |
|---:|---:|---:|---:|---:|---:|---:|
| 999 | 5.888 s | 121,748,232 | 23.243 s | 1.162 s | 92.957 s | 330 ms |
| 9,999 | 12.085 s | 264,048,896 | 43.054 s | 2.388 s | 124.505 s | 331 ms |
| 99,999 | 70.292 s | 1,685,232,522 | 258.412 s | 14.761 s | 457.219 s | 331 ms |

SP1 setup is 11.8–12.4 s across profiles. `prove + wrap` is one measured SDK phase; SP1 6.1 does not expose an isolated wrapping timer through this API. Subtracting separate core-prove runs would be a host-bound differential, not a direct wrapping measurement.

| Rows | Core VRAM | Core host RSS | Core artifact | Groth16 VRAM | Groth16 host RSS | On-chain proof |
|---:|---:|---:|---:|---:|---:|---:|
| 999 | 31,087 MiB | 19.1 GiB | 28,936,465 B | 31,057 MiB | 30.4 GiB | 356 B |
| 9,999 | 31,119 MiB | 18.4 GiB | 59,754,465 B | 31,574 MiB | 30.4 GiB | 356 B |
| 99,999 | 31,087 MiB | 22.0 GiB | 369,944,683 B | 31,542 MiB | 30.5 GiB | 356 B |

SP1 proving-key sizes remain unavailable through the selected CUDA SDK API. The 356-byte proof is not compatible with Portal's current arkworks verifier encoding.

### Custom execution table

These are **host-bound lower bounds**, not production-circuit results. Native replay validates the witness, while setup fixes accepted witness values into the relation.

| Rows | Constraints | Constraint generation | Setup | Prove | Verify | Peak RSS |
|---:|---:|---:|---:|---:|---:|---:|
| 999 | 12,995 | 25 ms | 92 ms | 81 ms | 3 ms | 56 MiB |
| 9,999 | 129,995 | 207 ms | 777 ms | 684 ms | 3 ms | 427 MiB |
| 99,999 | 1,299,995 | 2.064 s | 7.985 s | 7.087 s | 3 ms | 4.10 GiB |

The relation uses about 13 constraints per VM row. Proving keys grow from 2,603,952 to 24,993,968 to 275,108,528 bytes. Verifying key and proof sizes stay 520 and 128 bytes.

## Cost attribution

SP1 cycle trackers at 99,999 rows report:

| Cost center | Cycles | Share | Classification |
|---|---:|---:|---|
| BN254 Poseidon commitments | 817,509,072 | 48.510% | Measured |
| SHA trace hash | 759,027,422 | 45.040% | Measured |
| Witness decoding | 69,989,908 | 4.153% | Measured |
| SBPF rows, boundary memory, syscall checks | 36,434,388 | 2.162% | Measured |
| Program ELF hash | 692,465 | 0.041% | Measured |
| Ed25519 signature | 95,418 | 0.006% | Measured |
| Wire, accounts, fees, blockhash, header | 8,185 | <0.001% | Measured |

Tracked regions cover 99.91% of total cycles.

Interpretation:

- Hashing and Poseidon, not SBPF execution, dominate this proof implementation.
- A signature coprocessor has negligible value on the one-signature fixture.
- Memory, accounts, CPI, and syscall-heavy workloads are underrepresented. Their future cost must be measured on dedicated fixtures rather than inferred from this loop.
- The custom track's speed reflects fixture-fixed constants and a narrow row relation. It does not show that a general custom SVM circuit would prove in seven seconds.

## CU projections

The loop has `executed CU = VM rows + 10`. Ordinary least-squares fits over all three measured profiles produce these **projected** medians:

| Transaction | SP1 execute | SP1 core prove | SP1 Groth16 prove + wrap | Custom prove, host-bound |
|---:|---:|---:|---:|---:|
| 50K CU | 37.9 s | 139.2 s | 272.8 s | 3.5 s |
| 200K CU | 135.2 s | 496.6 s | 825.8 s | 14.2 s |
| 1.4M CU | 914.0 s | 3,355.6 s | 5,249.5 s | 99.3 s |

These are ALU/branch-loop projections. Signature-heavy, hash-heavy, CPI, multi-account, loader, and syscall-heavy transactions may differ materially. The 1.4M-CU point extrapolates 14x beyond the largest measured fixture.

The existing eight-input Solana Groth16 verifier baseline is 108,915 CU. Treat that only as a projection for an encoding-compatible outer proof. The current SP1 356-byte output is incompatible; the direct custom proof is fixture-specific and cannot be deployed.

## Remaining semantic gaps

- Trace v1 has invocation-boundary memory images and deltas, not online per-load/store events.
- The zkVM binds the ELF and fetched words but does not independently parse ELF/loader state.
- Internal function hash-to-PC targets remain host-derived witness data constrained against control-flow rows.
- CPI, precompiles, most syscalls, builtins, deployment/upgrade, lookup tables, durable nonce, native programs, and broader transaction formats remain fail-closed.
- Account-heavy and syscall-heavy cost attribution needs dedicated fixtures.
- The custom relation must replace fixture constants with witness-variable instruction, register, memory, account, and outcome constraints before it can make a soundness claim.
- A compatible SP1-to-Portal outer wrapper and Solana verification measurement do not yet exist.

## Delivery estimate

Assumes two proving engineers initially, adding one runtime/circuit engineer after the first gate.

### 3 months: 6 engineer-months

- Build and measure a bounded Poseidon/hash coprocessor boundary without changing the eight public inputs.
- Add a versioned memory-event trace extension and mutation tests.
- Produce an encoding-compatible outer proof and measure it in Portal.
- Extend the fixture set with one account-heavy and one syscall/CPI transaction.

### 6 months: 15–18 cumulative engineer-months

- Cover common CPI, precompile, syscall, versioned-transaction, and loader paths.
- Independently bind ELF/function metadata and witness-variable memory transitions.
- Add parallel/sharded proving and prove a representative 200K-CU transaction end to end.
- Integrate proof delivery with the dispute-step lifecycle on devnet.

### 12 months: 35–45 cumulative engineer-months

- Reach an explicitly versioned production coverage set across native programs, loaders, CPI, precompiles, accounts, and rollback.
- Automate Agave upgrade differential testing and proving-key/version rotation.
- Operate bounded multi-GPU proving for the 1.4M-CU class with monitored latency and cost.

## Go/no-go gates

Proceed past the three-month hybrid de-risker only if all gates pass:

1. **Hash value:** the coprocessor cuts 100K-row zkVM cycles by at least 60% and Groth16 prove+wrap latency by at least 2x while preserving all eight public inputs.
2. **Memory soundness:** per-access mutation tests fail closed; trace/witness size and proving latency overhead stay below 25% on the 100K fixture.
3. **Latency:** a 50K-CU representative transaction produces an outer proof in at most 150 s on one L40S after warmup.
4. **Capacity:** peak VRAM stays below 40 GiB and host RSS below 96 GiB.
5. **On-chain delivery:** the encoding-compatible verifier stays at or below 130K CU and proof bytes fit the Portal instruction/account path.
6. **Correctness:** zero differential mismatches across the supported corpus; every unsupported branch emits no proof.

Proceed from six to twelve months only if:

- a representative 200K-CU transaction proves and wraps in at most five minutes on the declared production prover shape;
- common CPI/syscall/native paths meet the published coverage matrix;
- a proof reaches devnet settlement before the dispute-step delivery deadline;
- projected 1.4M-CU delivery is at most 20 minutes on a bounded four-L40S configuration.

Stop the full-SVM track if the hash de-risker misses a 2x latency improvement, memory events cannot stay within the 25% overhead bound, Portal verification exceeds 150K CU, or semantic coverage cannot advance without changing proof kind 2/version 1.

## Stretch de-risker selected

After O3 closure, the first bounded stretch item should be the Poseidon/hash coprocessor boundary. Signature acceleration is not selected because measured Ed25519 cost is negligible; memory-event work remains mandatory for soundness but is not the dominant performance de-risker.
