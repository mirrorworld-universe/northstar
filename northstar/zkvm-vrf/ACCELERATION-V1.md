# Experimental SHA-256 implementation variant: sha2-sp1-v1

This is a separately built implementation variant of the bounded actual SBPF replay candidate, not a new application relation. It is not enabled in the native/root workspace or legacy guest workspace. Do not replace a configured Portal key without fresh SDK execution, proof verification and explicit development configuration.

## Scope and dependency pin

The workspace patch redirects compatible registry SHA2 0.10 consumers to official SHA2 **0.10.9** at **`e48b656ebc806117554bb33c2f8687e4637e37ff`**:

- [Manifest](https://github.com/sp1-patches/RustCrypto-hashes/blob/e48b656ebc806117554bb33c2f8687e4637e37ff/sha2/Cargo.toml).
- [SHA-256 dispatch](https://github.com/sp1-patches/RustCrypto-hashes/blob/e48b656ebc806117554bb33c2f8687e4637e37ff/sha2/src/sha256.rs).
- [Succinct implementation](https://github.com/sp1-patches/RustCrypto-hashes/blob/e48b656ebc806117554bb33c2f8687e4637e37ff/sha2/src/sha256/succinct.rs).

Only `target_os="zkvm", target_vendor="succinct"` selects its SP1 SHA-256 extend/compress syscalls. The patch already served the binding crate's target-specific hashes in the baseline. This variant additionally redirects the remaining registry 0.10.9 consumers, including applicable account/program hashing and runtime hash syscalls. SHA-512 is not accelerated by this patch. Native/non-Succinct dispatch stays native/software. SHA2 0.9.9/0.11.0, curve25519-dalek 4.1.3/5.0.0 and ed25519-dalek 3.0.0 remain unchanged. SP1 libraries stay locked at 6.8.0.

No application ELF, native engine source, instruction policy, curve/CPI implementation, witness encoding or public-field order changes. There is no supplied digest or effect oracle. The actual Agave interpreter still executes the pinned SBPF, System/PDA-signed CPIs, curve operations and return-data behavior. The precompile implements the same mathematical SHA-256 compression function inside the SP1 execution/proof system.

## Fixed statement identity

- Candidate proof kind **3**, version **1**; eight ordered fields / **256 bytes** unchanged.
- Typed policy SHA-256 **`c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d`**.
- Exactly **one checkpoint step**, effective admission age **900**.
- Same zero-fee, curve-only, 32-KiB-heap development profile.
- No production acceptance or cross-layer settlement is introduced.

The compiled typed-policy digest is **not** the deployment JSON file hash. Deployment JSON includes the resulting ELF hash and must be regenerated for every distinct ELF. SDK program keys must be derived from the actual new ELF, never copied from the baseline or another diagnostic build.

## Build options and prerequisites

Use a separate source tree and fresh output directories. Preserve the baseline source/ELF/key/proof records. Prerequisites are host Cargo, the approved Succinct RV64IM compiler, cargo-prove **6.8.0**, Python 3, and either a RISC-V C compiler or clang. This recipe installs nothing and uses two build jobs. The parent controls remote builds and proving.

```sh
export NORTHSTAR_VRF_BUILD_VARIANT=sha2-sp1-v1
export NORTHSTAR_VRF_PROFILE_SHA256=c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d
# If the guest compiler is not already on PATH:
export NORTHSTAR_GUEST_COMPILER_BIN=/absolute/path/to/approved/guest/compiler/bin

# Regular candidate, used for performance and any eventual development key.
NORTHSTAR_VRF_DIAGNOSTIC=none bash northstar/zkvm-vrf/build-accelerated-guest.sh /fresh/absolute/regular-output

# Separate diagnostic ELF: known-answer checks, then the real supplied envelope.
NORTHSTAR_VRF_DIAGNOSTIC=crypto bash northstar/zkvm-vrf/build-accelerated-guest.sh /fresh/absolute/crypto-output
```

Optional `cycles` or `all` diagnostics add cycle trackers around policy derivation, envelope decoding, verification and the actual interpreter call. They delegate unchanged to the actual engine. Retain the SDK cycle-tracker report when using them. Diagnostic ELFs are distinct builds/keys; do not mix their measurements or receipts with the regular ELF.

C compilation is forced to **`-march=rv64im -mabi=lp64 -mno-relax`**, plus the RISC-V target for clang. Affected packages, including `blst`, `blstrs` and `blake3`, are cleaned in the **release profile and actual guest target directory** before building. This avoids stale epoch-mtime metadata and compressed C instructions. Both metadata/build use the frozen lockfile. The final header guard requires ELF64 little-endian RISC-V (**machine 243, flags 0**). Header screening is not SDK disassembly or execution evidence.

The recipe emits complete build logs/status, Cargo metadata, ELF hashes and `build-variant.json`. These are build receipts only; no proof or Portal key is installed.

## Validation and acceptance boundary

Native diagnostics compare the pinned implementation against fixed SHA-256 known answers, incremental/padding boundaries, all eight byte alignments, raw compression and empty-block behavior, plus independent registry SHA2 0.11 and the fixed typed-policy digest:

```sh
RUSTUP_AUTO_INSTALL=0 cargo test --locked --manifest-path northstar/zkvm-vrf/Cargo.toml \
  -p northstar-zkvm-vrf-shared --features crypto-checks,cycle-profiling --lib
```

Native tests exercise the native dispatch, **not** the Succinct precompile branch. The diagnostic guest must execute these checks through the real SDK, then execute a genuine canonical envelope and match all native public bytes. The regular guest must independently reproduce the real request and distinct fulfillment/callback cases, reject the authenticated mutation corpus, and preserve failure/rollback behavior before new-key selection. Actual CUDA measurements and verification on **g5.xlarge/A10G** follow; no speedup follows from this source patch alone. Repeating one input is characterization, not three distinct acceptance cases. Human acceptance and independent reproduction remain separate requirements.
