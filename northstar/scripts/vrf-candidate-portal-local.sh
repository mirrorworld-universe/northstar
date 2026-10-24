#!/usr/bin/env bash
set -euo pipefail
umask 077
[[ ${NORTHSTAR_SANDBOX:-} == 1 ]] || { echo 'Run inside the Northstar sandbox' >&2; exit 2; }
phase=${1:?usage: vrf-candidate-portal-local.sh init|prepare|resolve|inspect WORK [lifecycle options...]}
work=${2:?persistent private work directory required}
shift 2
[[ $phase == init || $phase == prepare || $phase == resolve || $phase == inspect ]]
: "${NORTHSTAR_LIVE_PORTAL_SBF:?explicit actual Portal SBF required}"
: "${NORTHSTAR_LIVE_VALIDATOR:?explicit built Northstar solana-test-validator required}"
: "${NORTHSTAR_TS_SDK_DIR:?read-only SDK with installed dependencies required}"
root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$work"
work=$(realpath "$work")
portal=$(realpath "$NORTHSTAR_LIVE_PORTAL_SBF")
validator=$(realpath "$NORTHSTAR_LIVE_VALIDATOR")
exec 9>"$work/lifecycle.lock"
flock -n 9 || { echo 'Another lifecycle phase owns this ledger' >&2; exit 2; }
rpc_port=${NORTHSTAR_LIVE_RPC_PORT:-18999}
faucet_port=${NORTHSTAR_LIVE_FAUCET_PORT:-19900}
[[ $rpc_port =~ ^[0-9]+$ && $faucet_port =~ ^[0-9]+$ ]]
rpc="http://127.0.0.1:$rpc_port"
if curl --max-time 2 -s -o /dev/null "$rpc"; then echo 'Refusing an occupied RPC endpoint' >&2; exit 2; fi
session="vrf-candidate-l1-$$"
attempt=$(date +%s%N)
log="$work/validator-$attempt.log"
status="$work/phase-$attempt.exit"
cleanup() {
    local code=$?
    if tmux has-session -t "$session" 2>/dev/null; then
        tmux send-keys -t "$session" C-c || true
        local deadline=$((SECONDS + 20))
        while tmux has-session -t "$session" 2>/dev/null && ((SECONDS < deadline)); do sleep .1; done
        if tmux has-session -t "$session" 2>/dev/null; then
            tmux kill-session -t "$session" 2>/dev/null || true
            printf 'forced tmux termination after SIGINT deadline; validator exit unknown\n' > "$work/validator-$attempt.shutdown"
        fi
    fi
    if [[ ! -f $work/validator-$attempt.exit && ! -f $work/validator-$attempt.shutdown ]]; then
        printf 'validator exit unknown; tmux session ended without status\n' > "$work/validator-$attempt.shutdown"
    fi
    printf '%s\n' "$code" > "$status"
}
trap cleanup EXIT
health() {
    curl --max-time 2 -sf -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' "$rpc" | jq -e '.result == "ok"' >/dev/null &&
    curl --max-time 2 -sf -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getSlot","params":[{"commitment":"confirmed"}]}' "$rpc" | jq -e '.result >= 2' >/dev/null
}
wait_for() {
    local deadline=$((SECONDS + $1)); shift
    until "$@"; do
        tmux has-session -t "$session" 2>/dev/null || { echo 'Validator exited; full private log retained' >&2; return 1; }
        ((SECONDS < deadline)) || { echo 'Readiness/durable snapshot deadline exceeded' >&2; return 1; }
        sleep .2
    done
}
mkdir -p "$work/public"
sha256sum "$validator" "$portal" > "$work/tool-identities-$attempt.private.txt"
printf -v command '%q ' env "LD_LIBRARY_PATH=${LD_LIBRARY_PATH:-}" NORTHSTAR_TEST_VALIDATOR_SNAPSHOT_INTERVAL_SLOTS=20 RUST_LOG=warn "$validator" --log --ledger "$work/ledger" --rpc-port "$rpc_port" --faucet-port "$faucet_port" --bind-address 127.0.0.1 --gossip-port 20000 --dynamic-port-range 20000-20050 --bpf-program 5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf "$portal"
printf '#!/usr/bin/env bash\ntrap ":" INT\n%s > %q 2>&1\ncode=$?\nprintf "%%s\\n" "$code" > %q\nexit "$code"\n' "$command" "$log" "$work/validator-$attempt.exit" > "$work/validator-$attempt.launch.sh"
printf -v launch '%q ' bash "$work/validator-$attempt.launch.sh"
tmux new-session -d -s "$session" "exec $launch"
wait_for 120 health
set +e
bun "$root/northstar/scripts/vrf-candidate-portal-lifecycle.mjs" "$phase" --work "$work" --rpc "$rpc" --portal-sbf "$portal" "$@" > "$work/phase-$attempt.log" 2>&1
code=$?
set -e
printf '%s\n' "$code" > "$work/phase-cli-$attempt.exit"
if ((code != 0)); then echo "Lifecycle $phase failed (exit $code); preserving durable failure state before shutdown" >&2; fi
# Never stop before a durable rooted bank includes every confirmed phase transaction.
fence=$(curl --max-time 2 -sf -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getSlot","params":[{"commitment":"confirmed"}]}' "$rpc" | jq -er .result)
snapshot_ready() {
    local archive slot
    for archive in "$work/ledger"/snapshot-*.tar.zst; do
        slot=${archive##*/snapshot-}; slot=${slot%%-*}
        if [[ $slot =~ ^[0-9]+$ ]] && ((slot >= fence)); then
            printf '{"schema":"northstar.vrf-candidate-portal-durable.v1","phase":"%s","phase_exit":%s,"confirmed_fence":%s,"snapshot_slot":%s}\n' "$phase" "$code" "$fence" "$slot" > "$work/public/durable-$attempt.json"
            return 0
        fi
    done
    return 1
}
wait_for 120 snapshot_ready
if ((code == 0)); then printf 'Completed %s; validator stopped on exit. Public receipts: %s/public\n' "$phase" "$work"; fi
exit "$code"
