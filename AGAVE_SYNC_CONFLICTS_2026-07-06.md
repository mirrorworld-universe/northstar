# Agave Sync Conflict Report

Date: 2026-07-06

Attempted upstream sync:
- Base synced Agave commit: `fed083ba3fb0ac5f0fb5ec93a9c98eff8cf32ee6`
- Target Agave branch: `anza-xyz/agave:master`
- Target Agave head: `2f955833b8ebed647ac08f2c40c03eb79a387288`
- New upstream commits since last sync: `373`

Direct merge attempt into `northstar/master` produced conflicts in:
- `.github/workflows-disabled/docs.yml`
- `CONTRIBUTING.md`
- `Cargo.lock`
- `accounts-db/src/accounts_cache.rs`
- `cli/Cargo.toml`
- `core/src/validator.rs`
- `runtime/src/bank.rs`
- `svm/src/transaction_processor.rs`
- `test-validator/src/lib.rs`

GitHub App push blocker:
- Pushing a branch that points directly at `agave/master` was rejected because the app token does not have `workflows` permission for changes under `.github/workflows/*`.
- Because of that restriction, this PR is a conflict-tracking handoff rather than a full upstream branch import.

Upstream change summary from the GitHub API compare:
- `83` commits matched Consensus/Alpenglow work.
- `68` commits matched Runtime/Accounts/SVM work.
- `34` commits matched Networking/RPC/XDP work.
- `108` commits matched CI/Deps/Tooling work.
- `80` commits fell into mixed or uncategorized changes.

Representative upstream commits:
- `2f955833b8eb` `runtime: enable strict_nonce_size_check for simulation (#13664)`
- `e4aaf1eaae95` `program-runtime: make the InvokeContext API require a sanitized message (#13618)`
- `9e9cf1025dc7` `introduces notar; finalize; fast-finalize certs (#13601)`
- `529f96cee14e` `SIMD-0326: Alpenglow: new consensus algorithm (#11814)`
- `1d0d6343bf24` `XDP by default (#12119)`

Next step for manual resolution:
- Re-run the upstream merge locally with a token that can push workflow changes, then resolve the files above without dropping Northstar-specific behavior.
