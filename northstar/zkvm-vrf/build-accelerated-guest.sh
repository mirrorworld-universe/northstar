#!/usr/bin/env bash
# Experimental SHA-256 implementation variant; never installs a Portal key.
set -euo pipefail
workspace=$(cd -- "$(dirname -- "$0")" && pwd)
out=${1:?fresh absolute result directory required}
[[ $out == /* && ! -e $out ]] || { printf 'fresh absolute output path required\n' >&2; exit 2; }
mkdir -p "$out"
exec >"$out/build.log" 2>&1
trap 'r=$?; printf "%s\n" "$r" >"$out/build.exit"' EXIT
[[ ${NORTHSTAR_VRF_BUILD_VARIANT:-} == sha2-sp1-v1 ]] || { printf 'explicit sha2-sp1-v1 opt-in required\n' >&2; exit 2; }
policy=c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d
[[ ${NORTHSTAR_VRF_PROFILE_SHA256:-} == "$policy" ]] || { printf 'fixed typed policy digest mismatch\n' >&2; exit 2; }
if [[ -n ${NORTHSTAR_GUEST_COMPILER_BIN:-} ]]; then
    export PATH="$NORTHSTAR_GUEST_COMPILER_BIN:$PATH"
fi
export RUSTUP_AUTO_INSTALL=0 CARGO_BUILD_JOBS=2
if command -v riscv64-unknown-elf-gcc >/dev/null; then
    export CC_riscv64im_succinct_zkvm_elf=riscv64-unknown-elf-gcc
    export CFLAGS_riscv64im_succinct_zkvm_elf='-march=rv64im -mabi=lp64 -mno-relax'
elif command -v clang >/dev/null; then
    export CC_riscv64im_succinct_zkvm_elf=clang
    export CFLAGS_riscv64im_succinct_zkvm_elf='--target=riscv64-unknown-elf -march=rv64im -mabi=lp64 -mno-relax'
else
    printf 'RISC-V cross C compiler or clang required; no tools installed by this recipe\n' >&2
    exit 2
fi
features=candidate
case ${NORTHSTAR_VRF_DIAGNOSTIC:-none} in
    none) ;;
    crypto) features+=,crypto-checks ;;
    cycles) features+=,cycle-profiling ;;
    all) features+=,crypto-checks,cycle-profiling ;;
    *) printf 'diagnostic mode must be none/crypto/cycles/all\n' >&2; exit 2 ;;
esac
cd "$workspace/program"
rustc --version
cargo prove --version
cargo metadata --locked --format-version 1 --manifest-path "$workspace/program/Cargo.toml" \
    --filter-platform riscv64im-succinct-zkvm-elf --features "$features" >"$out/cargo-metadata.json"
python3 - "$out/cargo-metadata.json" <<'PY'
import json, sys
packages = json.load(open(sys.argv[1]))['packages']
sha = [p for p in packages if p['name'] == 'sha2' and p['version'] == '0.10.9']
assert len(sha) == 1, sha
assert sha[0]['source'].endswith('#e48b656ebc806117554bb33c2f8687e4637e37ff'), sha
assert sha[0]['source'].startswith('git+https://github.com/sp1-patches/RustCrypto-hashes?rev='), sha
for package in packages:
    if package['name'].startswith(('sp1-', 'slop-')):
        assert package['version'] == '6.8.0', (package['name'], package['version'])
assert {p['version'] for p in packages if p['name'] == 'curve25519-dalek'} == {'4.1.3', '5.0.0'}
assert {p['version'] for p in packages if p['name'] == 'ed25519-dalek'} == {'3.0.0'}
print('locked SHA-256 patch and unchanged curve/SP1 package versions verified')
PY
# Archive mtimes do not establish freshness. Clear only affected guest packages,
# including C objects that previously carried unsupported compressed instructions.
CARGO_TARGET_DIR="$workspace/target/elf-compilation" cargo clean --locked --release \
    --target riscv64im-succinct-zkvm-elf --manifest-path "$workspace/Cargo.toml" \
    -p sha2@0.10.9 -p blst -p blstrs -p blake3 \
    -p northstar-vrf-sbpf-replay -p northstar-transaction-proof \
    -p northstar-zkvm-vrf-shared -p northstar-zkvm-vrf-program \
    -p solana-program-runtime -p solana-bpf-loader-program -p solana-syscalls \
    -p agave-feature-set -p solana-svm-measure
cargo prove build --locked --ignore-rust-version --packages northstar-zkvm-vrf-program \
    --binaries northstar-zkvm-vrf-candidate --features "$features" --output-directory "$out/elf"
python3 - "$workspace" "$out" "$features" "$policy" <<'PY'
import hashlib, json, pathlib, struct, sys
workspace, out = map(pathlib.Path, sys.argv[1:3])
receipts = []
for path in sorted((out/'elf').iterdir()):
    data = path.read_bytes()
    assert len(data) >= 64 and data[:6] == b'\x7fELF\x02\x01', path.name
    machine, = struct.unpack_from('<H', data, 18)
    flags, = struct.unpack_from('<I', data, 48)
    assert machine == 243 and flags == 0, (path.name, machine, flags)
    receipts.append({'file':path.name, 'sha256':hashlib.sha256(data).hexdigest(), 'bytes':len(data), 'machine':machine, 'flags':flags})
assert len(receipts) == 1
receipt = {'schema':'northstar.vrf-sha2-build.v1', 'variant':'sha2-sp1-v1', 'features':sys.argv[3],
    'policy_profile_sha256':sys.argv[4], 'policy_checkpoint_steps':1,
    'sha2_version':'0.10.9', 'sha2_patch_revision':'e48b656ebc806117554bb33c2f8687e4637e37ff',
    'cargo_lock_sha256':hashlib.sha256((workspace/'Cargo.lock').read_bytes()).hexdigest(),
    'elf':receipts[0], 'classification':'build-and-ISA-screening-only', 'execution_verified':False,
    'key_configured':False, 'production_acceptance':False}
(out/'build-variant.json').write_text(json.dumps(receipt, indent=2)+'\n')
PY
sha256sum "$out"/elf/* >"$out/elf-sha256.txt"
