# Agave sync conflict report - 2026-09-23

This sync attempt compares upstream Agave `07f3bf47f5092d350f69e02649aa652c392d0e06`
to `a748ed1b0e6f96794d1a43637d75fec44fae50c4`.

The merge was attempted with `agave/master` and stopped on conflicts. No conflicts
were resolved automatically.

## Conflict files

- `Cargo.lock`
- `core/Cargo.toml`
- `core/src/validator.rs`
- `rpc-client-api/src/custom_error.rs`
- `runtime/src/bank.rs`

## Upstream commit summary

- Runtime / SVM: 21 commits
- Networking / Consensus: 10 commits
- Programs / Loader: 2 commits
- RPC / API: 1 commit
- Tooling / Dependencies: 4 commits

## Notable upstream changes

- `svm: check if nonce account is writable (#15400)`
- `Preparation - Block production TX dropping in program runtime (#13860)`
- `SIMD-0599: Remove inactive stakes from stake delegations (#15078)`
- `SIMD-0433: Loader V3 Set Program Data to ELF Length (#15402)`
- `runtime: ebpp relatch (#15284)`
- `runtime: Make deactivating stake actually deactivating (#15430)`
- `Bump version to 4.5.0-alpha.0 (#15427)`
- `perf(gossip): use fairer parking_lot RwLock (#15456)`
