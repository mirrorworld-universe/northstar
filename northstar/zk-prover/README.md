# Northstar one-account transition circuit

This crate contains the first `northstar-er-step-v1` Groth16 relation. It is a feasibility prototype, not a full SVM transition proof.

## Relation

Eight public BN254 scalar values bind:

1. protocol domain, proof kind, and proof version;
2. session context;
3. ER slot and step index;
4. pre-state root;
5. post-state root;
6. transaction/effect descriptor root;
7. readonly L1 root;
8. settlement-effect root.

Private witness contains one account's identity, owner, old/new lamports, old/new data root, and one shared 32-level Merkle path. Constraints enforce:

- account identity and owner do not change;
- old and new account leaves open at the same path under the public state roots;
- settlement-effect root commits the exact old/new account values;
- transaction/effect root binds domain, session, position, readonly root, and effect root;
- protocol domain selects one-account proof kind 1, version 1.

Poseidon uses Light Protocol's Circom-compatible BN254 x5 parameters, matching Solana's `sol_poseidon` parameters. This prototype proves internally consistent account effects. It does **not** prove SBF/SVM execution, transaction signatures, authorization, CPI behavior, or current SHA-256 checkpoint membership.

## Test vector

`test-vectors/one-account-transition-v1.json` contains deterministic witness, public inputs, raw Solana proof, and verifying key. Seeds are fixed for reproducibility, so this setup has known toxic waste and must never secure production funds.

Regenerate from repository root:

```bash
./cargo run --release -p northstar-zk-prover --bin generate-test-vector
```

## Laptop benchmark

Run date: 2026-08-02. Host: developer laptop, AMD Ryzen 7 7840U, 16 logical CPUs, 60 GiB RAM, AC power. This is not a production prover node; timings establish local feasibility only.

- Constraints: **21,972**
- Setup: **317.3 ms**
- Warm proof median, 10 runs: **343.5 ms**
- Warm proof range: **321.0-365.6 ms**
- Compressed proving key: **4,460,720 bytes**
- Compressed arkworks verifying key: **520 bytes**
- Solana raw proof: **256 bytes**
- Solana raw verifying-key layout: **1,024 bytes**

Setup is measured separately from proving. No GPU acceleration was used. Production capacity and latency require rerunning the same command on intended prover hardware under controlled load.
