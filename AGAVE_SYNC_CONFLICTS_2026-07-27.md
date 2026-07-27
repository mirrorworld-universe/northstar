# Agave Sync Conflict Report

Date: 2026-07-27

Attempted upstream sync:
- Base synced Agave commit: `fed083ba3fb0ac5f0fb5ec93a9c98eff8cf32ee6`
- Target Agave branch: `anza-xyz/agave:master`
- Target Agave head: `29e1fa1a795ac482479e5f923af4630592cc4cde`
- New upstream commits since last sync: `647`
- Compare URL: https://github.com/anza-xyz/agave/compare/fed083ba3fb0ac5f0fb5ec93a9c98eff8cf32ee6...29e1fa1a795ac482479e5f923af4630592cc4cde
- Baseline source: `.agave-sync.json` is not present on `northstar/master`, so the last synced base was recovered from PR `#149`

Direct merge attempt into `northstar/master` produced conflicts in:
- `.github/workflows-disabled/docs.yml`
- `CONTRIBUTING.md`
- `Cargo.lock`
- `accounts-db/src/accounts_cache.rs`
- `clap-utils/src/input_validators.rs`
- `cli/Cargo.toml`
- `core/src/validator.rs`
- `runtime/src/bank.rs`
- `svm/src/transaction_processor.rs`
- `test-validator/src/lib.rs`

Why this is a report-only PR:
- The sync policy for this job does not auto-resolve merge conflicts.
- The current handoff is limited to documenting the exact upstream range and the files that need manual reconciliation.

Upstream change summary from the GitHub API compare:
- `60` commits matched Consensus/Alpenglow/Votor work.
- `112` commits matched Runtime/Accounts/SVM work.
- `67` commits matched Networking/RPC/XDP work.
- `39` commits matched Snapshots/Ledger/Storage work.
- `206` commits matched CI/Deps/Tooling work.
- `163` commits fell into mixed or uncategorized changes.

Representative upstream commits:
- `29e1fa1a795a` `chore: Remove publish = true from TOML files (#14127)`
- `9310132e7ff0` `fix(runtime): Avoid panicking if stake_rewards capacity turns out be larger (#13697)`
- `f918b34688d7` `bls-sigverifier: improves stats / metrics for banning (#13626)`
- `0193b569e4f7` `test(xdp): Avoid allocation in recv_matching_payload (#14023)`
- `13ec0b63993a` `feat(snapshots): add StableAbi for snapshot serde types (#13479)`

Next step for manual resolution:
- Re-run the upstream merge locally, resolve the files above without dropping Northstar-specific behavior, then update this PR with the resolved sync branch.
