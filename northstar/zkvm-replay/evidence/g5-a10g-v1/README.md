# A10G / g5.xlarge proving evidence

**Historical baseline under the previous guest pin.** The current candidate uses the [SHA-precompile guest and optimized profile](../g5-a10g-sha-v1/README.md); its Portal verifier deliberately rejects these older-guest proofs. The limits and setup below describe the earlier configuration, not the current recommendation. Reproducing this baseline requires the original guest/verifier binding rather than the current default provisioner.

Measured on 2026-10-20: four vCPUs, 15.4 GiB usable host RAM, NVIDIA A10G (23,028 MiB reported VRAM), driver 595.91.07, CUDA 13.1.115, SP1 6.8.0, and the existing pinned guest ELF. Circuit artifacts were already downloaded and checksum-verified on local NVMe. A 32 GiB NVMe swap partition prevented the original host OOM failures.

## Result and scope

**The warmed SDK process meets the 120-second target for the retained corpus. Cold-start proving does not.**

- All 14 distinct retained witnesses passed native replay, SP1 proof verification, and standalone Portal SBF verification. The corpus contains `fixture-v1.bin` and every retained `witness-v2.bin`; `resolver-fixture.bin` files are Portal fixtures, not replay witnesses.
- The 13 requests after the corpus's first proof took **72.958–98.309 seconds**, including SDK verification. Separate baseline repetitions took 77.871 and 90.101 seconds.
- The fastest completed cold proof plus verification took **160.071 seconds**. This excludes client initialization, and therefore does not establish a two-minute one-shot job deadline.
- Portal accepted all 14 proof/public-input pairs within **84,082 CU**. Changed-proof and changed-public-input controls were rejected. Verification used a freshly built Portal ELF and an existing cached native test harness; their hashes are retained.
- This remains the existing supported replay relation, not arbitrary SVM execution or demonstrated validity of every transaction in a full 16-transaction checkpoint. `production_acceptance` remains **false**.

The existing persistent worker applies its 120-second proof guard to warm-up too. These cold measurements exceed that guard. No deadline or verification guard was relaxed; live worker/coordinator readiness was not demonstrated.

## Capacity limit

A maximum-memory trial reached **15,260 MiB of live Go heap**, against approximately 15,783 MiB of total host RAM. That leaves insufficient room for Rust prover state, the CUDA driver, and the OS. Allowing a 15 GiB proof cgroup still recorded 14.207 GiB resident memory and 5.863 GiB swap, and did not get cold proving below two minutes.

This is a host-memory limit of the current SP1/Groth16 stack. A 32 GiB host, such as g5.2xlarge, is the next capacity test; its deadline performance has not been measured here. Increasing only the cgroup limit does not add physical RAM.

## Completed comparisons

Times below include proof generation, Groth16 wrapping, and SDK verification. "Cold" means a new prover process with circuit files already present. Warm samples reuse the same SDK client and GPU server. Profiles are different configurations, not repetitions of one configuration.

| Profile | Cold (s) | Warm (s) | Outcome |
|---|---:|---:|---|
| Released buffers, 14 GiB cap | 160.753 | 90.101, 77.871 | Recommended conservative profile |
| Same server, 15 GiB cap | 160.071 | 108.364, 87.969 | No useful cold-start improvement |
| zswap/LZ4 | 212.033 | 98.800, 98.979 | Slower; disabled |
| Explicit Go heap scavenging | 191.719 | 90.904, 84.399 | Slower; not shipped |
| Parallel circuit/key initialization; 14-witness corpus | 201.170 | 72.958–98.309 | Cold initialization regressed |
| 18 GiB Go limit, GOGC=10 | 183.933 | 134.506 | Slower; rejected |
| Swap readahead (`page-cluster=3`) | — | — | CUDA MSM failure; no proof |

The readahead trial reported an illegal CUDA memory access during MSM. Its root cause was **not established**; it is not counted as a proof or a successful optimization. `page-cluster=0` was restored. The finite successful samples do not establish production reliability.

Earlier stock-server trials failed from host OOM. Reducing worker counts alone was insufficient. A one-buffer server without releasing idle buffers still exceeded the host budget or stalled. Releasing buffers and returning unused CUDA pool memory made repeated proving possible.

## Changes retained

