#!/usr/bin/env bash
set -euo pipefail
umask 077
mode=${1:?usage: live-proof-coordinator.sh smoke|proving|upload}
[[ $mode == smoke || $mode == proving || $mode == upload ]]
: "${NORTHSTAR_LIVE_PORTAL_SBF:?prototype Portal SBF required}"
: "${NORTHSTAR_LIVE_OWNER_SBF:?replay-owner SBF required}"
root=$(cd "$(dirname "$0")/../.." && pwd)
target=$(realpath "${CARGO_TARGET_DIR:-$root/target}")
validator="$target/debug/solana-test-validator"
work=$(mktemp -d "${NORTHSTAR_EVIDENCE_DIR:-/tmp}/northstar-coordinator-$mode.XXXXXXXX")
mkdir "$work/public"
validator_session="coordinator-validator-$$"
driver_session="coordinator-driver-$$"
worker_session="coordinator-worker-$$"
cleanup() {
    tmux kill-session -t "$driver_session" 2>/dev/null || true
    tmux kill-session -t "$validator_session" 2>/dev/null || true
    tmux kill-session -t "$worker_session" 2>/dev/null || true
    echo "Evidence: $work/public"
}
trap cleanup EXIT
export NORTHSTAR_LIVE_RPC_URL=http://127.0.0.1:18999
export NORTHSTAR_LIVE_ER_RPC_URL=http://127.0.0.1:8910
export NORTHSTAR_COORDINATOR_EVIDENCE="$work/public"
export NORTHSTAR_PROOF_JOB_DIR="$work/jobs"
export NORTHSTAR_PROOF_CHALLENGER_KEYPAIR="$work/challenger.json"
export NORTHSTAR_CHECKPOINT_PLAN_DIR="$work/ledger/northstar-checkpoint-plans"
export CARGO_TARGET_DIR="$target"
export NORTHSTAR_GPU_REPLAY_DIR="$root/northstar/zkvm-replay"
export NORTHSTAR_GPU_WORKER_SOCKET="$work/worker.sock"
if [[ ${NORTHSTAR_COORDINATOR_CONTENTION:-0} == 1 ]]; then
    export NORTHSTAR_COORDINATOR_ER_GATE="$work/contention-resume"
fi
solana-keygen new --silent --no-bip39-passphrase --outfile "$NORTHSTAR_PROOF_CHALLENGER_KEYPAIR" >/dev/null
for url in "$NORTHSTAR_LIVE_RPC_URL" "$NORTHSTAR_LIVE_ER_RPC_URL"; do
    if curl --max-time 2 -s -o /dev/null "$url"; then echo "Refusing occupied endpoint: $url" >&2; exit 2; fi
done
wait_for() {
    local deadline=$((SECONDS+$1)); shift
    until "$@"; do
        ((SECONDS < deadline)) || { echo "Timed out: $*" >&2; return 1; }
        if [[ $1 == health ]] && ! tmux has-session -t "$validator_session" 2>/dev/null; then echo 'Validator exited before readiness' >&2; return 1; fi
        if [[ -f $work/driver.exit ]] && [[ $(<"$work/driver.exit") != 0 ]]; then echo 'Driver failed' >&2; return 1; fi
        sleep .2
    done
}
health() {
    curl --max-time 2 -sf -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' "$NORTHSTAR_LIVE_RPC_URL" | jq -e '.result == "ok"' >/dev/null &&
    curl --max-time 2 -sf -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getSlot","params":[{"commitment":"confirmed"}]}' "$NORTHSTAR_LIVE_RPC_URL" | jq -e '.result >= 2' >/dev/null
}
cd "$root"
cargo test --locked -p northstar --features proof-coordinator live_coordinator_ --no-run > "$work/build.log" 2>&1
export NORTHSTAR_PROOF_PROVER="$work/prover"
if [[ $mode == smoke ]]; then
    printf '#!/bin/sh\nexit 0\n' > "$work/prover"
    export NORTHSTAR_COORDINATOR_SMOKE=1
