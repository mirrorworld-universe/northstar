# G5 SHA-precompile candidate

Measured on 2026-10-21 on the same **g5.xlarge: 4 vCPUs, 15.4 GiB usable RAM, A10G / 23,028 MiB VRAM**, NVIDIA 595.91.07, CUDA 13.1.115, SP1 6.8.0.

## Result

The retained corpus meets the **37–49 second warm-proof target**. Times include proof generation, Groth16 wrapping, and SDK verification:

| Measurement | Result |
|---|---:|
| 13 warm corpus cases | **40.996–42.732 s** |
| Separate warm baseline repetitions, final client | **41.664, 42.453 s** |
| Warm corpus median | **41.997 s** |
| Previous guest/profile warm median | 80.233 s |
| Median speedup | **1.91×** |
| First, process-cold proof | **93.329 s** |
| SDK initialization, measured separately | 16.701 s |
| Observed proof-cgroup RAM peak | 13.227 GiB |
| Observed proof-cgroup swap peak | **0 bytes** |
| Portal verification | **84,079–84,081 CU** |

"Cold" means a new client/server process with circuit artifacts already downloaded and checksum-verified on local NVMe. It does not include downloading artifacts or provisioning the machine. Warm cases reuse one client and server, with **one in-flight proof and no concurrent submissions**.

All **14 distinct retained cases** have warm proof evidence below 49 seconds and passed native replay, SDK proof verification, and the checkpoint-bound Portal SBF verifier. Their 256-byte public-input artifacts are byte-identical to the [previous corpus](../g5-a10g-v1/corpus.json). All 28 one-bit proof/public-input controls were rejected. The candidate verifier also rejects the previous guest key. Three additional baseline proofs from the final rebuilt client passed SDK and Portal verification.

This is the retained supported replay relation, **not arbitrary SVM execution or demonstrated validity of every transaction in a full 16-transaction checkpoint**. `production_acceptance` remains **false**. No on-chain deployment or live coordinator acceptance is claimed.

## Changes

1. **Guest SHA-256 precompile.** Use the pinned official SP1-patched `sha2` on zkVM while retaining native `sha2` off-zkVM. Baseline guest execution fell from **23,798,044 to 5,851,015 instruction cycles**; trace hashing fell from 18,643,393 to 1,823,082 cycles. The guest compiler is `succinct-1.96.0-64bit-v3`.
2. **Host-memory profile.** Retain the one-buffer SP1 configuration and CUDA-pool release. Skip the proving key's optional CPU FFT tables, remove only R1CS diagnostic metadata, and explicitly release unused Go heap around Groth16 work. The successful combination uses `GOMEMLIMIT=11GiB`; individual earlier experiments were not uniformly faster.
3. **BN254 proof-of-work parallelism.** Upstream launches one 512-thread CUDA block. The candidate uses 128 blocks, a correctly sized 32-bit flag initialized before launch, and one atomic result publisher. Challenge difficulty and CPU `check_witness`/transcript advancement are unchanged. Transcript-equivalence tests passed at both launch sizes, including zero-bit simultaneous-success cases and 20-bit searches; CUDA memory checking reported zero errors.
4. **Candidate verifier binding.** The guest key changed to the value in [the candidate manifest](../../partial-candidate-v1.json). Portal's guest constant **and pre-folded `FIXED_INPUTS_IC` point** were updated together. The original Groth16 key, recursion root, 256-byte ABI, proof envelope, and verification rules are unchanged. The fixed-input/full-key equivalence test guards the specialization.

Signature binding, instruction binding, and complete witness encoding were checked in both native replay and the guest. Rejected guest executions must report a nonzero exit code and produce no public output; a returned execution report alone does not imply acceptance.

## Reproduce

Use fresh absolute installation prefixes, an A10G-class GPU, installed driver/CUDA/build prerequisites, and local NVMe for circuit storage. The provisioners **do not format disks**. A 32 GiB swap allowance remains configured as a safety margin even though this corpus did not use it. Keep zswap disabled, `vm.page-cluster=0`, and `vm.swappiness=60`.

From the repository root:

```bash
export CUDA_ARCHS=86 CARGO_BUILD_JOBS=2 CMAKE_BUILD_PARALLEL_LEVEL=2
export NORTHSTAR_GPU_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_DEBUG=0
bash northstar/scripts/provision-gpu-userspace.sh "$ICICLE_PREFIX"
NORTHSTAR_GPU_G5_OPTIMIZED=1 bash northstar/scripts/provision-gpu-server.sh \
  "$ICICLE_PREFIX" "$SERVER_PREFIX"

set -a
source northstar/scripts/gpu-g5-sha.env
set +a
```

The [optimized patch](../../../scripts/patches/sp1-v6.8.0-g5-optimized.patch) applies to SP1 commit `58c4aeadbc504c274dd9fb82ed8130d0939ab756`. Provisioning runs the Go domain/diagnostic invariants and GPU transcript checks before marking the server ready. Without `SP1_GPU_BN254_GRIND_BLOCKS`, the new kernel retains the upstream one-block default.

Keep the private server's `.sp1/circuits` directory on the verified NVMe cache. Run the [corpus driver](../../script/src/bin/northstar-proof-corpus.rs) with a new results directory, shared device lock, and the measured memory budget:

```bash
(
  exec 9>"$GPU_LOCK_FILE"
  flock --nonblock 9
  unit="northstar-g5-candidate-$$"
  trap 'status=$?; systemctl --user stop "$unit.scope" >/dev/null 2>&1 || true; exit "$status"' EXIT
  systemd-run --user --scope --quiet --unit="$unit" \
    --property=MemoryMax=14G --property=MemorySwapMax=32G \
    "$SERVER_PREFIX/bin/with-gpu-server" \
    "$ICICLE_PREFIX/target/release/northstar-proof-corpus" \
    "$PWD/northstar/zkvm-replay/g5-corpus-v1.json" "$NEW_RESULTS_DIRECTORY"
)
```

All launch paths sharing the GPU must use the same lock. Do not submit concurrent requests inside the client. A persistent worker/coordinator deployment is separate validation; no deadline or verifier guard was relaxed for these measurements.

## Evidence

- [corpus.json](corpus.json): per-case timings, historical-input equality, artifact hashes, runtime overrides, resource peaks, client/server hashes, and source-patch hash.
- [portal-verification.json](portal-verification.json): all 14 proof pairs, 28 mutation controls, compute units, and exact verifier/harness hashes.
- [portal-pilot.json](portal-pilot.json): candidate acceptance and previous-guest rejection with the same verifier binary.
- [final-client.json](final-client.json) and [final-client-portal.json](final-client-portal.json): three final-client baseline proofs, including two warm repetitions; artifacts are in `final-client/`.
- [validation.json](validation.json): formatting, full workspace/focused CUDA clippy, unit tests, GPU transcript checks, and memory checking.
- [guest-invariants.json](guest-invariants.json): valid-input equivalence and three native/guest rejection controls.
- [candidate.elf.gz](candidate.elf.gz): retained guest, checked against the manifest's ELF hash.
- `corpus/<label>/`: SDK proof, on-chain proof, public inputs, and per-case measurements.

The root-level proof pair and `measurements.json` retain the earlier SHA-candidate pilot, not the recommended profile's timing result. Use `corpus.json` for the optimized corpus measurements.
