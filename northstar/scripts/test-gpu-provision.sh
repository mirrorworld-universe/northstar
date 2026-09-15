#!/usr/bin/env bash
set -euo pipefail
script=$(cd "$(dirname "$0")" && pwd)/provision-gpu-userspace.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
expect_refusal() {
    local status=0
    bash "$script" "$@" > "$work/output" 2>&1 || status=$?
    [[ $status == 2 ]] || { echo "Unexpected status: $status" >&2; exit 1; }
}
expect_refusal
expect_refusal relative-prefix
mkdir "$work/existing"
printf 'preserve\n' > "$work/existing/sentinel"
expect_refusal "$work/existing"
[[ $(< "$work/existing/sentinel") == preserve ]]
ln -s "$work/missing" "$work/link"
expect_refusal "$work/link"
[[ ! -e $work/missing ]]
status=0
PATH=/nonexistent "$(command -v bash)" "$script" "$work/new" > "$work/output" 2>&1 || status=$?
[[ $status == 2 && ! -e $work/new ]]
echo 'GPU provisioning guards passed (no downloads or system changes).'
