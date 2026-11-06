#!/usr/bin/env bash
# Sonic: Keep opt-in builds separate from live binaries and retained protocol state.
set -euo pipefail
umask 077
export RUSTUP_AUTO_INSTALL=0

if [[ $# != 1 || $1 != /* ]]; then
  echo "Usage: build-continuity-v2.sh ABSOLUTE_NEW_STAGING_DIRECTORY" >&2
  exit 2
fi
stage=$1
if [[ -e "$stage" || -L "$stage" ]]; then
  echo "Staging directory already exists; retained artifacts are never reset" >&2
  exit 1
fi
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
target=${NORTHSTAR_CONTINUITY_BUILD_TARGET_DIR:-$root/target/continuity-v2-build}
cd "$root"
sbf_version=$(cargo build-sbf --version)
revision=$(git rev-parse --verify HEAD)
mkdir -- "$stage"
mkdir -- "$stage/bin" "$stage/lib" "$stage/programs"

cargo build-sbf --tools-version v1.56 --arch v0 \
  --manifest-path northstar/programs/portal/Cargo.toml \
  --features checkpoint-continuity-v2 --sbf-out-dir "$stage/programs" -- --locked
cargo build-sbf --tools-version v1.56 --arch v0 \
  --manifest-path northstar/programs/token-bridge/Cargo.toml \
  --features checkpoint-continuity-v2 --sbf-out-dir "$stage/programs" -- --locked
cargo build --locked --release --target-dir "$target" --bin agave-validator \
  --features northstar/checkpoint-continuity-v2
cargo build --locked --release --target-dir "$target" --package solana-cli --bin solana \
  --no-default-features --features checkpoint-continuity-v2
cargo build --locked --release --target-dir "$target" --package northstar-geyser-events --lib
install -m 755 -- "$target/release/agave-validator" "$stage/bin/agave-validator"
install -m 755 -- "$target/release/solana" "$stage/bin/solana"
install -m 500 -- "$target/release/libnorthstar_geyser_events.so" "$stage/lib/libnorthstar_geyser_events.so"

python3 - "$stage" "$revision" "$sbf_version" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import sys

stage, revision, builder = Path(sys.argv[1]), sys.argv[2], sys.argv[3].splitlines()[0]
files = {
    "validator": stage / "bin/agave-validator",
    "cli": stage / "bin/solana",
    "geyser_events": stage / "lib/libnorthstar_geyser_events.so",
    "portal": stage / "programs/northstar_portal.so",
    "token_bridge": stage / "programs/northstar_token_bridge.so",
}
checksums = {}
for name, path in files.items():
    data = path.read_bytes()
    if not data.startswith(b"\x7fELF"):
        raise ValueError(f"missing or invalid ELF: {name}")
    checksums[name] = hashlib.sha256(data).hexdigest()
manifest = {
    "version": 2,
    "protocol": "continuity-v2",
    "source_revision": revision,
    "source_binding": "caller-provided checkout; not an independent build attestation",
    "sbf_builder": builder,
    "platform_tools": "v1.56",
    "arch": "v0",
    "sha256": checksums,
    "activation_performed": False,
    "hosted_qualified": False,
    "proof_acceptance_qualified": False,
}
with (stage / "manifest.json.tmp").open("x") as output:
    json.dump(manifest, output, indent=2)
    output.write("\n")
    output.flush()
    os.fsync(output.fileno())
os.replace(stage / "manifest.json.tmp", stage / "manifest.json")
PY
printf 'Continuity-v2 artifacts staged; no programs deployed or services changed: %s\n' "$stage"
