#!/usr/bin/env bash
set -euo pipefail

mode=${1:-cadence}
[[ $mode == cadence || $mode == crash || $mode == no-effects ]] || { echo 'usage: live-cadence-recovery.sh [cadence|crash|no-effects]' >&2; exit 2; }
root=$(cd "$(dirname "$0")/../.." && pwd)
validator="$root/target/debug/solana-test-validator"
[[ -x $validator ]] || { echo 'Build solana-test-validator first.' >&2; exit 2; }
work=$(mktemp -d /tmp/northstar-cadence-recovery.XXXXXX)
validator_session="northstar-cadence-validator-$$"
test_session="northstar-cadence-test-$$"
url=http://127.0.0.1:18999
cleanup() {
    tmux kill-session -t "$test_session" 2>/dev/null || true
    tmux kill-session -t "$validator_session" 2>/dev/null || true
    echo "Evidence: $work"
}
trap cleanup EXIT
health() {
    curl --max-time 2 -sf -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' "$url" | grep -q '"ok"'
}
wait_for() {
    local deadline=$((SECONDS + $1))
    shift
    until "$@"; do
        (( SECONDS < deadline )) || { echo "Timed out waiting for: $*" >&2; return 1; }
        sleep .2
    done
}
for endpoint in "$url" http://127.0.0.1:8910; do
    if curl --max-time 2 -s -o /dev/null "$endpoint"; then
        echo "$endpoint is already occupied; refusing to reuse another service." >&2
        exit 2
    fi
done
start_validator() {
    local command
    printf -v command '%q ' env RUST_LOG=warn,northstar=info "$validator" \
        --ledger "$work/ledger" --rpc-port 18999 --bind-address 127.0.0.1
    tmux new-session -d -s "$validator_session" "exec $command >> '$work/validator-console.log' 2>&1"
    wait_for 120 health
}
start_validator
args=(env "NORTHSTAR_LIVE_RPC_URL=$url" "BPF_OUT_DIR=$root/target/deploy")
if [[ $mode == crash ]]; then
    args+=("NORTHSTAR_LIVE_RESTART_READY=$work/ready" "NORTHSTAR_LIVE_RESTART_RESUME=$work/resume")
fi
if [[ $mode == no-effects ]]; then
    args+=(NORTHSTAR_LIVE_NO_EFFECTS=1)
fi
printf -v test_command '%q ' "${args[@]}" cargo test -p northstar live_service_seals_and_settles_one_transaction -- --ignored --nocapture
tmux new-session -d -s "$test_session" "cd '$root'; $test_command > '$work/test.log' 2>&1; echo \$? > '$work/result'"
if [[ $mode == crash ]]; then
    wait_for 180 test -f "$work/ready"
    old_pid=$(tmux list-panes -t "$validator_session" -F '#{pane_pid}')
    [[ $(readlink -f "/proc/$old_pid/exe") == "$validator" ]] || { echo 'Refusing to signal unexpected process.' >&2; exit 1; }
    kill -KILL "$old_pid"
    wait_for 30 test ! -e "/proc/$old_pid"
    start_validator
    new_pid=$(tmux list-panes -t "$validator_session" -F '#{pane_pid}')
    [[ $new_pid != "$old_pid" ]]
    printf 'signal=SIGKILL old_pid=%s new_pid=%s same_ledger=true\n' "$old_pid" "$new_pid" | tee "$work/restart.txt"
    touch "$work/resume"
fi
wait_for 300 test -f "$work/result"
grep -E 'NORTHSTAR_TIMING|test result:|panicked|timed out:' "$work/test.log" | tail -12
read -r result < "$work/result"
exit "$result"
