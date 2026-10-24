#!/usr/bin/env bash
# Coordinator-run; one fresh job and one reviewed lock-owning launcher.
set -euo pipefail
umask 077
[[ $# -ge 5 ]] || { printf 'usage: %s GPU_PREFIX CLIENT EXPECTED_CLIENT_SHA EXISTING_OUTPUT_PARENT CLIENT_COMMAND [ARGS...]\n' "$0" >&2; exit 2; }
gpu=$(realpath -e "$1")
client=$(realpath -e "$2")
expected=$3
parent=$(realpath -e "$4")
shift 4
mode=$1
case "$mode" in key|execute|execute-corpus|groth16|verify) ;; *) exit 2 ;; esac
for arg in "$@"; do [[ $arg != --out && $arg != --out=* ]] || { printf 'job owns the fresh --out directory\n' >&2; exit 2; }; done
[[ $expected =~ ^[0-9a-f]{64}$ && $(sha256sum "$client" | cut -d' ' -f1) == "$expected" ]]
[[ $(sha256sum "$gpu/bin/with-optimized-sha-prover" | cut -d' ' -f1) == 5165545ea73a688ef97815a0f97007776db4521ed38633c7eff98c66d771d5ec ]]
[[ $(sha256sum "$gpu/nonce-server/bin/with-gpu-server" | cut -d' ' -f1) == 88f04e67ad792ac06f0f6aa2430ab4c87fb2529c1e155b8a92e8e99a1fd2d69f ]]
[[ $(sha256sum "$gpu/sha-optimized.env" | cut -d' ' -f1) == 152ff9f697cd5a08345559013d3cc51f890c28c45e4587d1d08a058be7d4e7b8 ]]
[[ $(sha256sum "$gpu/nonce-server/home/.sp1/bin/sp1-gpu-server" | cut -d' ' -f1) == 375fb14b5bf07d6b4b2d2a80608d2ecd718509ee6812986e4ed8335b0d61b7c5 ]]
job=$(mktemp -d "$parent/vrf-client-$mode-XXXXXXXX")
printf '%s\n' "$job"
exec >"$job/run.log" 2>&1
trap 'code=$?; printf "%s\n" "$code" >"$job/run.exit"; date -u -Iseconds >"$job/finished"' EXIT
date -u -Iseconds >"$job/started"
printf 'Development candidate client job; NOT Portal resolution or production acceptance.\n' >"$job/CLASSIFICATION.txt"
sha256sum "$client" "$gpu/bin/with-optimized-sha-prover" "$gpu/sha-optimized.env" \
  "$gpu/nonce-server/home/.sp1/bin/sp1-gpu-server" >"$job/inputs.sha256"
nvidia-smi --query-gpu=uuid,name,driver_version,memory.total --format=csv >"$job/gpu.csv"
backend=cpu
[[ $mode != groth16 ]] || backend=cuda
# The launcher already owns the device lock and its own cleanup scope.
timeout --signal=TERM --kill-after=60s 30m "$gpu/bin/with-optimized-sha-prover" \
  env SP1_PROVER="$backend" "$client" "$@" --out "$job/output"
[[ $(sha256sum "$client" | cut -d' ' -f1) == "$expected" ]]
