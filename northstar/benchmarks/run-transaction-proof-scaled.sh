#!/usr/bin/env bash
set -uo pipefail

repo=$(git rev-parse --show-toplevel)
results=${1:?usage: run-transaction-proof-scaled.sh RESULTS_DIR}
runs=${RUNS:-3}
warmups=${WARMUPS:-1}
profiles=(1k 10k 100k)
mkdir -p "$results"

root_target="$repo/target/release"
sp1_dir="$repo/northstar/zkvm-replay"
sp1_bin="$sp1_dir/target/release/northstar-zkvm-replay-script"
fixture_dir="$results/fixtures"
mkdir -p "$fixture_dir"

{
  echo "revision=$(git -C "$repo" rev-parse HEAD)"
  echo "tree=$(git -C "$repo" status --porcelain=v1)"
  echo "date=$(date --iso-8601=seconds)"
  uname -a
  lscpu | grep -E 'Model name|Socket|Core|Thread|CPU\(s\)'
  free -b | head -2
  nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader
  rustc --version
  "$HOME/.sp1/bin/cargo-prove" --version
} > "$results/environment.txt" 2>&1

for spec in "1k 71" "10k 1071" "100k 11071"; do
  read -r profile iterations <<< "$spec"
  "$root_target/generate_fixture" "$fixture_dir/$profile.bin" "$iterations" \
    "$fixture_dir/$profile.trace-v1.bin"
done
sha256sum "$fixture_dir"/*.bin > "$results/fixture-sha256.txt"

monitor_run() {
  local prefix=$1
  shift
  nvidia-smi \
    --query-gpu=timestamp,memory.used,utilization.gpu,power.draw \
    --format=csv,noheader,nounits -l 1 > "$prefix.gpu.csv" &
  local gpu_monitor=$!
  (
    while true; do
      ps -eo pid=,rss=,comm= | awk -v now="$(date +%s)" \
        '$3 ~ /sp1-gpu-server|northstar-zkvm/ {print now "," $1 "," $2 "," $3}'
      sleep 1
    done
  ) > "$prefix.rss.csv" &
  local rss_monitor=$!
  /usr/bin/time -v "$@" > "$prefix.stdout.json" 2> "$prefix.stderr.txt"
  local status=$?
  kill "$gpu_monitor" "$rss_monitor" 2>/dev/null || true
  wait "$gpu_monitor" "$rss_monitor" 2>/dev/null || true
  echo "$status" > "$prefix.exit"
  return "$status"
}

wait_for_gpu_server() {
  for _ in $(seq 1 30); do
    if ! pgrep -f "$HOME/.sp1/bin/sp1-gpu-server" >/dev/null; then
      sleep 2
      return
    fi
    sleep 1
  done
  return 1
}

run_custom() {
  local profile=$1
  local phase_dir="$results/$profile/custom"
  mkdir -p "$phase_dir"
  for ((i = 1; i <= warmups; i++)); do
    "$root_target/benchmark_transaction" "$profile" > "$phase_dir/warmup-$i.json"
  done
  for ((i = 1; i <= runs; i++)); do
    /usr/bin/time -v "$root_target/benchmark_transaction" "$profile" \
      > "$phase_dir/run-$i.json" 2> "$phase_dir/run-$i.time.txt"
    echo "$?" > "$phase_dir/run-$i.exit"
  done
}

run_sp1() {
  local profile=$1
  local mode=$2
  local phase_dir="$results/$profile/sp1-$mode"
  local fixture="$fixture_dir/$profile.bin"
  mkdir -p "$phase_dir"
  for ((i = 1; i <= warmups; i++)); do
    local status=0
    SP1_PROVER=cuda "$sp1_bin" "$mode" "$fixture" \
      "$phase_dir/warmup-$i.json" "$profile" \
      > "$phase_dir/warmup-$i.stdout.txt" 2> "$phase_dir/warmup-$i.stderr.txt" || status=$?
    wait_for_gpu_server || true
    ((status == 0)) || return "$status"
  done
  for ((i = 1; i <= runs; i++)); do
    local status=0
    monitor_run "$phase_dir/run-$i" env SP1_PROVER=cuda \
      "$sp1_bin" "$mode" "$fixture" "$phase_dir/run-$i.json" "$profile" || status=$?
    wait_for_gpu_server || true
    ((status == 0)) || return "$status"
  done
}

for profile in "${profiles[@]}"; do
  run_custom "$profile"
  run_sp1 "$profile" execute
  run_sp1 "$profile" core
  run_sp1 "$profile" groth16
done
