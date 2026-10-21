#!/usr/bin/env python3
"""Check Icicle's compiler-cache behavior with CMake and Clang, without CUDA or downloads."""

import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile

script_path = (
    Path(sys.argv[1])
    if len(sys.argv) > 1
    else Path(__file__).with_name("provision-gpu-userspace.sh")
)
script = script_path.read_text()
start = script.index('    cmake -S "$prefix/src/icicle-gnark/icicle"')
end = script.index('\n    cmake --build', start)
configure = script[start:end]
with tempfile.TemporaryDirectory(prefix="northstar-cmake-regression-") as temporary:
    prefix = Path(temporary) / "private"
    source = prefix / "src/icicle-gnark/icicle"
    source.mkdir(parents=True)
    (source / "CMakeLists.txt").write_text('''cmake_minimum_required(VERSION 3.18)
project(icicle_compiler_regression)
find_program(CLANG_COMPILER clang++)
find_program(CLANG_C_COMPILER clang)
if(CLANG_COMPILER AND CLANG_C_COMPILER)
    set(CMAKE_CXX_COMPILER ${CLANG_COMPILER} CACHE STRING "Clang++ compiler" FORCE)
    set(CMAKE_C_COMPILER ${CLANG_C_COMPILER} CACHE STRING "Clang compiler" FORCE)
endif()
add_library(prefix_probe SHARED probe.cpp)
install(TARGETS prefix_probe LIBRARY DESTINATION "${CMAKE_INSTALL_PREFIX}/lib")
''')
    (source / "probe.cpp").write_text('int prefix_probe() { return 0; }\n')
    env = dict(os.environ, CUDA_ARCHS="86")
    command = (
        f'prefix={shlex.quote(str(prefix))}; curve=bn254; cuda_flags=();\n'
        f"{configure}\n{configure}"
    )
    result = subprocess.run(
        ["bash", "-euc", command],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if result.returncode:
        print(result.stdout[-3500:])
        raise SystemExit("CMake configuration failed")
    cache = {}
    for line in (prefix / "build-bn254/CMakeCache.txt").read_text().splitlines():
        if not line.startswith(("#", "//")) and ":" in line and "=" in line:
            key, value = line.split("=", 1)
            cache[key.split(":", 1)[0]] = value
    if (
        cache.get("CMAKE_INSTALL_PREFIX") != str(prefix)
        or cache.get("CMAKE_CUDA_ARCHITECTURES") != "86"
        or cache.get("CURVE") != "bn254"
    ):
        raise SystemExit(
            "Private prefix, curve, and CUDA architecture must survive CMake regeneration"
        )
    result = subprocess.run(
        ["cmake", "--build", str(prefix / "build-bn254"), "--target", "install", "-j", "1"],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if result.returncode:
        print(result.stdout[-3500:])
        raise SystemExit("Private installation failed")
    print("Private prefix, curve, and CUDA architecture preserved across configure/build/install.")
