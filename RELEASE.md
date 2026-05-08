# Release Process

This document describes how NorthStar releases are cut and deployed.

## Branches

```
master ───────────────────────────────────►   active development
   │
   └──► release ────────────────────────────►  promoted for deployment
```

- **`master`** — the integration branch. All feature work, fixes, and
  refactors land here first via pull request. CI runs on every push and PR.
- **`release`** — promotion branch. Pushing to `release` triggers an automated
  build-and-deploy to the NorthStar devnet validator (see
  [`.github/workflows/deploy-release.yml`](.github/workflows/deploy-release.yml)).

## Versioning

NorthStar follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Release-worthy changes should be recorded in [`CHANGELOG.md`](CHANGELOG.md) in
the same pull request that introduces them. See the *Adding to this
Changelog* section at the bottom of that file for the conventions we follow.

## Cutting a Release

1. **Confirm `master` is green.** All required checks on the CI workflow must
   pass on the commit you intend to release.
2. **Update the changelog.** Make sure every noteworthy change since the
   previous release has a `CHANGELOG.md` entry.
3. **Promote to `release`.** Fast-forward `release` to the chosen `master`
   commit and push:
   ```bash
   git fetch origin
   git checkout release
   git merge --ff-only origin/master
   git push origin release
   ```
   Pushing to `release` triggers
   [`.github/workflows/deploy-release.yml`](.github/workflows/deploy-release.yml),
   which:
   - runs `cargo +nightly fmt --check` and `cargo clippy --all --tests -- -D warnings`,
   - builds the portal SBF program (`./cargo-build-sbf --manifest-path northstar/programs/portal/Cargo.toml`),
   - builds the validator binary (`cargo build --release --bin agave-validator`),
   - and restarts the `northstar-validator` systemd service on the devnet host.
4. **Tag the release.** Once the deployment completes successfully, tag the
   same commit with the new version and push the tag:
   ```bash
   git tag vX.Y.Z <commit-sha>
   git push origin vX.Y.Z
   ```
5. **Publish the GitHub release.** Create a new release on the
   [GitHub releases page](https://github.com/mirrorworld-universe/northstar/releases)
   pointing at the tag. Use the relevant section of `CHANGELOG.md` as the
   release notes.

## Hotfixes

For urgent fixes that must ship outside the normal flow:

1. Open a PR against `master` with the fix.
2. Once merged and CI is green, follow steps 3–5 above to promote `master`
   onto `release` and tag the resulting commit.

If `master` contains other unreleased changes that are not yet ready to ship,
land the fix on `master` first, then cherry-pick the fix commit onto
`release` and push:

```bash
git checkout release
git cherry-pick <fix-commit-sha>
git push origin release
```

## Rolling Back

If a release introduces a regression, roll `release` back to the previous
known-good commit and push:

```bash
git checkout release
git reset --hard <previous-good-sha>
git push --force-with-lease origin release
```

The deploy workflow will rebuild and redeploy the previous binary. Open a PR
against `master` for the actual fix once the cause is understood.
