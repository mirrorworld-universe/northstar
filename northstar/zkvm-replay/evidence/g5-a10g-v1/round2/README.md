# Second-pass warm-latency investigation

Status: in progress. The previously validated released-server profile remains the recommendation. None of the experiments below is promoted, and production acceptance remains false.

## Target and invariants

The target is approximately 37–49 seconds per warm proof on the same g5.xlarge, compared with the retained corpus's 72.958–98.309 seconds including verification. Cold startup and initialization remain separately reported; moving work outside a timer does not count as a speedup.

Keep the proof relation, security parameters, witness checks, and verification intact. The experiments so far retain the existing guest ELF, verifying key, Groth16 circuit, and proof format. Re-run corpus verification and Portal checks before accepting any candidate.

## Completed experiments

All timings below include SDK verification. The first sample is a fresh-process sample, not a warm result. These fixture-only experiments do not establish corpus-wide performance.

| Experiment | First sample | Warm samples | Result |
| --- | ---: | ---: | --- |
| Released server, diagnostic logging | 168.699 s | 103.951 s, 80.086 s | Profiling reference; not an improvement |
| Skip CPU FFT precomputation while loading the GPU proving key | 206.519 s | 115.960 s, 103.626 s | Slower with the existing 12 GiB Go memory budget |
| Same cache change, two core workers | 127.482 s | 130.487 s, 123.046 s | Slower warm proofs |

The diagnostic reference's last warm sample spent approximately 2.379 seconds before core execution submission, 34.583 seconds between core execution and shrink/wrap submission, and 5.373 seconds before Groth16 submission. It generated six core proof tasks. The Groth16 solver alone reported 19.504 seconds. These are log-derived phase boundaries, not an exclusive CPU/GPU time decomposition.

The proving-key dump enables CPU FFT precomputation for a domain of 16,777,216 elements. An opt-in loader experiment clears only that serialized cache flag while preserving all domain field elements, the architecture marker, and key payload. Focused tests cover byte preservation, cached/uncached FFT equivalence, inverse FFT round trips, truncation, and invalid cache flags. The integrated Rust/Go server built and produced three SDK-verified proofs, but latency regressed; reduced allocation alone is not a demonstrated performance improvement.

Full Go package tests cannot link independently because the package expects Rust-provided field-inversion symbols. The focused tests compile the actual loader helper in isolation; the complete server build and proof runs cover its integration.

## Pending investigations

- Full-size GPU shards, with unchanged security parameters.
- Host-side SP1 profiling support to recover cycle-tracker labels from the existing ELF; no guest change is required for this diagnostic.
- Go CPU/heap profiles and a separately measured memory-budget experiment.

Do not infer a hardware limit or a 2× improvement from the current results.