else
    : "${NORTHSTAR_GPU_SERVER_WRAPPER:?with-gpu-server executable required}"
    : "${NORTHSTAR_GPU_PROVER:?built SP1 worker executable required}"
    printf -v command '%q ' "$NORTHSTAR_GPU_SERVER_WRAPPER" env "NORTHSTAR_GPU_PROVER=$NORTHSTAR_GPU_PROVER" "NORTHSTAR_GPU_REPLAY_DIR=$NORTHSTAR_GPU_REPLAY_DIR" python3 "$root/northstar/scripts/gpu-worker.py" serve "$NORTHSTAR_GPU_WORKER_SOCKET" "$work/worker-state"
    tmux new-session -d -s "$worker_session" "exec $command > '$work/worker.log' 2>&1"
    wait_for 240 test -S "$NORTHSTAR_GPU_WORKER_SOCKET"
    {
        printf '#!/usr/bin/env bash\nset -euo pipefail\n'
        if [[ $mode == proving ]]; then
            printf 'if [[ $1 == groth16 ]]; then touch %q; until [[ -f %q ]]; do sleep .1; done; fi\n' "$work/prover-entered" "$work/release-proof"
        fi
        printf 'exec %q "$@"\n' "$root/northstar/scripts/gpu-worker.py"
    } > "$work/prover"
fi
chmod 700 "$work/prover"
start_validator() {
    local extra=()
    if [[ ${1:-initial} == restart ]]; then extra+=(NORTHSTAR_TEST_VALIDATOR_LOAD_ONLY_SNAPSHOTS=1);
    elif [[ $mode == upload ]]; then extra+=(NORTHSTAR_PROOF_TEST_UPLOAD_FENCE="$work/upload-fence"); fi
    printf -v command '%q ' env "NORTHSTAR_PROOF_CHALLENGE_WINDOW_SLOTS=${NORTHSTAR_PROOF_CHALLENGE_WINDOW_SLOTS:-750}" "NORTHSTAR_PROOF_JOB_DIR=$NORTHSTAR_PROOF_JOB_DIR" "NORTHSTAR_PROOF_PROVER=$NORTHSTAR_PROOF_PROVER" "NORTHSTAR_PROOF_CHALLENGER_KEYPAIR=$NORTHSTAR_PROOF_CHALLENGER_KEYPAIR" "NORTHSTAR_GPU_WORKER_SOCKET=$NORTHSTAR_GPU_WORKER_SOCKET" "NORTHSTAR_GPU_REPLAY_DIR=$NORTHSTAR_GPU_REPLAY_DIR" RUST_LOG=warn,northstar=info,solana_runtime::bank::er_replay=debug "${extra[@]}" "$validator" --log --ledger "$work/ledger" --rpc-port 18999 --faucet-port 19900 --bind-address 127.0.0.1 --portal 5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf --bpf-program 5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf "$NORTHSTAR_LIVE_PORTAL_SBF" --bpf-program FpuSfMKs3Bf5bxFZJ8UDDVYTbCGnZURDuLBmhjb5u9XC "$NORTHSTAR_LIVE_OWNER_SBF"
    tmux new-session -d -s "$validator_session" "exec $command >> '$work/validator.log' 2>&1"
    wait_for 120 health
}
start_validator
export NORTHSTAR_LIVE_PAYER="$work/ledger/validator-keypair.json"
launch_test() {
    local test=$1 output=$2
    local test_env=()
    for key in PATH HOME CARGO_TARGET_DIR LD_LIBRARY_PATH NORTHSTAR_LIVE_RPC_URL NORTHSTAR_LIVE_ER_RPC_URL NORTHSTAR_LIVE_PAYER NORTHSTAR_COORDINATOR_EVIDENCE NORTHSTAR_PROOF_JOB_DIR NORTHSTAR_PROOF_CHALLENGER_KEYPAIR NORTHSTAR_CHECKPOINT_PLAN_DIR NORTHSTAR_COORDINATOR_SMOKE NORTHSTAR_COORDINATOR_ER_GATE; do
        if [[ -v $key ]]; then test_env+=("$key=${!key}"); fi
    done
    printf -v command '%q ' env "${test_env[@]}" cargo test --locked -p northstar --features proof-coordinator "$test" -- --ignored --nocapture
    printf 'cd %q; %s > %q 2>&1; echo $? > %q\n' "$root" "$command" "$work/$output.log" "$work/$output.exit" > "$work/$output-launch.sh"
    tmux new-session -d -s "$driver_session" "exec bash '$work/$output-launch.sh'"
}
launch_test live_coordinator_drive_er_checkpoint driver
if [[ ${NORTHSTAR_COORDINATOR_CONTENTION:-0} == 1 ]]; then
    wait_for 120 test -f "$work/public/er-ready"
    export NORTHSTAR_CONTENTION_ROOT="$root" NORTHSTAR_CONTENTION_OUTPUT="$work/public"
    python3 - <<'PY'
