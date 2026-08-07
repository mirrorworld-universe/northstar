# Live BN254 Groth16 verifier benchmark

This package measures `groth16-solana` against Northstar's current Agave runtime using an actual compiled SBF program. It replaces the arithmetic-only estimates in the earlier ZK research.

## Results

Run date: 2026-08-02. Agave: `4.2.0-alpha.0`. `groth16-solana`: pinned revision `43fee1a67e91c0df7bb4edad5ba87ae4602aa208`.

| Public inputs | Proof | Public inputs | Instruction | Minimal tx | Portal envelope tx | Raw VK | CU |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 8 | 256 B | 256 B | 513 B | 683 B | 914 B | 1,024 B | 108,915 |
| 12 | 256 B | 384 B | 641 B | 811 B | 1,042 B | 1,280 B | 126,141 |
| 16 | 256 B | 512 B | 769 B | 939 B | 1,170 B | 1,536 B | 143,584 |

`Instruction` includes one selector byte. `Portal envelope tx` uses one fee-payer/submitter plus seven distinct readonly state accounts, matching an eight-account Portal instruction. Both transaction shapes were serialized, and both were successfully simulated against the SBF verifier. Maximum legacy transaction size is 1,232 bytes.

All variants fit the default 200,000-CU instruction budget, so none requires a compute-budget instruction. Sixteen inputs leave 62 bytes in the Portal-envelope transaction. Adding accounts, another signer, or unrelated instructions can exhaust that margin.

## Account sizes

Verifying keys are embedded in executable `.rodata`; verification needs no VK or scratch account.

- Benchmark SBF ELF: 24,584 bytes.
- Upgradeable-loader Program account: 36 bytes.
- ProgramData account: 24,629 bytes, including 45-byte loader metadata.
- Raw VK contribution: `512 + 64 * public_inputs`, shown above.
- Portal `StepProofAccount`: 529 bytes when the existing account-backed upload path is used.
- Feature-enabled Portal SBF ELF: 302,728 bytes in this run.

The standalone verifier instruction can verify in one transaction. Existing challenge resolution is not yet end-to-end one transaction: `StepProofAccount` creation, proof writes, sealing, and resolution remain separate operations. This prototype intentionally does not alter that lifecycle.

## Failure modes

| Input | Runtime result | CU |
|---|---|---:|
| Payload one byte short | `InvalidInstructionData` | 64 |
| Public input equal to BN254 Fr modulus | `PublicInputGreaterThanFieldSize` / custom 9 | 155 |
| Mutated 16-input proof | `ProofVerificationFailed` / custom 1 | 143,585 |
| Valid Portal proof capped at 100,000 CU | `ComputationalBudgetExceeded` | 99,850 |

Host and Portal tests also cover mutated public inputs, noncanonical fields, and the generated one-account transition vector. Invalid proofs fail without state changes because the prototype verifier instruction is stateless.

## Method

`solana-program-test` loads `target/deploy/northstar_zk_verifier_bench.so`, executes the SBF ELF, and reports runtime-consumed CU. This exercises Agave's BN254 syscalls and SBF byte parsing; it is not a native Rust verifier timing or a CU formula. CU is deterministic and independent of the developer laptop's proving capability.

Fixtures use deterministic test-only Groth16 setup seeds. They must never secure production funds.

Reproduce from repository root:

```bash
./cargo run --release -p northstar-zk-prover --bin generate-verifier-bench-vectors
rustup run 1.96.0 cargo build-sbf \
  --manifest-path northstar/zk-verifier-bench/Cargo.toml
SBF_OUT_DIR="$PWD/target/deploy" ./cargo test \
  -p northstar-zk-verifier-bench --test live_sbf -- --nocapture
```

Machine-readable output: [`results/2026-08-02.json`](results/2026-08-02.json).
