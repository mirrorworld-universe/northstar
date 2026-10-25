# Bounded-VRF proof-time follow-up

## Result: target not met

Best verified diagnostic: **142137 ms prove+wrap**, **490 ms SDK verification**, **165968 ms SDK wall time**. Target remains **120000 ms across three genuinely fresh configured-session cases**. This work completes **zero** such cases and no new Portal resolutions.

All follow-up proofs reuse the historical request from the [v11 evidence](../zkvm-vrf/evidence/g5-a10g-v11/README.md). The new guest has a genuinely derived, separately packaged [development key](../programs/portal/keys/proof-time-wordwise-v3/README.md); it is not installed. No release key, proof relation, public ABI, historical acceptance gate or production setting changed.

[Machine-readable attempts](../zkvm-vrf/evidence/proof-time-v1/attempts.json) retain 16 diagnostic runs: 15 SDK-verified proofs, an interrupted second fixed-PK attempt and one larger-shard configuration failure. The third fixed-PK sample was not attempted. [Selected proof bytes, controls and timings](../zkvm-vrf/evidence/proof-time-v1/README.md) are published separately from private operational logs.

## What changed

- Canonical Borsh validation compares streamed encoding against the original bytes instead of allocating another envelope-sized buffer.
- Domain-separated binding hashes stream the identical encoding with its existing u64 length prefix. Prestate hashing is delayed only until the existing branch needs it; authentication still runs.
- zkVM byte comparisons use exact aligned-word comparisons, including guarded reconstruction of misaligned words. Native builds retain slice equality. Account equality checks every field, with exhaustive destructuring.
- The outer reader removes only diagnostic CBOR values before loading the pinned R1CS. Full normalized constraint serialization matches the ordinary reader after identical diagnostic removal.
- Optional timing instrumentation and a tested summarizer expose overlapping stages without treating them as additive.

The actual SBPF interpreter relation, strict signatures, roots/absence proofs, CPI/return-producer binding, feature/sysvar/fee checks, rollback and final output equality remain. Policy `c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d`, eight public fields/256 bytes, one checkpoint, age 900 and the 149-byte session format are unchanged. Ordinary validator JIT and existing Portal selection/legacy code were not changed.

## Costs and cache boundaries

Hardware: **g5.xlarge, A10G 23028 MiB**, four logical CPUs on two AMD EPYC cores, approximately 14.6 GiB RAM and 32 GiB swap. Prover scope: 14 GiB RAM/32 GiB swap. **Not L40S.** No new hardware or paid proving was used.

Milliseconds below. Every completed row is a process-cold, old-request diagnostic. Circuit artifacts were already present; filesystem caches were not flushed. INFO logging, optional span events and one-second sampling add diagnostic overhead. Prove+wrap retains the original SDK clock; initialization/setup, CPU preflight and SDK verification are separate. No expensive initialization was moved before that clock.

| Configuration | SDK init | Setup | CPU | Prove+wrap | Verify | SDK wall |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Original guest/backend characterization | 901 | 18323 | 4355 | 249138 | 488 | 273412 |
| Timing instrumentation | 743 | 18396 | 4340 | 255389 | 488 | 279569 |
| Two-worker pipeline | 774 | 18337 | 4370 | 210670 | 487 | 234849 |
| Fixed PK, first proof only | 847 | 18408 | 4360 | 220129 | 491 | incomplete |
| Filtered R1CS reader | 840 | 18364 | 4323 | 198008 | 489 | 222239 |
| Full-size shards/four core workers | 848 | 19156 | 4353 | 171550 | 489 | 196609 |
| Streamed guest/key | 877 | 19113 | 3992 | 174653 | 490 | 199304 |
| Wordwise v2 guest/key | 849 | 18933 | 3557 | 156890 | 488 | 180894 |
| v2, 16M trace chunks | 873 | 18919 | 3586 | 148086 | 489 | 172130 |
| v2, native CPU ISA | 844 | 17798 | 3547 | 153500 | 500 | 176939 |
| v2, two core workers | 877 | 18956 | 3581 | 157674 | 493 | 181760 |
| v2, full outer initialization overlap | 838 | 18753 | 3564 | 208750 | 508 | 233311 |
| v3, adjacent aligned loads | 1048 | 18994 | 3435 | 146677 | 488 | 170816 |
| v3, repeated effective GPU geometry | 846 | 18871 | 3445 | **142137** | 490 | 165968 |
| v3, key-only overlap | 838 | 18678 | 3422 | 168683 | 500 | 192979 |

