# Bounded full-transaction proof spike v1

## Shared fixture

The success fixture is a deterministic signed legacy transaction executed through `execute_txn_with_trace`. It has one signer, one top-level compiled SBF instruction, one writable data account, no CPI, a 5,000-lamport fee debit, and an eight-byte account whose first byte changes from `0` to `100`.

The tracked `write-to-account` compiler output invokes exactly one `sol_memcpy_` syscall while serializing/deserializing program data. The checker constrains that registry key and rejects every other syscall. Claiming zero syscalls would be false for this ELF.

A paired `write-then-fail` ELF mutates account data and returns `InvalidArgument`. The traced Bank result proves/checks fee commit plus account rollback.

| Metric | Success | Rollback |
|---|---:|---:|
| Trace bytes | 42,413 | 51,200 |
| VM rows | 208 | 345 |
| Executed units | 218 | 355 |
| Fee | 5,000 | 5,000 |

Success witness: 339,927 bytes, SHA-256 `831bd44a7577659d2e87849d44dce4f3c46e8e301e58c2eb06effd738ac41b24`.

## SP1 replay

Pinned SP1 version: `6.1.0`. Guest independently decodes canonical legacy wire bytes, uses strict Ed25519 verification, checks recent blockhash membership and fee debit, validates supported SBPF rows/registers/call-stack control flow, binds boundary memory and trace data, checks success/account effects, recomputes typed commitments, and commits exactly eight canonical BN254 scalars.

Reproducible commands:

```bash
curl -L https://sp1up.succinct.xyz | bash
~/.sp1/bin/sp1up --version 6.1.0
cd northstar/zkvm-replay
PATH="$HOME/.sp1/bin:$PATH" cargo run --release -p northstar-zkvm-replay-script -- execute
PATH="$HOME/.sp1/bin:$PATH" SP1_PROVER=cpu cargo run --release -p northstar-zkvm-replay-script -- core
PATH="$HOME/.sp1/bin:$PATH" SP1_PROVER=cpu cargo run --release -p northstar-zkvm-replay-script -- groth16
# CUDA 12, Nvidia compute capability >= 8.0, and >= 24 GB VRAM:
PATH="$HOME/.sp1/bin:$PATH" SP1_PROVER=cuda cargo run --release -p northstar-zkvm-replay-script -- all
```

Heavy proofs are explicit commands, not default workspace tests.

## Custom execution table

`northstar/zk-prover/src/transaction.rs` builds rows with pinned `solana-sbpf 0.22.0` decoding, checks the same shared replay statement, exposes exactly eight public inputs, and generates a real arkworks BN254 Groth16 proof. Unsupported opcode/syscall paths fail before proof generation. This first relation is fixture-specific: native replay validates transitions before synthesis, while setup fixes the accepted witness values into the relation. It is not yet a reusable algebraic SBPF transition circuit.

## Measured baseline

Raw results: `northstar/benchmarks/transaction-proof-spike-v1.json`. Measurements used one AWS `g6e.4xlarge`: 16-vCPU AMD EPYC 7R13, 128 GiB RAM, one NVIDIA L40S with 46,068 MiB VRAM, Ubuntu 26.04, driver 580.173.02, SP1 6.1.0. CUDA results report the median and range of three runs after one warmup. CPU proof results are one run because they took 21.5–32.7 minutes and approached host memory capacity.

| Phase | CUDA median (range) | CPU | GPU speedup |
|---|---:|---:|---:|
| SP1 execute | 4.678 s (4.664–4.680) | 4.621 s (4.621–4.662) | 0.99× |
| SP1 core setup | 11.871 s (11.844–11.901) | 2.937 s | 0.25× |
| SP1 core prove + verify | 28.394 s (28.347–28.653) | 1,259.360 s | 44.35× |
| SP1 Groth16 setup | 12.042 s (11.781–12.660) | 2.961 s | 0.25× |
| SP1 Groth16 prove + wrap + verify | 90.856 s (90.186–93.965) | 1,931.956 s | 21.26× |

The guest executes 109,365,172 cycles, down 1.52% after hashing the ELF once and reusing its digest. Core produces a 25,871,485-byte saved artifact. Groth16 produces a 356-byte on-chain proof and 1,950-byte saved artifact. End-to-end process wall time, including initialization, is 31.40× faster on GPU for core and 18.94× faster for Groth16.

CUDA core peaked at 31,087 MiB VRAM and 19.3 GiB prover RSS; CUDA Groth16 peaked at 31,510 MiB VRAM and 30.3 GiB prover RSS. CPU core peaked at 107.6 GiB RSS and CPU Groth16 at 97.2 GiB RSS. SP1 6.1's selected CUDA SDK keeps proving keys opaque, so their serialized sizes are not reported.

| Custom fixture-specific phase | Median | Range | Output |
|---|---:|---:|---:|
| Table + constraints | 34 ms | 34 ms | 208 rows / 2,712 constraints |
| Groth16 setup | 31 ms | 31 ms | 565,456-byte proving key |
| Groth16 prove | 30 ms | 29–31 ms | 128-byte proof |
| Groth16 verify | 3 ms | 3 ms | 520-byte verifying key |

The existing July eight-input Portal verifier measured 108,915 CU. That is only an outer-verifier projection for a compatible 256-byte arkworks proof/key encoding. SP1's 356-byte Groth16 encoding is not compatible with the current Portal verifier.

## Soundness boundary

Proved or independently checked in this bounded spike:

- supported one-signer canonical legacy wire format and strict Ed25519 signature;
- bounded blockhash queue, fee, account ordering, success effects, and rollback fixture;
- native replay of fetched instruction bytes, register continuity, supported ALU/jump/load/store families, entry/final rows, internal call/return stack transitions, and constrained `sol_memcpy_` identity;
- typed Light/Circom-compatible BN254 Poseidon commitments and exact eight-input ABI.

Bound but not independently derived:

- ELF extraction: guest commits the full ELF digest and Agave-produced loaded-text hash, then checks fetched text words, but does not parse ELF;
- trace generation and boundary memory images come from Agave; trace v1 lacks online per-access memory events;
- runtime/feature/syscall registry identities are committed fixed values for this fixture;
- internal function hash-to-PC targets are host-derived witness data constrained against committed VM control-flow rows rather than independently extracted from ELF;
- the custom Groth16 setup fixes the accepted fixture witness into the relation; general witness-variable transition constraints remain future work;
- SP1 proving/verifying key serialized sizes are unavailable through the selected CUDA SDK API.

Unsupported and fail-closed: CPI, precompiles, other syscalls, builtins, deployment/upgrade, lookup tables, durable nonce, unknown transaction/version/event/opcode, and trailing witness bytes.

SP1 Groth16 output is not compatible with Portal's current verifier ABI. Custom Groth16 is direct, fixture-specific, and not recursive. The 1K/10K/100K sweep and larger-CU projections remain future work; the custom track must first generalize its circuit beyond fixed fixture constants.
