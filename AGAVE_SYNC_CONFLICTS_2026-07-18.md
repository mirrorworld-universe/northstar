# Agave Sync Conflict Report

Date: 2026-07-18

Attempted upstream sync:
- Base synced Agave commit: `fed083ba3fb0ac5f0fb5ec93a9c98eff8cf32ee6`
- Target Agave branch: `anza-xyz/agave:master`
- Target Agave head: `4b5bfe6f7230a0f1ee7380affc39c381b9f358b5`
- New upstream commits since last sync: `554`
- Baseline source: `.agave-sync.json` was not present on `northstar/master`, so the last synced base was recovered from PR `#135`

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

Upstream change summary from the Agave range:
- `72` commits matched Consensus/Alpenglow/Votor work.
- `89` commits matched Runtime/Accounts/SVM work.
- `56` commits matched Networking/RPC/XDP work.
- `33` commits matched Snapshots/Ledger/Storage work.
- `150` commits matched CI/Deps/Tooling work.
- `154` commits fell into mixed or uncategorized changes.

Representative upstream commits:
- `4b5bfe6f7230` `chore(deps): bump regex from 1.12.4 to 1.13.0 (#13915)`
- `90706554582d` `snapshots: produce multi frame snapshots (#13902)`
- `79d5bf2e4f4c` `accounts-db: Inlines index thread pool selection for flush (#13927)`
- `406ef5a245c6` `Update rpc/std to use new api of tpu-client-next (#13900)`
- `29792b1781ee` `ag migration: change genesis vote threshold to >= (#13873)`

Next step for manual resolution:
- Re-run the upstream merge locally, resolve the files above without dropping Northstar-specific behavior, then open or update the full sync PR from the resolved branch.
