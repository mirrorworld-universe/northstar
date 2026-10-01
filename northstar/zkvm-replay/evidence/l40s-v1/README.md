# Retained L40S proof artifacts

These are public proof/witness artifacts, not signer keypairs. `SHA256SUMS` authenticates file integrity; acceptance scope and timings are described in [GPU validation](../../../docs/gpu-proof-validation-v1.md).

- `compatibility-02`: first cold, successful proof of the preserved fixture.
- `compatibility-03`: warm compatibility proof.
- `live-01` through `live-03`: three fresh 16-step/four-round isolated-transaction proofs.
- `live-partial`: three-step checkpoint, selected transaction 2.
- `live-guards`: additional fresh canonical run after adding measurement-pin and bond/cursor conservation assertions.
- `candidate.elf.gz`: preserved guest executable matching `partial-candidate-v1.json`. Decompressed SHA-256: `23f1520bf46ab8852770c0f4802005c8cb88b7fa6657f99f2d7c4a948d1551c5`.

Each proof directory retains the SDK container, 356-byte on-chain envelope, 256-byte public inputs, and SDK measurements. Live directories also retain the exact replay witness and extracted timing events. The optional `resolution.json` records post-hardening assertions' run. Earlier timing records preserve `resolved: false` at witness extraction; the later resolution event reports the actual terminal challenge outcome. Settlement and crash recovery are not established by these records.

Verify files from this directory:

```sh
sha256sum -c SHA256SUMS
```

Verify on CPU against real Portal SBF, from repository root:

```sh
cargo build-sbf --manifest-path northstar/programs/portal/Cargo.toml --features zk-verifier-prototype -- --locked
BPF_OUT_DIR="$PWD/target/deploy" cargo test --locked -p northstar-portal --features zk-verifier-prototype --test zk_verifier
# Restore the default fail-closed program before default integration tests.
cargo build-sbf --manifest-path northstar/programs/portal/Cargo.toml -- --locked
```

To embed the preserved guest instead of producing a path-dependent new key, from `northstar/zkvm-replay`, using the default target directory:

```sh
mkdir -p target/elf-compilation/riscv64im-succinct-zkvm-elf/release
gzip -dc evidence/l40s-v1/candidate.elf.gz > target/elf-compilation/riscv64im-succinct-zkvm-elf/release/northstar-zkvm-replay-program
cargo clean --release -p northstar-zkvm-replay-script
SP1_SKIP_PROGRAM_BUILD=true cargo build --release --locked -p northstar-zkvm-replay-script --features cuda
```

Check the embedded key before proving. With custom `CARGO_TARGET_DIR`, use that directory instead. GPU generation additionally requires SP1 6.1.0 GPU-server runtime dependencies and Icicle libraries; this artifact distribution does not make clean source builds reproducible. No paid proving service is required or authorized by these instructions.
