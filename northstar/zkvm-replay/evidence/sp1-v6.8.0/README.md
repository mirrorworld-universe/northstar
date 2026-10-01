# SP1 6.8.0 upgrade evidence

Generated on 2026-09-21 with SP1 SDK/GPU server 6.8.0, host Rust 1.98.1, guest Rust 1.93.0-dev (succinct), and an NVIDIA L40S. Server source tag `v6.8.0` resolves to `58c4aeadbc504c274dd9fb82ed8130d0939ab756`; Groth16 CUDA uses Icicle 3.2.2.

- `candidate.elf.gz`: rebuilt, pinned guest; uncompressed SHA-256 `484af2d12fb26d8b6a70014b2d209d7cb474ea32d66eed0d273036914650e033`.
- Program key: `0x00737c84091d0722e0884993f9e25b8c0f504636777329cbaeb36840933fa2a5`.
- `key.json`: GPU setup; `execute.json`: guest execution against `../../fixture-v1.bin`.
- `measurements.json` and adjacent proof/public-input files: warm-cache baseline proof.
- `cold/`: first proof, including fresh circuit download/initialization overhead.
- `resolver/`: fresh proof for the unchanged `../resolver-v1/witness-v2.bin` and authenticated account snapshot. Public inputs match the historical resolver artifact byte-for-byte.

| Check | Result |
|---|---:|
| Guest execution | 23,798,044 cycles / 883 ms |
| Cold Groth16 generation and wrapping | 184,969 ms |
| Warm baseline generation and wrapping | 42,795 ms |
| Resolver generation and wrapping | 40,728 ms |
| Standalone Portal SBF verification | 97,113 CU |

Both warm proofs passed SP1 verification. Portal SBF tests on Agave 4.4.0-alpha.5 accept the current proof, reject changed fields and previous-program proofs, and pass full `ResolveChallenge` state-transition/rejection checks within a 130,000-CU transaction budget. The separately enabled checkpoint-proof test also passes.

The Groth16 wrapper key and recursion root remain byte-identical to SP1 6.1.0. The guest program key changes: old proof evidence remains historical and is no longer accepted by the current Portal prototype. Default builds still fail closed; `production_acceptance` remains false. These are compatibility measurements, not live-validator or deadline-acceptance results.

Regenerate guest proofs with the pinned ELF, `SP1_SKIP_PROGRAM_BUILD=true`, and the isolated SP1 6.8.0 GPU server provisioned by `northstar/scripts/provision-gpu-server.sh`. SDK artifacts are produced by the replay script's `key`, `execute`, and `groth16` commands. Provisioning verifies the ELF digest before compiling the host client.
