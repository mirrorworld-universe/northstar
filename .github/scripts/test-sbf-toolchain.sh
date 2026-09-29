#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
cat > "$work/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == --list ]]; then
    printf '    build-sbf\n'
    exit 0
fi
printf '%s\n' "$@" > "$SBF_TEST_ARGS"
SH
chmod +x "$work/bin/cargo"
export PATH="$work/bin:$PATH" SBF_TEST_ARGS="$work/args"
bash "$root/cargo-build-sbf" --manifest-path northstar/programs/portal/Cargo.toml --features zk-verifier-prototype -- --locked
mapfile -t actual < "$SBF_TEST_ARGS"
expected=(build-sbf --arch v0 --tools-version v1.56 --manifest-path northstar/programs/portal/Cargo.toml --features zk-verifier-prototype -- --locked)
if [[ ${actual[*]} != "${expected[*]}" ]]; then
    printf 'Unexpected SBF arguments: %s\nExpected: %s\n' "${actual[*]}" "${expected[*]}" >&2
    exit 1
fi
printf 'Pinned SBF toolchain arguments passed\n'
