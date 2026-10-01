#!/usr/bin/env bash
set -euo pipefail

mode=${1:-cadence}
[[ $mode == cadence || $mode == crash || $mode == no-effects || $mode == crash-no-effects || $mode == crash-challenged || $mode == crash-settled ]] || { echo 'usage: live-cadence-recovery.sh [cadence|crash|no-effects|crash-no-effects|crash-challenged|crash-settled]' >&2; exit 2; }
root=$(cd "$(dirname "$0")/../.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
target=$(realpath "$target")
cd "$root"
validator="$target/debug/solana-test-validator"
[[ -x $validator ]] || { echo 'Build solana-test-validator first.' >&2; exit 2; }
work=$(mktemp -d "${NORTHSTAR_EVIDENCE_DIR:-/tmp}/northstar-cadence-recovery.XXXXXX")
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
        -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' "$url" | grep -q '"ok"' || return 1
    # Genesis programs deployed at slot zero are not invocable in that same slot.
    curl --max-time 2 -sf -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"getSlot","params":[{"commitment":"confirmed"}]}' "$url" | jq -e '.result >= 1' >/dev/null
}
wait_for() {
    local deadline=$((SECONDS + $1))
    shift
    until "$@"; do
        if [[ -f $work/result ]]; then
            echo "Test finished before readiness; see $work/test.log" >&2
            return 1
        fi
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
    printf -v command '%q ' env RUST_LOG=warn,northstar=info "${validator_env[@]}" "$validator" \
        --ledger "$work/ledger" --rpc-port 18999 --bind-address 127.0.0.1
    tmux new-session -d -s "$validator_session" "exec $command >> '$work/validator-console.log' 2>&1"
    wait_for 120 health
}
CARGO_TARGET_DIR="$target" cargo test --locked -p northstar --no-run > "$work/build.log" 2>&1 || {
    echo "Test build failed; see $work/build.log" >&2
    exit 1
}
validator_env=()
start_validator
args=(env "NORTHSTAR_LIVE_RPC_URL=$url" "BPF_OUT_DIR=$target/deploy" "CARGO_TARGET_DIR=$target")
if [[ $mode == crash* ]]; then
    args+=("NORTHSTAR_LIVE_RESTART_READY=$work/ready" "NORTHSTAR_LIVE_RESTART_RESUME=$work/resume")
fi
if [[ $mode == no-effects || $mode == crash-no-effects ]]; then
    args+=(NORTHSTAR_LIVE_NO_EFFECTS=1)
fi
if [[ $mode == crash-challenged ]]; then
    args+=(NORTHSTAR_LIVE_CRASH_CHALLENGED=1)
fi
if [[ $mode == crash-settled ]]; then
    args+=(NORTHSTAR_LIVE_CRASH_SETTLED=1)
fi
printf -v test_command '%q ' "${args[@]}" cargo test -p northstar live_service_seals_and_settles_one_transaction -- --ignored --nocapture
tmux new-session -d -s "$test_session" "cd '$root'; $test_command > '$work/test.log' 2>&1; echo \$? > '$work/result'"
if [[ $mode == crash* ]]; then
    wait_for 180 test -f "$work/ready"
    if [[ $mode == crash-settled ]]; then
        fence=$(curl --max-time 2 -sf -H 'Content-Type: application/json' \
            -d '{"jsonrpc":"2.0","id":1,"method":"getSlot","params":[{"commitment":"finalized"}]}' "$url" | jq -er '.result')
        snapshot_ready() {
            local archive slot
            for archive in "$work/ledger"/snapshot-*.tar.zst; do
                slot=${archive##*/snapshot-}
                slot=${slot%%-*}
                if [[ $slot =~ ^[0-9]+$ ]] && (( slot >= fence )); then
                    printf 'finalized_fence=%s snapshot_slot=%s\n' "$fence" "$slot" > "$work/snapshot-fence.txt"
                    return 0
                fi
            done
            return 1
        }
        wait_for 120 snapshot_ready
        validator_env+=(NORTHSTAR_TEST_VALIDATOR_LOAD_ONLY_SNAPSHOTS=1)
    fi
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
wait_for 1000 test -f "$work/result"
grep -E 'NORTHSTAR_TIMING|test result:|panicked|timed out:' "$work/test.log" | tail -12
read -r result < "$work/result"
exit "$result"
