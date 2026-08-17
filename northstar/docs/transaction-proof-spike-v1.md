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

`northstar/zk-prover/src/transaction.rs` builds rows with pinned `solana-sbpf 0.22.0` decoding, checks the same shared replay statement, exposes exactly eight public inputs, and generates a real arkworks BN254 Groth16 proof. Unsupported opcode/syscall paths fail before proof generation.

## Soundness boundary

Proved or independently checked in this bounded spike:

- supported one-signer legacy wire format and Ed25519 signature;
- bounded blockhash queue, fee, account ordering, success effects, and rollback fixture;
- fetched instruction bytes, register continuity, supported ALU/jump/load/store families, PC transitions, and constrained `sol_memcpy_` identity;
- typed Light/Circom-compatible BN254 Poseidon commitments and exact eight-input ABI.

Bound but not independently derived:

- ELF extraction: full ELF bytes and Agave-produced loaded-text hash are committed; guest checks fetched text words but does not parse ELF;
- trace generation and boundary memory images come from Agave; trace v1 lacks online per-access memory events;
- runtime/feature/syscall registry identities are committed fixed values for this fixture.

Unsupported and fail-closed: CPI, precompiles, other syscalls, builtins, deployment/upgrade, lookup tables, durable nonce, unknown transaction/version/event/opcode, and trailing witness bytes.

SP1 Groth16 output is not claimed compatible with Portal's current verifier ABI. Custom Groth16 is direct, not recursive. The 1K/10K/100K sweep and larger-CU projections remain future work.
