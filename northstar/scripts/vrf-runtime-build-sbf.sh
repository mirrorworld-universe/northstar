#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
manifest="$root/northstar/programs/vrf-runtime-probe/Cargo.toml"
target="$root/northstar/programs/vrf-runtime-probe/target"
out="$target/deploy"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
mkdir -p "$out"

if [[ -n "${VRF_PLATFORM_TOOLS:-}" ]]; then
    # Use an already installed compiler when the CLI wrapper cannot start. Never install tools here.
    tools=$(cd "$VRF_PLATFORM_TOOLS" && pwd)
    for tool in rust/bin/cargo rust/bin/rustc llvm/bin/clang llvm/bin/llvm-ar; do
        [[ -x "$tools/$tool" ]] || { printf 'Missing installed tool: %s\n' "$tools/$tool" >&2; exit 1; }
    done
    cd "$root"
    RUSTC="$tools/rust/bin/rustc" CC="$tools/llvm/bin/clang" AR="$tools/llvm/bin/llvm-ar" \
      CARGO_TARGET_DIR="$target" "$tools/rust/bin/cargo" build --locked \
      --target sbpf-solana-solana --release --manifest-path "$manifest" -p northstar-vrf-runtime-probe
    cp "$target/sbpf-solana-solana/release/northstar_vrf_runtime_probe.so" "$out/"
else
    backend=$(command -v cargo-build-sbf || true)
    [[ -n "$backend" ]] || backend="$HOME/.cargo/bin/cargo-build-sbf"
    [[ -x "$backend" ]] || { printf 'cargo-build-sbf missing; ask to add it to the devshell.\n' >&2; exit 1; }
    compiler="${XDG_CACHE_HOME:-$HOME/.cache}/solana/v1.56/platform-tools/rust/bin/rustc"
    [[ -x "$compiler" ]] || { printf 'Installed platform-tools v1.56 missing; ask to add it.\n' >&2; exit 1; }
    cd "$root"
    RUSTC="$compiler" "$backend" --skip-tools-install --no-rustup-override \
      --tools-version v1.56 --arch v0 --manifest-path "$manifest" --sbf-out-dir "$out" -- --locked
fi
printf 'Fixture ELF: %s\n' "$out/northstar_vrf_runtime_probe.so"
