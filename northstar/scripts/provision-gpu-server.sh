#!/usr/bin/env bash
set -euo pipefail
umask 077
if [[ $# != 2 || $1 != /* || $2 != /* ]]; then
    echo 'usage: provision-gpu-server.sh /absolute/icicle-prefix /absolute/new/server-prefix' >&2
    exit 2
fi
icicle=$1
prefix=$2
low_memory=${NORTHSTAR_GPU_LOW_MEMORY:-0}
g5_optimized=${NORTHSTAR_GPU_G5_OPTIMIZED:-0}
[[ $low_memory == 0 || $low_memory == 1 ]] || { echo 'NORTHSTAR_GPU_LOW_MEMORY must be 0 or 1.' >&2; exit 2; }
[[ $g5_optimized == 0 || $g5_optimized == 1 ]] || { echo 'NORTHSTAR_GPU_G5_OPTIMIZED must be 0 or 1.' >&2; exit 2; }
script_dir=$(cd "$(dirname "$0")" && pwd)
[[ ! -e $prefix && ! -L $prefix ]] || { echo 'Refusing an existing server prefix.' >&2; exit 2; }
[[ -f $icicle/READY && -f $icicle/lib/libicicle_curve_bn254.so ]] || { echo 'Complete provision-gpu-userspace.sh first.' >&2; exit 2; }
grep -qx 'icicle_revision=b62bbbe518a73214da10ece26969ad55e6fa0cd0' "$icicle/READY" || { echo 'Unexpected Icicle revision.' >&2; exit 2; }
for command in git cmake nvcc cc go rustup cargo protoc python3 sha256sum nvidia-smi timeout ldd; do
    command -v "$command" >/dev/null || { echo "Missing prerequisite: $command" >&2; exit 2; }
done
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]]
rustup run 1.98.1 rustc --version
cuda=$(dirname "$(dirname "$(command -v nvcc)")")
arch=${CUDA_ARCHS:-$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader | head -1 | tr -d '. ')}
[[ $arch =~ ^[0-9]+$ ]] || { echo 'Specify one numeric CUDA_ARCHS target.' >&2; exit 2; }
mkdir -p "$prefix/bin" "$prefix/home/.sp1/bin"
trap 'echo "Server provisioning failed; retained diagnostics in $prefix" >&2' ERR
echo "Server build log: $prefix/provision.log"
exec > "$prefix/provision.log" 2>&1
export LIBRARY_PATH="$icicle/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$icicle/cuda12-runtime/nvidia/cuda_runtime/lib:$icicle/lib:$cuda/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export C_INCLUDE_PATH="$icicle/include${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}"
export ICICLE_BACKEND_INSTALL_DIR="$icicle/lib/backend"
export CUDA_PATH="$cuda" CUDA_ARCHS="$arch"
if [[ -f $icicle/cuda-compat/bits/mathcalls.h ]]; then
    export NVCC_PREPEND_FLAGS="${NVCC_PREPEND_FLAGS:-} -I$icicle/cuda-compat"
fi
revision=58c4aeadbc504c274dd9fb82ed8130d0939ab756
git clone --depth 1 --branch v6.8.0 https://github.com/succinctlabs/sp1.git "$prefix/src"
[[ $(git -C "$prefix/src" rev-parse HEAD) == "$revision" ]]
patch_sha256=none
if [[ $low_memory == 1 || $g5_optimized == 1 ]]; then
    patch=sp1-v6.8.0-low-memory.patch
    if [[ $g5_optimized == 1 ]]; then patch=sp1-v6.8.0-g5-optimized.patch; fi
    cp "$script_dir/patches/$patch" "$prefix/source.patch"
    git -C "$prefix/src" apply --check "$prefix/source.patch"
    git -C "$prefix/src" apply "$prefix/source.patch"
    patch_sha256=$(sha256sum "$prefix/source.patch" | cut -d ' ' -f 1)
fi
build_jobs=${NORTHSTAR_GPU_BUILD_JOBS:-8}
if [[ $g5_optimized == 1 ]]; then
    build_jobs=${NORTHSTAR_GPU_BUILD_JOBS:-2}
    (cd "$prefix/src/crates/recursion/gnark-ffi/go"; go test -tags debug,icicle \
        sp1/groth16_diagnostics{,_test}.go sp1/groth16_domain_cache{,_test}.go)
fi
(cd "$prefix/src"; CARGO_TARGET_DIR="$prefix/target" cargo +1.98.1 build --release --locked \
    -j "$build_jobs" -p sp1-gpu-server --features groth16-cuda)
if [[ $g5_optimized == 1 ]]; then
    (
        cd "$prefix/src"
        export CARGO_TARGET_DIR="$prefix/target"
        cargo +1.98.1 test --release --locked -j "$build_jobs" \
            -p sp1-gpu-challenger bn254_grid_bounds
        for blocks in 1 128; do
            SP1_GPU_BN254_GRIND_BLOCKS=$blocks cargo +1.98.1 test --release --locked \
                -j "$build_jobs" -p sp1-gpu-basefold test_grinding_multi_field32
        done
    )
fi
server="$prefix/home/.sp1/bin/sp1-gpu-server"
cp "$prefix/target/release/sp1-gpu-server" "$server"
ldd "$server" > "$prefix/linked-libraries.txt"
if grep 'not found' "$prefix/linked-libraries.txt"; then exit 1; fi
grep -q 'libicicle_' "$prefix/linked-libraries.txt"
if grep 'libicicle_' "$prefix/linked-libraries.txt" | grep -vF "$icicle/lib/"; then
    echo 'Icicle resolved outside its private prefix' >&2
    exit 1
fi
[[ $("$server" --version) == 6.8.0 ]]
{
    printf '#!/usr/bin/env bash\nset -euo pipefail\n'
    printf 'export HOME=%q\n' "$prefix/home"
    printf 'export LD_LIBRARY_PATH=%q\n' "$LD_LIBRARY_PATH"
    printf 'export ICICLE_BACKEND_INSTALL_DIR=%q\n' "$ICICLE_BACKEND_INSTALL_DIR"
    printf 'exec "$@"\n'
} > "$prefix/bin/with-gpu-server"
chmod 700 "$prefix/bin/with-gpu-server"
sha256sum "$server" > "$prefix/server.sha256"
printf 'sp1_server_revision=%s\ngroth16_cuda=true\ncuda_archs=%s\nsource_patch_sha256=%s\n' "$revision" "$arch" "$patch_sha256" > "$prefix/READY"
echo "Built: $prefix/bin/with-gpu-server COMMAND [ARGS...]. Stop other GPU workers, then validate a compatibility proof before use."
