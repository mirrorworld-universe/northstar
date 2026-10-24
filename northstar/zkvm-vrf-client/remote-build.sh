#!/usr/bin/env bash
# Coordinator-run; no installation, guest build, proof or key switch.
set -euo pipefail
umask 077
if [[ ${1:-} == --inside ]]; then
  [[ $# == 3 ]]
  source_root=$2
  out=$3
  cd "$source_root/northstar/zkvm-vrf-client"
  cargo +1.98.1 --version >"$out/cargo-version.txt"
  rustup run 1.98.1 rustc --version --verbose >"$out/rustc-version.txt"
  sha256sum Cargo.lock >"$out/input-lock.sha256"
  cargo +1.98.1 metadata --locked --features cuda --format-version=1 >"$out/cargo-metadata.json" 2>"$out/metadata.log"
  python3 - "$out/cargo-metadata.json" "$out/dependency-audit.json" <<'PY'
import json,sys
from pathlib import Path
metadata=json.loads(Path(sys.argv[1]).read_text())
packages=[p for p in metadata['packages'] if p['name'].startswith(('sp1-','slop-'))]
assert packages and all(p['version']=='6.8.0' for p in packages),[(p['name'],p['version']) for p in packages]
sdk=[p for p in packages if p['name']=='sp1-sdk']
assert len(sdk)==1
node=next(n for n in metadata['resolve']['nodes'] if n['id']==sdk[0]['id'])
assert not {'network','tee','reserved-capacity'} & set(node['features']),node['features']
Path(sys.argv[2]).write_text(json.dumps({'packages':[{k:p[k] for k in ('name','version','source')} for p in packages], 'sdk_features':node['features']},indent=2)+'\n')
PY
  cp Cargo.lock "$out/normalized-Cargo.lock"
  sha256sum Cargo.lock >"$out/normalized-lock.sha256"
  # Deterministic overlays have old mtimes; never reuse stale first-party metadata.
  cargo +1.98.1 clean --locked --release -p northstar-zkvm-vrf-client -p northstar-zk-types >"$out/package-clean-release.log" 2>&1
  cargo +1.98.1 clean --locked -p northstar-zkvm-vrf-client -p northstar-zk-types >"$out/package-clean-dev.log" 2>&1
  cargo +1.98.1 build --release --locked --features cuda >"$out/cargo-build.log" 2>&1
  cp "$CARGO_TARGET_DIR/release/northstar-zkvm-vrf-client" "$out/northstar-zkvm-vrf-client"
  sha256sum "$out/northstar-zkvm-vrf-client" >"$out/client.sha256"
  ldd "$out/northstar-zkvm-vrf-client" >"$out/linked-libraries.txt"
  if grep -q 'not found' "$out/linked-libraries.txt"; then exit 1; fi
  "$out/northstar-zkvm-vrf-client" --help >"$out/client-help.txt"
  cargo +1.98.1 test --locked --no-default-features --tests >"$out/guard-tests.log" 2>&1
  sha256sum -c "$out/normalized-lock.sha256" >"$out/final-lock-check.log"
  printf 'Compiled runtime-ELF client; no actual guest execution/proof/key acceptance claimed.\n' >"$out/CLASSIFICATION.txt"
  exit 0
fi
[[ $# == 4 ]] || { printf 'usage: %s SOURCE_ROOT GPU_PREFIX ICICLE_PREFIX FRESH_OUTPUT_DIR\n' "$0" >&2; exit 2; }
source_root=$(realpath -e "$1")
gpu=$(realpath -e "$2")
icicle=$(realpath -e "$3")
out=$4
icicle_include="$icicle/include"
if [[ ! -d "$icicle_include" ]]; then
  icicle_include="$icicle/src/icicle-gnark/icicle/include"
  [[ $(git -C "$icicle/src/icicle-gnark" rev-parse HEAD) == b62bbbe518a73214da10ece26969ad55e6fa0cd0 ]]
  git -C "$icicle/src/icicle-gnark" diff --quiet HEAD -- icicle/include
  [[ -z $(git -C "$icicle/src/icicle-gnark" ls-files --others --exclude-standard -- icicle/include) ]]
fi
[[ -d "$icicle_include" && -d "$icicle/lib" && -f "$source_root/northstar/zkvm-vrf-client/Cargo.toml" ]]
mkdir "$out"
out=$(realpath -e "$out")
script=$(realpath -e "$0")
exec >"$out/build.log" 2>&1
trap 'code=$?; printf "%s\n" "$code" >"$out/build.exit"; date -u -Iseconds >"$out/finished"' EXIT
[[ $(sha256sum "$gpu/bin/with-optimized-sha-prover" | cut -d' ' -f1) == 5165545ea73a688ef97815a0f97007776db4521ed38633c7eff98c66d771d5ec ]]
[[ $(sha256sum "$gpu/nonce-server/home/.sp1/bin/sp1-gpu-server" | cut -d' ' -f1) == 375fb14b5bf07d6b4b2d2a80608d2ecd718509ee6812986e4ed8335b0d61b7c5 ]]
[[ $(sha256sum "$gpu/sha-optimized.env" | cut -d' ' -f1) == 152ff9f697cd5a08345559013d3cc51f890c28c45e4587d1d08a058be7d4e7b8 ]]
[[ $(sha256sum "$gpu/nonce-server/bin/with-gpu-server" | cut -d' ' -f1) == 88f04e67ad792ac06f0f6aa2430ab4c87fb2529c1e155b8a92e8e99a1fd2d69f ]]
[[ $(sha256sum "$gpu/nonce-server/source.patch" | cut -d' ' -f1) == 16773cd1a50fbcc0819b21bad5d30cfaa7cfb6ab3992f4bbfd0213c0ba9a3f1c ]]
outer_vk="$gpu/nonce-server/home/.sp1/circuits/groth16/v6.1.0/groth16_vk.bin"
[[ $(sha256sum "$outer_vk" | cut -d' ' -f1) == 4388a21c687fdd5f218d7e3d13190cac4c5355818d3605fd5fb811df468ee696 ]]
date -u -Iseconds >"$out/started"
printf '%s\n' "$icicle_include" >"$out/icicle-include-root.txt"
sha256sum "$script" "$source_root/northstar/zkvm-vrf-client/"Cargo.{toml,lock} \
  "$source_root/northstar/zk-types/"{Cargo.toml,src/lib.rs,src/trace.rs} \
  "$gpu/nonce-server/source.patch" "$gpu/sha-optimized.env" "$outer_vk" >"$out/build-inputs.sha256"
original_cargo=${CARGO_HOME:-$HOME/.cargo}
original_rustup=${RUSTUP_HOME:-$HOME/.rustup}
original_path="$HOME/.cargo/bin:$gpu/guest-tools/bin:$PATH"
env -u RUSTC -u RUSTDOC -u RUSTUP_TOOLCHAIN \
  "$gpu/bin/with-optimized-sha-prover" env \
  CARGO_HOME="$original_cargo" RUSTUP_HOME="$original_rustup" PATH="$original_path" \
  CARGO_TARGET_DIR="$icicle/target" CARGO_BUILD_JOBS=2 \
  LIBRARY_PATH="$icicle/lib${LIBRARY_PATH:+:$LIBRARY_PATH}" \
  C_INCLUDE_PATH="$icicle_include${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}" \
  "$BASH" "$script" --inside "$source_root" "$out"