import json, os, subprocess, time
from pathlib import Path
root=Path(os.environ['NORTHSTAR_CONTENTION_ROOT']);output=Path(os.environ['NORTHSTAR_CONTENTION_OUTPUT'])
rows=[]
for index in range(6):
    directory=output/f'colocated-{index:02}';directory.mkdir()
    started=time.monotonic()
    with (directory/'request.log').open('w') as log:
        subprocess.run([str(root/'northstar/scripts/gpu-worker.py'),'groth16',str(root/'northstar/zkvm-replay/fixture-v1.bin'),'measurements.json','colocated-validator-stress'],cwd=directory,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=180)
    measurement=json.loads((directory/'measurements.json').read_text())
    phase=next(phase for phase in measurement['phases'] if phase['phase']=='groth16')
    assert phase['prove_and_wrap_ms']<=120000
    rows.append({'index':index,'request_s':time.monotonic()-started,'prove_and_wrap_ms':phase['prove_and_wrap_ms'],'loadavg':Path('/proc/loadavg').read_text().strip(),'gpu':subprocess.check_output(['nvidia-smi','--query-gpu=memory.used,utilization.gpu','--format=csv,noheader,nounits'],text=True).strip()})
(output/'contention.json').write_text(json.dumps({'schema':'northstar-colocated-stress-v1','validator_running':True,'fresh_er_session_active':True,'serial':rows},indent=2))
Path(os.environ['NORTHSTAR_COORDINATOR_ER_GATE']).touch()
PY
fi
wait_for 600 test -f "$work/driver.exit"
[[ $(<"$work/driver.exit") == 0 ]]
tmux kill-session -t "$driver_session" 2>/dev/null || true
if [[ $mode == smoke ]]; then
    cp "$work/driver.log" "$work/public/driver.log"
    exit 0
fi
fence=$(<"$work/public/driver-fence")
if [[ $mode == upload ]]; then
    wait_for 180 test -f "$work/upload-fence"
    upload_fence=$(<"$work/upload-fence")
    ((upload_fence <= fence)) || fence=$upload_fence
else
    wait_for 60 test -f "$work/prover-entered"
fi
snapshot_ready() {
    local archive slot
    for archive in "$work/ledger"/snapshot-*.tar.zst; do
        slot=${archive##*/snapshot-}; slot=${slot%%-*}
        if [[ $slot =~ ^[0-9]+$ ]] && ((slot >= fence)); then
            printf '{"finalized_fence":%s,"snapshot_slot":%s}\n' "$fence" "$slot" > "$work/public/snapshot-fence.json"
            return 0
        fi
    done
    return 1
}
wait_for 120 snapshot_ready
if [[ $mode == proving ]]; then
    previous_requests=$(find "$work/worker-state" -mindepth 3 -maxdepth 3 -name witness.bin | wc -l)
    touch "$work/release-proof"
    inflight() {
        local count
        count=$(find "$work/worker-state" -mindepth 3 -maxdepth 3 -name witness.bin | wc -l)
        (( count > previous_requests )) && nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits | awk '$1 > 0 { found=1 } END { exit !found }'
    }
    wait_for 30 inflight
fi
old_pid=$(tmux list-panes -t "$validator_session" -F '#{pane_pid}')
[[ $(readlink -f "/proc/$old_pid/exe") == "$validator" ]]
kill -KILL "$old_pid"
wait_for 30 test ! -e "/proc/$old_pid"
if [[ $mode == upload ]]; then
    tmux kill-session -t "$worker_session"
    printf '{"worker_stopped_before_validator_restart":true}\n' > "$work/public/worker-offline.json"
fi
start_validator restart
printf '{"mode":"%s","signal":"SIGKILL","same_ledger":true,"snapshot_only":true,"driver_exited_before_restart":true}\n' "$mode" > "$work/public/restart.json"
launch_test live_coordinator_observe_settlement observer
wait_for 800 test -f "$work/observer.exit"
[[ $(<"$work/observer.exit") == 0 ]]
cp "$work/driver.log" "$work/observer.log" "$work/public/"
grep -E 'Proof (coordinator|checkpoint|job|upload)' "$work/validator.log" > "$work/public/coordinator-events.log"
if [[ $mode == upload ]]; then [[ $(grep -c 'Proof job proving:' "$work/public/coordinator-events.log") == 1 ]]; fi
printf '0\n' > "$work/public/result"