The 142137-ms run requested `ELEMENT_THRESHOLD=469762048`, but the pinned GPU builder overwrites that setting with its 402653184-element threshold. It still emitted 17 core shards. The difference from 146677 ms is **not evidence of a larger-shard improvement or a repeatable statistical gain**.

The original genuinely fresh v11 batch remains **245493 / 204670 / 210837 ms**, with complete Production resolutions **111253 / 109753 / 111253 CU**. Those results concern the original configured key, not this follow-up key.

## Attribution and blockers

The best sample used 48487671 guest cycles. Its non-additive span unions include 67.282 s core setup/proving, 67.145 s normalization, 14.955 s record conversion and 47.500 s final Groth16. These overlap; do not sum them. Native outer loading took 13.553 s for R1CS and 14.407 s for the key; native Groth16 generation took 17.358 s.

Sampled GPU peak: **19035 MiB**. Scope memory peak: **14651236352 bytes**. Sampled swap: **zero**. Per-run memory records are in `attempts.json`.

- Full initialization overlap ran inside proving but reached the 14 GiB cap and **3391340544 bytes sampled swap**. It regressed to 208750 ms.
- Key-only overlap likewise paged **1801310208 bytes**, regressing to 168683 ms. Final Groth16 grew to 61.600 s despite preloading the key.
- An explicit GPU threshold override did select 469762048 elements, then hit [`RecursionVks::open`'s canonical key lookup](https://github.com/succinctlabs/sp1/blob/58c4aeadbc504c274dd9fb82ed8130d0939ab756/crates/prover/src/recursion.rs#L143-L158): **`vk not allowed`**. The SDK subsequently reported `artifact not found`. CPU preflight passed; no proof was produced. Verification was not disabled and larger values were not attempted.
- Fixed-PK reuse stalled on the second request after an executor-cache hit and zero core shards. No completed warm result exists.
- The historical ICICLE AR1/Xid31 failure remains retained. Subsequent verified proofs are not a demonstrated fix for it.

These establish practical host-memory and supported-geometry limits for the tested configurations, **not an absolute proof that 120 seconds is impossible**. Next decision: authorize measurement on a higher-CPU/RAM A10G configuration, or scope further low-level host-memory work. Changing the canonical recursion key set/circuit requires separate review; bypassing its checks is not an option.

## Validation and reproduction

Current frozen guest source passed:

- Nightly workspace formatting and `cargo clippy --all --tests`.
- Locked `northstar-transaction-proof --no-default-features` tests, replay unit tests and replay CLI builds.
- **22 native differential inputs:** seven positives and fifteen rejections.
- **Three complete source/session associations** and nine source-record mutation rejections.
- **17 actual guest controls:** two exact 256-byte positives and fifteen nonzero-exit/empty-output negatives.
- Both valid failed-transaction/rollback witnesses with exact expected outputs.
- Pinned regular ELF build with RISC-V machine 243/flags zero, genuine CPU/CUDA setup agreement and actual SDK proof verification.
- Reader guards and real 15972262-constraint normalized-serialization equivalence; deterministic development-key regeneration; six timing-summarizer tests.

See [backend build/test instructions](../zkvm-vrf-client/backend/README.md). The existing [guest build recipe](../zkvm-vrf/build-accelerated-guest.sh) requires explicit `sha2-sp1-v1`, the fixed policy digest and diagnostic mode `none`; use installed tools and a fresh output directory. Do not inherit another worktree's `CARGO_TARGET_DIR`. Build paths can affect ELF bytes: derive a genuine new key for any changed ELF rather than relabeling a receipt. The [client instructions](../zkvm-vrf-client/README.md) define genuine setup, controls and proof clocks.

Native validation can be repeated from this worktree with an explicit target directory:

```sh
export CARGO_TARGET_DIR="$PWD/target"
cargo +1.98.1 test --locked -p northstar-transaction-proof --no-default-features
cargo clippy --all --tests
cargo +nightly fmt --all
python3 -m unittest discover -s northstar/zkvm-vrf-client/tools -p 'test_summarize_timing.py'
python3 northstar/zkvm-vrf-client/tools/check-proof-time-evidence.py
```

Compiler/setup interruptions and unsuccessful build-driver configurations remain retained; they were not counted as guest rejections. No new configured Portal SBF, fresh hard window, three fresh proofs, whole-resolution CU results, independent human reproduction, L40S equivalence or hosted/production acceptance is claimed. No commit, push or follow-up PR was automatically made.
