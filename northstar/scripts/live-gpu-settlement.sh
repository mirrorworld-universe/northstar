#!/usr/bin/env bash
set -euo pipefail
umask 077
mode=${1:-settle}
[[ $mode == settle || $mode == crash-settling || $mode == crash-upload ]] || { echo 'usage: live-gpu-settlement.sh [settle|crash-settling|crash-upload]' >&2; exit 2; }
: "${NORTHSTAR_LIVE_PROVER:?preflight-capable GPU adapter required}"
: "${NORTHSTAR_LIVE_PORTAL_SBF:?explicit prototype Portal SBF required}"
root=$(cd "$(dirname "$0")/../.." && pwd)
target=$(realpath "${CARGO_TARGET_DIR:-$root/target}")
portal=$(realpath "$NORTHSTAR_LIVE_PORTAL_SBF")
validator="$target/debug/solana-test-validator"
[[ -x $validator && -f $portal ]]
cd "$root"
work=$(mktemp -d "${NORTHSTAR_EVIDENCE_DIR:-/tmp}/northstar-gpu-settlement.XXXXXXXX")
validator_session="gpu-settlement-validator-$$"
test_session="gpu-settlement-test-$$"
cleanup() {
    tmux kill-session -t "$test_session" 2>/dev/null || true
    tmux kill-session -t "$validator_session" 2>/dev/null || true
    echo "Evidence: $work"
}
trap cleanup EXIT
url=http://127.0.0.1:18999
rpc_slot() {
    curl --max-time 2 -sf -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' "$url" | jq -e '.result >= 1' >/dev/null
}
wait_for() {
    local deadline=$((SECONDS + $1))
    shift
    until "$@"; do
        [[ ! -f $work/result ]] || { echo "Test ended; see $work/test.log" >&2; return 1; }
        (( SECONDS < deadline )) || { echo "Timed out: $*" >&2; return 1; }
        sleep .2
    done
}
for endpoint in "$url" http://127.0.0.1:8910; do
    if curl --max-time 2 -s -o /dev/null "$endpoint"; then
        echo "Refusing occupied endpoint: $endpoint" >&2
        exit 2
    fi
done
export CARGO_TARGET_DIR="$target"
NORTHSTAR_LIVE_GENESIS_DIR="$work/genesis" cargo test --locked -p northstar export_live_settlement_genesis -- --ignored > "$work/genesis.log" 2>&1
accounts=()
for file in "$work/genesis"/*.json; do
    key=${file##*/}; key=${key%.json}
    accounts+=(--account "$key" "$file")
done
validator_env=()
start_validator() {
    local command
    printf -v command '%q ' env RUST_LOG=warn "${validator_env[@]}" "$validator" \
        --ledger "$work/ledger" --rpc-port 18999 --faucet-port 19900 --bind-address 127.0.0.1 \
        --portal GikCSCpYUq7QR7esoK6GM4UbJzKgdKNvS5bR1rBYH5E4 \
        --bpf-program 5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf "$portal" "${accounts[@]}"
    printf 'exec %s >> %q 2>&1\n' "$command" "$work/validator.log" > "$work/validator-launch.sh"
    tmux new-session -d -s "$validator_session" "exec bash '$work/validator-launch.sh'"
    wait_for 120 rpc_slot
}
start_validator
export NORTHSTAR_LIVE_RPC_URL="$url" NORTHSTAR_LIVE_SETTLE=1 NORTHSTAR_LIVE_PROOF_DIR="$work/proof"
unset NORTHSTAR_LIVE_SETTLEMENT_RESTART_READY NORTHSTAR_LIVE_SETTLEMENT_RESTART_RESUME NORTHSTAR_LIVE_UPLOAD_RESTART_READY NORTHSTAR_LIVE_UPLOAD_RESTART_RESUME
if [[ $mode == crash-settling ]]; then
    export NORTHSTAR_LIVE_SETTLEMENT_RESTART_READY="$work/ready" NORTHSTAR_LIVE_SETTLEMENT_RESTART_RESUME="$work/resume"
elif [[ $mode == crash-upload ]]; then
    export NORTHSTAR_LIVE_UPLOAD_RESTART_READY="$work/ready" NORTHSTAR_LIVE_UPLOAD_RESTART_RESUME="$work/resume"
fi
test_env=()
for name in CARGO_TARGET_DIR PATH HOME RUSTC RUSTDOC LD_LIBRARY_PATH ICICLE_BACKEND_INSTALL_DIR NORTHSTAR_LIVE_PROVER NORTHSTAR_LIVE_RPC_URL NORTHSTAR_LIVE_SETTLE NORTHSTAR_LIVE_PROOF_DIR NORTHSTAR_LIVE_PAYER NORTHSTAR_LIVE_STEP_COUNT NORTHSTAR_LIVE_SELECTED_STEP NORTHSTAR_LIVE_SETTLEMENT_RESTART_READY NORTHSTAR_LIVE_SETTLEMENT_RESTART_RESUME NORTHSTAR_LIVE_UPLOAD_RESTART_READY NORTHSTAR_LIVE_UPLOAD_RESTART_RESUME NORTHSTAR_GPU_SSH NORTHSTAR_GPU_REMOTE_RUNNER NORTHSTAR_GPU_REMOTE_ARTIFACTS NORTHSTAR_GPU_PROVER NORTHSTAR_GPU_REPLAY_DIR; do
    if [[ -v $name ]]; then test_env+=("$name=${!name}"); fi
done
printf -v command '%q ' env "${test_env[@]}" cargo test --locked -p northstar real_checkpoint_bisects_to_captured_transaction -- --ignored --nocapture
printf 'cd %q; %s > %q 2>&1; echo $? > %q\n' "$root" "$command" "$work/test.log" "$work/result" > "$work/test-launch.sh"
tmux new-session -d -s "$test_session" "exec bash '$work/test-launch.sh'"
if [[ $mode == crash* ]]; then
    wait_for 600 test -f "$work/ready"
    read -r fence < "$work/ready" || [[ -n $fence ]]
    [[ $fence =~ ^[0-9]+$ ]]
    snapshot_ready() {
        local archive slot
        for archive in "$work/ledger"/snapshot-*.tar.zst; do
            slot=${archive##*/snapshot-}; slot=${slot%%-*}
            if [[ $slot =~ ^[0-9]+$ ]] && (( slot >= fence )); then
                printf 'finalized_fence=%s snapshot_slot=%s\n' "$fence" "$slot" > "$work/snapshot-fence.txt"
                return 0
            fi
        done
        return 1
    }
    wait_for 120 snapshot_ready
    old_pid=$(tmux list-panes -t "$validator_session" -F '#{pane_pid}')
    [[ $(readlink -f "/proc/$old_pid/exe") == "$validator" ]]
    kill -KILL "$old_pid"
    wait_for 30 test ! -e "/proc/$old_pid"
    validator_env+=(NORTHSTAR_TEST_VALIDATOR_LOAD_ONLY_SNAPSHOTS=1)
    start_validator
    new_pid=$(tmux list-panes -t "$validator_session" -F '#{pane_pid}')
    [[ $new_pid != "$old_pid" ]]
    printf 'signal=SIGKILL old_pid=%s new_pid=%s same_ledger=true\n' "$old_pid" "$new_pid" > "$work/restart.txt"
    touch "$work/resume"
fi
wait_for 1000 test -f "$work/result"
grep -E 'proof_resolution|proof_to_settlement|upload_recovery|settlement_recovery|test result:|panicked' "$work/test.log" | tail -8
read -r result < "$work/result"
exit "$result"
