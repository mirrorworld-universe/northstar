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

Success witness: 339,819 bytes, SHA-256 `4f1d5d2837fea47c51285c9f6fbaf8ee08efef44e238479374b45e7d09b620f1`.

## SP1 replay

Pinned SP1 version: `6.1.0`. Guest independently decodes exact wire bytes, verifies Ed25519, checks recent blockhash membership and fee debit, validates supported SBPF row/register/control flow, binds boundary memory and trace data, checks success/account effects, recomputes typed commitments, and commits exactly eight canonical BN254 scalars.

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

Raw results: `northstar/benchmarks/transaction-proof-spike-v1.json`. Measurements used an AWS `g6e.4xlarge`: 16-vCPU AMD EPYC 7R13, 128 GiB RAM, one NVIDIA L40S with 46,068 MiB VRAM, Ubuntu 26.04, driver 580.173.02, SP1 6.1.0. Each warm result reports three runs after one warmup.

| Phase | Median | Range | Output |
|---|---:|---:|---:|
| SP1 execute | 4.746 s | 4.740–4.749 s | 111,050,365 cycles |
| SP1 core setup | 12.404 s | 11.558–12.472 s | — |
| SP1 core prove + verify | 28.695 s | 28.631–29.252 s | 27,343,935-byte artifact |
| SP1 Groth16 setup | 12.703 s | 11.670–12.724 s | — |
| SP1 Groth16 prove + wrap + verify | 89.568 s | 86.917–94.821 s | 356-byte on-chain proof |
| Custom table + constraints | 34 ms | 34 ms | 208 rows / 2,712 constraints |
| Custom Groth16 setup | 31 ms | 31–32 ms | 565,456-byte proving key |
| Custom Groth16 prove | 29 ms | 29–30 ms | 128-byte proof |
| Custom Groth16 verify | 3 ms | 3 ms | 520-byte verifying key |

SP1 core peaked at 31,510 MiB VRAM and 19.3 GiB prover RSS. Warm SP1 Groth16 peaked at 31,498 MiB VRAM and 30.3 GiB prover RSS. The first cold Groth16 wrap took 244.088 seconds; it is excluded from the warm median. SP1 6.1's selected CUDA SDK keeps proving keys opaque, so their serialized sizes are not reported.

The existing July eight-input Portal verifier measured 108,915 CU. That is only an outer-verifier projection for a compatible 256-byte arkworks proof/key encoding. SP1's 356-byte Groth16 encoding is not compatible with the current Portal verifier.

## Soundness boundary

Proved or independently checked in this bounded spike:

- supported one-signer legacy wire format and Ed25519 signature;
- bounded blockhash queue, fee, account ordering, success effects, and rollback fixture;
- native replay of fetched instruction bytes, register continuity, supported ALU/jump/load/store families, PC transitions, and constrained `sol_memcpy_` identity;
- typed Light/Circom-compatible BN254 Poseidon commitments and exact eight-input ABI.

Bound but not independently derived:

- ELF extraction: full ELF bytes and Agave-produced loaded-text hash are committed; guest checks fetched text words but does not parse ELF;
- trace generation and boundary memory images come from Agave; trace v1 lacks online per-access memory events;
- runtime/feature/syscall registry identities are committed fixed values for this fixture;
- the custom Groth16 setup fixes the accepted fixture witness into the relation; general witness-variable transition constraints remain future work;
- SP1 proving/verifying key serialized sizes are unavailable through the selected CUDA SDK API.

Unsupported and fail-closed: CPI, precompiles, other syscalls, builtins, deployment/upgrade, lookup tables, durable nonce, unknown transaction/version/event/opcode, and trailing witness bytes.

SP1 Groth16 output is not compatible with Portal's current verifier ABI. Custom Groth16 is direct, fixture-specific, and not recursive. The 1K/10K/100K sweep and larger-CU projections remain future work; the custom track must first generalize its circuit beyond fixed fixture constants.
