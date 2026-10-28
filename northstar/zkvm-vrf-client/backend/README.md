# SP1 6.8.0 backend experiments

These patches do not establish fresh-session or production acceptance. The supported-geometry filtered reader produced real SDK-verified proofs. Initialization overlap regressed; the larger-shard experiment failed the recursion verifying-key allowlist. **Do not disable that check.** See [results](../../docs/vrf-proof-time-followup.md).

## Pinned baseline

Use a fresh external checkout of [SP1 `58c4aeadbc504c274dd9fb82ed8130d0939ab756`](https://github.com/succinctlabs/sp1/tree/58c4aeadbc504c274dd9fb82ed8130d0939ab756). SP1 is MIT/Apache-2.0 licensed. `sp1-baseline-58c4aead.patch` preserves the pre-existing measured nonce/grinding, trace-release and low-memory backend changes; it is **not** a newly measured improvement in this follow-up. Its SHA-256 is `16773cd1a50fbcc0819b21bad5d30cfaa7cfb6ab3992f4bbfd0213c0ba9a3f1c`.

The baseline Go module uses gnark v0.14.0 with the pinned `p4u/gnark` replacement `cd7874155e26`, gnark-crypto `022ec58e8c19` and icicle-gnark/v3 v3.2.2. The actual `go.mod` SHA-256 is `b1286654011c91fadc9178174fdd3e2475bcfe37d36b9045d79ff3e775cd7d39`; `go mod verify` passed. Preserve `Cargo.lock`, `go.mod` and `go.sum`.

Prerequisites: installed Rust/CUDA/Go toolchains and compatible ICICLE headers/libraries. The measured build used Rust 1.98.1, CUDA architecture 86, two build jobs and no native-CPU Rust flags. Do not substitute unrelated prebuilt servers or libraries. Configure their compiler/linker/runtime paths explicitly; no installer is provided.

## Filtered-reader build

`SP1_SRC` is the fresh external checkout; `NORTHSTAR_SRC` is this repository. Do not apply these patches to a running or previously measured source tree.

```sh
backend="$NORTHSTAR_SRC/northstar/zkvm-vrf-client/backend"
git -C "$SP1_SRC" apply "$backend/sp1-baseline-58c4aead.patch"
git -C "$SP1_SRC" apply "$backend/../tools/sp1-server-timing.patch"
cp "$backend"/r1cs_reader{,_test}.go "$SP1_SRC/crates/recursion/gnark-ffi/go/sp1/"
git -C "$SP1_SRC" apply "$backend/r1cs-reader.patch"

cd "$SP1_SRC/crates/recursion/gnark-ffi/go"
go test -tags icicle sp1/r1cs_reader.go sp1/r1cs_reader_test.go -run '^TestR1CSReader' -count=1
SP1_R1CS_TEST_ARTIFACT="$CANONICAL_GROTH16_R1CS" \
  go test -tags icicle sp1/r1cs_reader.go sp1/r1cs_reader_test.go \
  -run '^TestR1CSReaderPinnedArtifact$' -count=1

cd "$SP1_SRC"
CUDA_ARCHS=86 CARGO_BUILD_JOBS=2 cargo +1.98.1 build \
  --release --locked -j 2 -p sp1-gpu-server --features groth16-cuda
```

The file-specific Go tests avoid unrelated native C exports required by whole-package linking. The actual Rust/CUDA FFI build is still required.

Enable `SP1_GROTH16_FILTER_DIAGNOSTICS=1` at runtime. The reader validates the pinned binary envelope, substitutes nil only for CBOR `DebugInfo`/`MDebug`, and preserves all constraint sections and other fields. The canonical artifact is never rewritten. Filtering and deserialization remain **inside prove+wrap**.

The real v6.1.0 artifact has 15,972,262 constraints and 2,437,991,441 bytes. Full normalized serialization matched the ordinary reader with diagnostics removed: SHA-256 `1e715475fc2a159f90f45b7ba18085e9acc9f0163f64faa74e19c07cd807054a`. Tests also cover malformed/truncated/layout input, source immutability and valid/invalid witness behavior.

## Optional diagnostic instrumentation

`SP1_TIMING_SPANS=1` emits span-close events. The default logger is preserved otherwise. Summarize retained logs without summing overlapping stages:

```sh
python3 "$backend/../tools/summarize-timing.py" \
  --log "$RUN_LOG" --measurements "$SDK_MEASUREMENTS" --out "$TIMING_SUMMARY"
```

The summarizer cannot grant acceptance. It retains failed/incomplete attempts and hashes its inputs. Changing the guest to add cycle instrumentation requires a separate diagnostic ELF/key; such samples cannot qualify the release artifact.

## Unselected experiments

**Initialization overlap:** apply `sp1-groth16-preload.patch`, copy `groth16_preload*.go` into the Go `sp1/` directory and `preload_export.go` into its parent. Run the file-specific `groth16_preload` and `groth16_preload_unavailable` tests, then rebuild the CUDA server. `SP1_GROTH16_OVERLAP_INIT=1` starts mutex-protected artifact loading during the existing prove call and joins it before returning. `SP1_GROTH16_OVERLAP_COMPONENT=all` loads circuit then key; `key` loads only the key. Empty directories, unknown components and loader failures return errors. No initialization cost is moved before the authoritative clock. Both measured modes caused paging and regressed.

**Shard size:** stock `local_gpu_opts()` overwrites `ELEMENT_THRESHOLD`. Merely setting that environment variable did not enlarge GPU shards. `sp1-gpu-shard-threshold.patch` adds an explicit, logged, full-size-only diagnostic override. The tested 469,762,048-element configuration reached `vk not allowed` in `RecursionVks::open`; the client subsequently reported `artifact not found`. CPU preflight passed; no proof was generated. The 536,870,912 alternative was not attempted. The patch is retained to reproduce the unsupported configuration, not as a recommended optimization.

**Fixed PK:** the second repeated-input proof stalled after an executor-cache hit and zero core shards. It was stopped; the third was not attempted. Keep `SP1_WORKER_USE_FIXED_PK=false`.

No experiment changes guest authentication, public fields, SDK verification, canonical recursion-key checks or Portal release keys. The historical ICICLE AR1/Xid31 failure is not demonstrated fixed by these results.
