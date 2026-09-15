#!/usr/bin/env bash
set -euo pipefail
umask 077
if [[ $# != 1 || $1 != /* ]]; then
    echo 'usage: provision-gpu-userspace.sh /absolute/new/prefix' >&2
    echo 'Requires an existing NVIDIA driver, CUDA toolkit, Rust 1.97.1, Go, CMake, C/C++ toolchain, protoc and libclang.' >&2
    exit 2
fi
prefix=$1
[[ ! -e $prefix && ! -L $prefix ]] || { echo 'Refusing an existing prefix; preserve prior builds and use a fresh directory.' >&2; exit 2; }
for command in git cmake nvcc cc go rustup cargo protoc python3 gzip sha256sum nvidia-smi timeout ldd; do
    command -v "$command" >/dev/null || { echo "Missing prerequisite: $command" >&2; exit 2; }
done
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]]
rustup run 1.97.1 rustc --version
nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv,noheader
root=$(cd "$(dirname "$0")/../.." && pwd)
replay="$root/northstar/zkvm-replay"
mkdir -p "$prefix/src" "$prefix/bin"
trap 'echo "Provisioning failed; retained diagnostics in $prefix" >&2' ERR
echo "Provisioning log: $prefix/provision.log"
exec > "$prefix/provision.log" 2>&1
export PREFIX="$prefix"
python3 - <<'PY'
import hashlib, json, os, urllib.request, zipfile
from pathlib import Path
prefix = Path(os.environ['PREFIX'])
expected = '25bba2dfb01d48a9b59ca474a1ac43c6ebf7011f1b0b8cc44f54eb6ac48a96c3'
with urllib.request.urlopen('https://pypi.org/pypi/nvidia-cuda-runtime-cu12/12.9.79/json', timeout=30) as response:
    metadata = json.load(response)
asset = next(asset for asset in metadata['urls'] if asset['digests']['sha256'] == expected)
wheel = prefix / 'cuda-runtime.whl'
with urllib.request.urlopen(asset['url'], timeout=60) as response:
    wheel.write_bytes(response.read())
if hashlib.sha256(wheel.read_bytes()).hexdigest() != expected:
    raise SystemExit('CUDA runtime wheel hash mismatch')
with zipfile.ZipFile(wheel) as archive:
    archive.extractall(prefix / 'cuda12-runtime')
PY
revision=b62bbbe518a73214da10ece26969ad55e6fa0cd0
git clone --depth 1 --branch v3.2.2 https://github.com/ingonyama-zk/icicle-gnark.git "$prefix/src/icicle-gnark"
[[ $(git -C "$prefix/src/icicle-gnark" rev-parse HEAD) == "$revision" ]]
printf '#include <cuda_runtime.h>\nint main() { return 0; }\n' > "$prefix/cuda-probe.cu"
cuda_flags=()
if ! nvcc -c "$prefix/cuda-probe.cu" -o "$prefix/cuda-probe.o" > "$prefix/cuda-probe.log" 2>&1; then
    grep -q 'rsqrt' "$prefix/cuda-probe.log" || { echo 'CUDA compiler probe failed; inspect cuda-probe.log' >&2; exit 1; }
    # CUDA 13.1 conflicts with glibc 2.43's C23 rsqrt declarations; leave system headers intact.
    header="/usr/include/$(cc -print-multiarch)/bits/mathcalls.h"
    [[ -f $header ]]
    mkdir -p "$prefix/cuda-compat/bits"
    python3 - "$header" "$prefix/cuda-compat/bits/mathcalls.h" <<'PY'
import sys
from pathlib import Path
text = Path(sys.argv[1]).read_text()
start = text.rfind('#if ', 0, text.index('(rsqrt,'))
end = text.index('\n', start)
if text[start:end] != '#if __GLIBC_USE (IEC_60559_FUNCS_EXT_C23)':
    raise SystemExit('Unrecognized CUDA/glibc conflict; no overlay applied')
text = text[:end] + ' && !defined __CUDACC__' + text[end:]
Path(sys.argv[2]).write_text(text)
PY
    cuda_flags=("-DCMAKE_CUDA_FLAGS=-I$prefix/cuda-compat")
    nvcc -I"$prefix/cuda-compat" -c "$prefix/cuda-probe.cu" -o "$prefix/cuda-probe.o"
fi
for curve in bn254 bls12_377 bls12_381 bw6_761; do
    cmake -S "$prefix/src/icicle-gnark/icicle" -B "$prefix/build-$curve" \
        -DCMAKE_CUDA_COMPILER="$(command -v nvcc)" -DCMAKE_CUDA_ARCHITECTURES=native \
        -DCMAKE_INSTALL_PREFIX="$prefix" -DCMAKE_INSTALL_LIBDIR=lib \
        -DCURVE="$curve" -DMSM=ON -DNTT=ON -DG2=ON -DCMAKE_BUILD_TYPE=Release "${cuda_flags[@]}"
    cmake --build "$prefix/build-$curve" --target install -j "${NORTHSTAR_GPU_BUILD_JOBS:-8}"
done
export CARGO_TARGET_DIR="$prefix/target"
export LIBRARY_PATH="$prefix/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$prefix/cuda12-runtime/nvidia/cuda_runtime/lib:$prefix/lib:$(dirname "$(dirname "$(command -v nvcc)")")/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export C_INCLUDE_PATH="$prefix/include${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}"
export ICICLE_BACKEND_INSTALL_DIR="$prefix/lib/backend"
elf="$CARGO_TARGET_DIR/elf-compilation/riscv64im-succinct-zkvm-elf/release/northstar-zkvm-replay-program"
mkdir -p "$(dirname "$elf")"
gzip -dc "$replay/evidence/l40s-v1/candidate.elf.gz" > "$elf"
printf '23f1520bf46ab8852770c0f4802005c8cb88b7fa6657f99f2d7c4a948d1551c5  %s\n' "$elf" | sha256sum -c -
(cd "$replay"; SP1_SKIP_PROGRAM_BUILD=true cargo +1.97.1 build --release --locked -p northstar-zkvm-replay-script --features cuda)
{
    printf '#!/usr/bin/env bash\nset -euo pipefail\n'
    printf 'export LD_LIBRARY_PATH=%q\n' "$LD_LIBRARY_PATH"
    printf 'export ICICLE_BACKEND_INSTALL_DIR=%q\n' "$ICICLE_BACKEND_INSTALL_DIR"
    printf 'export NORTHSTAR_GPU_REPLAY_DIR=%q\n' "$replay"
    printf 'export NORTHSTAR_GPU_PROVER=%q\n' "$CARGO_TARGET_DIR/release/northstar-zkvm-replay-script"
    printf 'exec %q "$@"\n' "$root/northstar/scripts/gpu-prover.py"
} > "$prefix/bin/gpu-prover"
chmod 700 "$prefix/bin/gpu-prover"
ldd "$CARGO_TARGET_DIR/release/northstar-zkvm-replay-script" > "$prefix/linked-libraries.txt"
if grep 'not found' "$prefix/linked-libraries.txt"; then exit 1; fi
grep -q 'libicicle_' "$prefix/linked-libraries.txt"
if grep 'libicicle_' "$prefix/linked-libraries.txt" | grep -vF "$prefix/lib/"; then
    echo 'Icicle resolved outside the private prefix' >&2
    exit 1
fi
"$prefix/bin/gpu-prover" preflight
printf 'icicle_revision=%s\nsp1_sdk=6.1.0\nhost_rust=1.97.1\n' "$revision" > "$prefix/READY"
echo "Ready: $prefix/bin/gpu-prover. Run a compatibility proof to warm circuits before timing challenges."
