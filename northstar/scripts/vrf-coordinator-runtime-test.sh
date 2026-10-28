#!/usr/bin/env bash
set -euo pipefail
[[ ${NORTHSTAR_SANDBOX:-} == 1 ]] || { echo 'Run inside the Northstar sandbox.' >&2; exit 1; }
root=$(cd "$(dirname "$0")/../.." && pwd)
artifact_dir=${1:?Usage: vrf-coordinator-runtime-test.sh ARTIFACT_DIR NEW_JOURNAL [CYCLES]}
journal=${2:?Usage: vrf-coordinator-runtime-test.sh ARTIFACT_DIR NEW_JOURNAL [CYCLES]}
cycles=${3:-1}
[[ $cycles =~ ^[0-9]+$ ]] && (( cycles >= 1 && cycles <= 100 )) || { echo 'CYCLES must be 1..100.' >&2; exit 1; }
[[ ! -e "$journal" ]] || { echo 'Journal already exists; choose a fresh evidence path.' >&2; exit 1; }
artifact_dir=$(cd "$artifact_dir" && pwd)
journal_dir=$(cd "$(dirname "$journal")" && pwd)
journal="$journal_dir/$(basename "$journal")"
export VRF_COORDINATOR_ARTIFACT_DIR="$artifact_dir"
export VRF_REFERENCE_SOURCE="$artifact_dir/source"
export VRF_RUNTIME_JOURNAL="$journal"
export VRF_RUNTIME_CYCLES="$cycles"
export CARGO_BUILD_JOBS=2
: "${CARGO_HEAVY_LOCK:?Set the shared advisory Cargo lock path}"
: "${NODE_PATH:?Set NODE_PATH to existing modules containing @noble/curves and @noble/hashes}"
command -v bun >/dev/null
features=()
if [[ -n ${VRF_ATOMIC_CAPTURE_DIR:-} ]]; then features=(--features vrf-candidate-capture); fi
cd "$root"
flock "$CARGO_HEAVY_LOCK" cargo test -p northstar "${features[@]}" --test vrf_coordinator_runtime \
  vrf_coordinator_runtime_request_verify_callback -- --ignored --exact --nocapture
