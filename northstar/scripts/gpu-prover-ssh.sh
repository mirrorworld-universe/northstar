#!/usr/bin/env bash
set -euo pipefail
: "${NORTHSTAR_GPU_SSH:?authorized SSH destination required}"
: "${NORTHSTAR_GPU_REMOTE_RUNNER:?absolute remote runner executable required}"
: "${NORTHSTAR_GPU_REMOTE_ARTIFACTS:?absolute remote artifact directory required}"
[[ $NORTHSTAR_GPU_REMOTE_RUNNER == /* && $NORTHSTAR_GPU_REMOTE_ARTIFACTS == /* ]]
remote_command() {
    local command
    printf -v command '%q ' "$@"
    timeout --kill-after=5s 210s ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 "$NORTHSTAR_GPU_SSH" "$command"
}
if [[ $# == 1 && $1 == preflight ]]; then
    remote_command "$NORTHSTAR_GPU_REMOTE_RUNNER" preflight
    exit
fi
[[ $# == 4 && $1 == groth16 ]] || { echo 'Expected preflight or groth16 WITNESS MEASUREMENTS PROFILE' >&2; exit 2; }
# Use only simple remote paths for scp's remote operand parsing.
[[ $NORTHSTAR_GPU_REMOTE_ARTIFACTS =~ ^/[a-zA-Z0-9_./-]+$ ]]
remote=$(remote_command mktemp -d "$NORTHSTAR_GPU_REMOTE_ARTIFACTS/live.XXXXXXXX")
timeout --kill-after=5s 30s scp -q -o BatchMode=yes -o ConnectTimeout=10 "$2" "$NORTHSTAR_GPU_SSH:$remote/witness-v2.bin"
printf -v command 'cd %q && exec %q groth16 witness-v2.bin measurements.json %q > prover.log 2>&1' "$remote" "$NORTHSTAR_GPU_REMOTE_RUNNER" "$4"
remote_command bash -c "$command"
timeout --kill-after=5s 30s scp -q -o BatchMode=yes -o ConnectTimeout=10 "$NORTHSTAR_GPU_SSH:$remote/measurements.json" "$3"
for file in northstar-sp1-groth16.bin northstar-sp1-groth16-onchain.bin northstar-sp1-public-inputs.bin prover.log; do
    timeout --kill-after=5s 30s scp -q -o BatchMode=yes -o ConnectTimeout=10 "$NORTHSTAR_GPU_SSH:$remote/$file" "$PWD/$file"
done
