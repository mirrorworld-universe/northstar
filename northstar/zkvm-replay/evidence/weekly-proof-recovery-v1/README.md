# Colocated proof regression fixtures

This directory contains only inputs consumed by Portal's automated verifier and resolver tests. Reports, logs, timing samples, and bulk evidence are kept outside the repository.

Each case retains a proof and its public inputs. The three `live-*` cases and `manager` also retain the resolver account snapshots used to check binding, authority, and rejected-state invariants. `SHA256SUMS` identifies all retained binary inputs.

```sh
cargo build-sbf --manifest-path northstar/programs/portal/Cargo.toml --features zk-verifier-prototype
BPF_OUT_DIR=target/deploy cargo test --locked -p northstar-portal --features zk-verifier-prototype --test zk_verifier
```

The proofs use the frozen replay candidate and eight-field ABI. They do not enable the production verifier. See the [local GPU settlement harness](../../../docs/gpu-proof-to-settlement-v1.md) for reproduction.
