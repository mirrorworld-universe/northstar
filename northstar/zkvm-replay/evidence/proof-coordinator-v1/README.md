# Coordinator regression fixtures

Only test-consumed artifacts are retained here. Benchmark reports, worker logs, repeated stress proofs, and live-run summaries belong outside the repository.

- All four cases provide a 356-byte proof, its 256-byte public inputs, and a pre-resolution account snapshot for Portal verification and rejection-state tests.
- `proving/` additionally provides the checkpoint artifact and witness used by coordinator restart tests.
- `SHA256SUMS` identifies these immutable inputs.

Run from the repository root:

```sh
cargo test --locked -p northstar --features proof-coordinator proof_coordinator::tests
cargo build-sbf --manifest-path northstar/programs/portal/Cargo.toml --features zk-verifier-prototype
BPF_OUT_DIR=target/deploy cargo test --locked -p northstar-portal --features zk-verifier-prototype --test zk_verifier
```

See [coordinator configuration and live harness](../../../docs/proof-coordinator.md). These fixtures exercise the frozen supported replay profile, not arbitrary SVM execution or production enablement.
