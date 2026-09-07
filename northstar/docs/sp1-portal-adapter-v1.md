# SP1 Groth16 Portal adapter v1

Status: **direct adapter selected; compatibility prototype passes the preliminary CU gate**.

Portal can verify the SP1 6.1.0 Groth16 wrapper directly. A second outer circuit is not needed: Portal validates the frozen eight-field proof ABI, hashes its exact 256-byte encoding as SP1 does, reconstructs SP1's five outer public inputs, and verifies the wrapper proof with the pinned SP1 key.

## Proof layout

`SP1ProofWithPublicValues::bytes()` produces this 356-byte envelope:

| Offset | Length | Value |
|---:|---:|---|
| 0 | 4 | First four bytes of SHA-256 of the SP1 Groth16 verifying key |
| 4 | 32 | Exit code; v1 requires zero |
| 36 | 32 | SP1 recursion verifying-key root |
| 68 | 32 | Proof nonce, canonical BN254 Fr encoding |
| 100 | 64 | Gnark uncompressed proof A |
| 164 | 128 | Gnark uncompressed proof B |
| 292 | 64 | Gnark uncompressed proof C |

SP1 emits non-negated A. `groth16-solana` uses the standard pairing equation with negated A, so Portal negates A before invoking the verifier.

The instruction contains the 356-byte proof followed by the frozen 256-byte Portal public inputs. Including the one-byte instruction selector, instruction data is 613 bytes. A signed standalone legacy transaction is 783 bytes, below Solana's 1,232-byte limit.

## Verifying-key binding

The adapter binds both layers of SP1 verification:

- SP1 6.1.0 Groth16 key SHA-256: `4388a21c687fdd5f218d7e3d13190cac4c5355818d3605fd5fb811df468ee696`.
- Required proof prefix: `4388a21c`.
- SP1 recursion key root: `002f850ee998974d6cc00e50cd0814b098c05bfade466d28573240d057f25352`.
- Northstar replay program key hash: `0x0096a4f4437019c7c9c851edd1daa60cd5ce751b36ee0e2ab0fe280a88a38693`.

The compressed 492-byte SP1 key is retained in `programs/portal/keys/`. Portal embeds its converted 832-byte Solana verifier points in executable read-only data. Proof key prefix, exit code, recursion root, and all scalar encodings fail closed before pairing.

## Public-input mapping

Portal first parses all eight 32-byte big-endian values as canonical BN254 Fr elements and requires proof kind 2/version 1 through the domain field. Non-position commitments must be nonzero. Their order remains unchanged:

1. `domain`
2. `session_context`
3. `slot_step`
4. `pre_state_root`
5. `post_state_root`
6. `tx_effect_root`
7. `readonly_l1_root`
8. `settlement_effect_root`

The exact concatenated 256 bytes are SHA-256 hashed and the top three bits are cleared, matching `sp1-verifier::hash_public_inputs`. Portal then verifies SP1's five outer fields:

1. pinned Northstar replay program key hash;
2. hash of the exact eight-field Portal ABI;
3. zero exit code;
4. pinned SP1 recursion key root;
5. proof nonce from the authenticated envelope.

This keeps the frozen Portal ABI while adapting it to SP1's wrapper relation.

## Measurements

Measured on 2026-09-07 against the current Agave SBF runtime:

| Item | Result |
|---|---:|
| Northstar SP1 setup on Ryzen 7 7840U | 55,524 ms |
| Portal instruction data | 613 bytes |
| Signed standalone transaction | 783 bytes |
| Feature-enabled Portal SBF ELF | 359,080 bytes |
| Full five-input MSM + pairing path | 97,156 CU |
| Invalid SP1 key prefix rejection | 428 CU |
| Noncanonical Portal field rejection | 675 CU |
| Sealed step-proof capacity | 356 bytes |

The 97,156-CU run uses curve-valid Groth16 points from the existing benchmark corpus against the pinned SP1 key. It intentionally reaches all five scalar multiplications and the pairing syscall, then fails because it is not an SP1 proof. This is a preliminary verifier-path measurement, not a successful Northstar proof verification. It is 32,844 CU below the 130K target and 52,844 CU below the 150K stop threshold.

The direct verifier remains behind `zk-verifier-prototype` until an unchanged real proof passes the full resolver test. Feature-enabled builds route production `ResolveChallenge` through it; default builds fail closed with `StepProofVerifierUnavailable`. The dummy verifier still requires its explicit guarded test feature.

SP1 6.1.0 has CPU and CUDA proving backends but no AMD XDNA NPU backend. A canonical local Groth16 run on the Ryzen 7 7840U was stopped after 33 minutes without completing or emitting an artifact; it used all cores and increased memory pressure, so this machine is not a practical compatibility-proof source.

## Remaining compatibility check

Portal-side production challenge resolution now authenticates the isolated trace boundary and transaction/effect leaf, recomputes the canonical Poseidon `session_context`, reconstructs the eight public fields from the sealed account, and invokes the direct SP1 verifier. The replay relation still emits fixture-local Poseidon account-list commitments for state, readonly, transaction-effect, and settlement fields, while checkpoint v1 exposes projected SHA-256 state roots and global authenticated trees. A production proof therefore requires a checkpoint-binding witness revision before generating the final artifact.

The final compatibility check must:

- verify checkpoint trace, transaction-effect, readonly-L1, and settlement membership inside the replay relation;
- accept the resulting unchanged 356-byte Northstar proof through `ResolveChallenge`;
- reject changed raw proof bytes, paths, and each Portal public field;
- confirm successful-verification CU remains at or below 130K.

## Reproduction

```bash
cd northstar/zkvm-replay
cargo run -p northstar-zkvm-replay-script -- \
  key fixture-v1.bin /tmp/northstar-sp1-key.json baseline

cd ../..
cargo build-sbf \
  --manifest-path northstar/programs/portal/Cargo.toml \
  --features zk-verifier-prototype
BPF_OUT_DIR="$PWD/target/deploy" cargo test \
  -p northstar-portal \
  --features zk-verifier-prototype \
  --test zk_verifier -- --nocapture
```