The opt-in [SP1 patch](../../../scripts/patches/sp1-v6.8.0-low-memory.patch) targets upstream commit `58c4aeadbc504c274dd9fb82ed8130d0939ab756`:

1. Bound each pinned trace-buffer pool to one buffer, with validated opt-in configuration.
2. Return idle pinned allocations when their existing worker lease ends, recreating capacity on the next lease.
3. Configure the CUDA pool release threshold and synchronize before Groth16 so unused device allocations can be returned.

The patch does not change guest execution, proof arithmetic, keys, public inputs, or verification rules. Defaults remain unchanged unless the runtime flags are enabled. The profile limits the exposed worker pools to one worker per stage and requires one in-flight proof.

The userspace provisioner also pins C/C++ compilers before CMake configuration and accepts `CUDA_ARCHS=86`. This prevents Icicle's compiler selection from resetting the private install prefix to `/usr/local` during CMake regeneration. The replay lockfile now matches the workspace's existing `thiserror = 2.0.21` requirement.

`corpus-server.patch` retains the exact broader experimental patch used for the corpus. It adds diagnostic Go initialization/scavenging options and is **not** the recommended provisioning patch. The corpus run also used an 8 GiB CUDA pool threshold; all overrides and binary hashes are recorded in `corpus.json`.

## Historical setup

Use a verified instance-store device for the scratch cache and swap. The provisioners do not format disks. CUDA, NVIDIA drivers, Go, Rust 1.98.1, C/C++ compilers, CMake, and protoc must already be available.

From the repository root, with absolute, fresh installation paths:

```bash
export CUDA_ARCHS=86 CARGO_BUILD_JOBS=2 CMAKE_BUILD_PARALLEL_LEVEL=2
export NORTHSTAR_GPU_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_DEBUG=0
bash northstar/scripts/provision-gpu-userspace.sh "$ICICLE_PREFIX"
NORTHSTAR_GPU_LOW_MEMORY=1 bash northstar/scripts/provision-gpu-server.sh \
  "$ICICLE_PREFIX" "$SERVER_PREFIX"

set -a
source northstar/scripts/gpu-g5.env
set +a
```

Use local NVMe for the private server's `.sp1/circuits` directory. Keep `vm.swappiness=60`, `vm.page-cluster=0`, zswap disabled, and at least the tested swap allowance available. Do not run overlapping proof processes or submit concurrent requests. A 14 GiB RAM / 32 GiB swap cgroup was the conservative test configuration.

The [corpus driver](../../script/src/bin/northstar-proof-corpus.rs) accepts the [manifest](../../g5-corpus-v1.json) and a new output directory. It performs native preflight, generates every proof through CUDA, checks canonical public inputs, verifies every proof with the SDK, and records completed cases immediately. It does not hide the cold case or treat a slow proof as a deadline pass.

The userspace provisioner builds both client binaries.

```bash
systemd-run --user --scope --quiet \
  --property=MemoryMax=14G --property=MemorySwapMax=32G \
  "$SERVER_PREFIX/bin/with-gpu-server" \
  "$ICICLE_PREFIX/target/release/northstar-proof-corpus" \
  "$PWD/northstar/zkvm-replay/g5-corpus-v1.json" "$NEW_RESULTS_DIRECTORY"
```

The conservative recipe uses the recommended patch, not the slower experimental cold-init profile in the retained corpus run.

## Retained evidence and checks

- `corpus.json`, `corpus/*`: all measurements, witness/proof hashes, SDK artifacts, 356-byte on-chain proofs, and 256-byte public inputs.
- `baseline-released/*`: all three SDK/on-chain artifacts from the recommended profile and their measurements; these three proofs also passed Portal verification.
- `experiments.json`: successful and failed tuning profiles, resource limits, measured peaks, environment overrides, and binary hashes.
- `hardware.json`, `go-heap-evidence.txt`, `patches.json`: hardware/capacity observations, peak live-heap GC records, and source patch identities.
- `portal-verification.json`: 14 corpus acceptance results, three recommended-profile results, and both negative controls.
- `validation.json`: four trace-count unit tests, provisioning guards, the CMake regression, shell syntax, formatting, and focused CUDA-client clippy all passed. Full `cargo clippy --all --tests` remained incomplete after its 590-second compilation timeout.

No production acceptance flags were enabled, and no commits were created for this work.
