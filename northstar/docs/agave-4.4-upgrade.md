# Agave 4.4 upgrade

Merged upstream master `07f3bf47f5092d350f69e02649aa652c392d0e06` (4.4.0-alpha.5, Rust 1.98.1) without rewriting Northstar history. The preceding extraction moves proof fixtures, traces, and replay orchestration into `northstar-replay-harness`; only the Bank-private execution adapter remains in runtime behind `northstar-replay`.

Northstar adopts upstream transaction views, deployment-slot-aware program tombstones, RPC configuration, and instruction-error crate separation while preserving ER history, isolated builtins, fee policy, and forwarding behavior. Upstream's 300 ms L1 slot duration changes the wall-clock-preserving ER age limits from 1200/2400 to 900/1800.

## Dependency migration

- Arkworks 0.6, including the `gr1cs` API; ed25519-dalek 3; sha2 0.11; rand_chacha 0.10.
- `light-poseidon` 0.4 still requires Arkworks 0.5. A field/parameter adapter isolates it from the 0.6 circuit types and preserves Circom commitments. Boundary and gadget tests cover the conversion.
- Arkworks still consumes rand 0.8 traits. A small adapter exposes the current ChaCha20 generator through those traits; a reference-stream test guards deterministic bytes.
- Portal accepts solana-address 2.7 instead of pinning incompatible 2.6.1.
- SP1 SDK, guest, verifier, and GPU server move to 6.8.0. Removed obsolete transitive pins and the unused curve25519-dalek 4 patch after ed25519-dalek moved to curve25519-dalek 5.

The baseline witness and public inputs remain byte-identical. SP1's Groth16 wrapper key/root are unchanged, but the rebuilt guest has a new program key. [Fresh proof evidence](../zkvm-replay/evidence/sp1-v6.8.0/README.md) covers SDK verification, Portal SBF verification, resolver invariants, and rejection of previous-program proofs. Historical evidence is not rewritten; default production verification remains disabled.

## Build environment

Upstream now uses Anza's rust-rocksdb fork with bundled RocksDB 10.4.2. An arbitrary system RocksDB is not ABI-compatible. If a shell exports system-library overrides, build with:

```bash
env -u ROCKSDB_LIB_DIR -u ROCKSDB_INCLUDE_DIR cargo clippy --all --tests
```

Use an absolute `BPF_OUT_DIR` when running Portal integration tests. Build Portal separately for default and `zk-verifier-prototype` tests; they require different SBF artifacts.
