# Pinned GPU userspace setup

`northstar/scripts/provision-gpu-userspace.sh` builds a CUDA prover in a fresh, private prefix on compatible Linux x86_64 hosts. It does not install drivers, use sudo, alter system headers, reboot, deploy Portal, or obtain paid proving capacity.

## Prerequisites and boundary

Supply an already authorized NVIDIA host with a working driver and CUDA toolkit, Rust toolchain **1.97.1**, Go, CMake, a C/C++ toolchain, protoc plus protobuf development headers, libclang, Git, Python 3 and ordinary GNU utilities. Put `nvcc` and Cargo on PATH. Network access is required for pinned sources and Cargo/Go dependencies.

The tested host was an L40S with driver **595.91.07**, CUDA toolkit **13.1.115**, Go **1.26.0**, and Rust **1.97.1**. This is a userspace recipe, not universal OS/driver provisioning. Native compilation can take tens of minutes. `NORTHSTAR_GPU_BUILD_JOBS` defaults to 8.

## Run

From a checkout containing the retained candidate ELF:

```sh
bash northstar/scripts/provision-gpu-userspace.sh "$HOME/northstar-gpu-userspace"
```

Choose a new absolute prefix. The script refuses existing directories and symlinks; preserve failed builds for diagnosis rather than overwriting them. Run the build in a detached tmux session. Logs are retained as `provision.log` inside the prefix.

The recipe:

- Verifies CUDA runtime wheel **12.9.79** against SHA-256 `25bba2dfb01d48a9b59ca474a1ac43c6ebf7011f1b0b8cc44f54eb6ac48a96c3` before extraction.
- Fetches Icicle **v3.2.2**, verifies commit `b62bbbe518a73214da10ece26969ad55e6fa0cd0`, and builds all four required curves into the private prefix.
- Probes the CUDA compiler. On the recognized CUDA 13.1/glibc 2.43 `rsqrt` conflict, uses a private header overlay; unrecognized failures stop the build.
- Decompresses the preserved guest ELF and verifies SHA-256 `23f1520bf46ab8852770c0f4802005c8cb88b7fa6657f99f2d7c4a948d1551c5`. It builds the locked SP1 **6.1.0** host with `SP1_SKIP_PROGRAM_BUILD=true` and a private Cargo target directory.
- Verifies dynamic linkage and requires Icicle libraries to resolve inside the private prefix; runs pinned-key preflight before writing `READY`.

`bin/gpu-prover` is the generated adapter for local invocation or `NORTHSTAR_GPU_REMOTE_RUNNER`. It configures private CUDA runtime and Icicle library paths. Shared Cargo/Go caches and the SDK's user-level SP1 GPU-server/circuit cache may still be used; this is not a hermetic build or an empty-cache benchmark. The wrapper references the source checkout, which must remain available.

After `READY`, generate a compatibility proof in a new output directory:

```sh
mkdir "$HOME/northstar-gpu-compatibility"
cd "$HOME/northstar-gpu-compatibility"
"$HOME/northstar-gpu-userspace/bin/gpu-prover" groth16 \
  /absolute/checkout/northstar/zkvm-replay/fixture-v1.bin \
  "$PWD/measurements.json" userspace-validation
```

Do this before a timed challenge: preflight is not a full Groth16 warm-up. The bounded adapter rejects prove-and-wrap times above 120 seconds, including a cold-cache run that exceeds the target; it does not silently relax the acceptance limit.

## Retained validation

A fresh private-prefix build and CUDA proof passed:

| Check | Result |
| --- | --- |
| Icicle shared libraries resolved privately | 9 of 9 |
| Program key | Matches frozen candidate |
| Prove + wrap | **81.008s** |
| SDK verification | **329ms** |
| On-chain envelope / public inputs | 356 / 256 bytes |
| Header/driver changes | None |

Evidence is under [`evidence/userspace-v1`](../zkvm-replay/evidence/userspace-v1/): proof, measurements, sanitized environment/library hashes and `SHA256SUMS`. The recorded prover source commit and isolated Cargo.lock hash identify the host build inputs; that lockfile matches this checkout. The frozen guest, not a fresh path-dependent guest build, determines the program key. Existing SP1 circuits were shared with previous runs.

The final prefix-refusal/linkage guards were validated separately from the long native build, including linkage against the actual resulting binary. CPU-only CI exercises refusal paths without downloads or GPU access and verifies the retained proof against real Portal SBF.
