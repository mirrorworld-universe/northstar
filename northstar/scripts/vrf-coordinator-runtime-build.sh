#!/usr/bin/env bash
set -euo pipefail
[[ ${NORTHSTAR_SANDBOX:-} == 1 ]] || { echo 'Run inside the Northstar sandbox.' >&2; exit 1; }
source_dir=${1:?Usage: vrf-coordinator-runtime-build.sh SOURCE ARTIFACT_DIR}
artifact_dir=${2:?Usage: vrf-coordinator-runtime-build.sh SOURCE ARTIFACT_DIR}
revision=d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd
[[ $(git -C "$source_dir" rev-parse HEAD) == "$revision" ]] || { echo 'Unexpected library revision.' >&2; exit 1; }
[[ -z $(git -C "$source_dir" status --porcelain) ]] || { echo 'Library source must be clean.' >&2; exit 1; }
compiler="${XDG_CACHE_HOME:-$HOME/.cache}/solana/v1.56/platform-tools/rust/bin/rustc"
[[ -x "$compiler" ]] || { echo 'Installed platform-tools v1.56 required; no installation attempted.' >&2; exit 1; }
command -v cargo-build-sbf >/dev/null
mkdir -p "$artifact_dir"
artifact_dir=$(cd "$artifact_dir" && pwd)
[[ ! -e "$artifact_dir/source" ]] || { echo 'Use a fresh artifact directory.' >&2; exit 1; }
mkdir "$artifact_dir/source"
git -C "$source_dir" archive "$revision" | tar -x -C "$artifact_dir/source"
printf '%s\n' "$revision" > "$artifact_dir/source-revision.txt"
export CARGO_BUILD_JOBS=2
export RUSTUP_TOOLCHAIN=1.98.1
export CARGO_TARGET_DIR="$artifact_dir/target"
export RUSTC="$compiler"
cd "$artifact_dir/source"
for program in vrf-coordinator reference-verify test-consumer; do
  printf '%s\n' "Building $program"
  cargo-build-sbf --skip-tools-install --no-rustup-override --tools-version v1.56 --arch v2 \
    --manifest-path "programs/$program/Cargo.toml" --sbf-out-dir "$artifact_dir/deploy" -- --locked
 done
sha256sum "$artifact_dir/deploy/vrf_coordinator.so" "$artifact_dir/deploy/reference_verify.so" \
  "$artifact_dir/deploy/test_consumer.so" > "$artifact_dir/program-sha256.txt"
