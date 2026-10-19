#!/usr/bin/env bash
# Offline native checks only. This script never invokes a prover or an RPC endpoint.
set -euo pipefail

if [[ $# != 1 ]]; then
  echo 'usage: bash northstar/scripts/vrf-proof-cpu-conformance.sh NEW_OUTPUT_DIRECTORY' >&2
  exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
output=$1
[[ $output = /* ]] || output="$PWD/$output"
mkdir -m 700 -- "$output"
cd -- "$repo"
export CARGO_BUILD_JOBS=2
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$repo/target/vrf-proof-cpu"}
[[ $CARGO_TARGET_DIR = /* ]] || CARGO_TARGET_DIR="$repo/$CARGO_TARGET_DIR"

if ! cargo build --locked -p northstar-transaction-proof --no-default-features \
    --bin vrf-proof-readiness >"$output/build.log" 2>&1; then
  echo "Build failed; full log: $output/build.log" >&2
  exit 2
fi
binary="$CARGO_TARGET_DIR/debug/vrf-proof-readiness"
{
  printf 'schema=northstar-vrf-proof-cpu-run-v1\n'
  printf 'source_revision=%s\n' "$(git rev-parse HEAD)"
  printf 'execution=native_offline\nproof_generated=false\nsp1_execute=not_run\n'
  rustc --version
  cargo --version
} >"$output/run.txt"
git status --porcelain -- northstar/transaction-proof \
  northstar/scripts/vrf-proof-cpu-conformance.sh >"$output/SOURCE_STATUS"
sha256sum -- northstar/transaction-proof/src/lib.rs \
  northstar/transaction-proof/src/readiness.rs \
  northstar/transaction-proof/src/bin/vrf-proof-readiness.rs \
  northstar/transaction-proof/tests/readiness.rs \
  northstar/scripts/vrf-proof-cpu-conformance.sh >"$output/SOURCE_SHA256SUMS"

for case in hardened-runner-v1 combined-settlement-v1 \
    proof-performance-v1/fast-live-1 proof-performance-v1/fast-live-2 \
    proof-performance-v1/fast-live-3; do
  evidence="northstar/zkvm-replay/evidence/$case"
  name=${case//\//-}
  witness="$evidence/witness-v2.bin"
  public="$evidence/northstar-sp1-public-inputs.bin"
  "$binary" "$witness" "$public" >"$output/$name.txt"
  status=0
  "$binary" "$witness" "$public" --require-vrf \
    >"$output/$name.require-vrf.txt" || status=$?
  if [[ $status != 3 ]]; then
    echo "Expected missing VRF support (exit 3), got $status for $case" >&2
    exit 2
  fi
  cmp -- "$output/$name.txt" "$output/$name.require-vrf.txt"
  sha256sum -- "$witness" "$public" >>"$output/INPUT_SHA256SUMS"
  printf '%s: native conformance passed; VRF support remains missing\n' "$case"
done
printf 'Reports: %s\n' "$output"
