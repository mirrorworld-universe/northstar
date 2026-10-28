# Bounded actual-SBPF development verifier

`zk-verifier-bounded-vrf` enables a separate kind **3**, version **1** route. It implies `zk-verifier-prototype`; this is opt-in local development, not production rollout. Prototype-only/default builds reject kind3. Legacy kind2 retains its existing guest hash, fixed point and behavior. Instruction data cannot select an arbitrary guest key. The eight public fields/order/256-byte encoding and existing 149-byte session-context commitment are unchanged.

## Selected integrated v11 identity

The selected development guest key is **`0x00f8e1033f6031fff7297eadacd389d84d78850c1bda406914e771bb69282abe`** for regular guest ELF SHA-256 **`b0531ecbe24a339b60407673181274a83aeafeb2d3cc04057369ff5992829a92`**. The complete integrated source snapshot is **`dfc27caa9879799f3e656706bde02eaf617db4712ff12abb8c55db0e0d648297`**. The deployment profile records the semantics-preserving implementation variant and exact runtime/library identities; this is not a generic SVM claim.

Typed policy **`c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d`** pins checkpoint cardinality **1**, transaction admission age **900**, heap **32768**, and zero transaction fees. The typed policy hash is distinct from the deployment JSON hash, which includes the resulting ELF identity. Public provenance inputs are `bounded-vrf-deployment-v1.json`, `bounded-vrf-setup-v1.json` and `bounded-vrf-sp1-verifying-key.bin`.

A genuine SP1 6.8.0 SDK-verified v11 development proof/public pair is retained at `tests/zk_verifier/fixtures/bounded-vrf-integrated-v11/`. It uses an actual prior configured session, **not the final fresh three-case lifecycle window**. Process-cold prove+wrap took **282.566 s**, SDK verification **0.498 s**; the 120-second target is **false**. CPU setup/execution, valid failed-transaction controls, cryptographic proof verification and fresh Production resolutions are distinct evidence levels. Human acceptance is not inferred from agent tests.

The historical v7c key `0x001a26a674f911be746ffa5ea2dbdd412b0ee692bd34ebf2562b0b3bd25bd311`, ELF `23543b749b4a7c3f35b580695392768d30e6f2ef99e7d13b4f8fcb7bd4412b34`, proof/public/setup/deployment artifacts remain under `tests/zk_verifier/fixtures/bounded-vrf-preview-v1/`. Its **309.924 s** prove+wrap and **0.499 s** SDK verification also miss the 120-second target. The selected v11 key explicitly rejects that historical candidate proof; it is not relabeled or deleted. Historical SBF `eed6d77eb10d46ae70a6b1410d3063bb01b662c84a0f10643d825c0c4c7fd881` and its **84,263 Verify CU** belong only to key001a and cannot qualify key00f8.

## Checkpoint and domain bindings

Checkpoint count is pinned in both the reviewed guest profile and selected key metadata. Creation and Production resolution reject a different checkpoint `step_count`, including pre-existing proof accounts. Another count requires an explicitly reviewed profile/key.

Kind3's `tx_effect_root` is the complete candidate transaction-tree root, not kind2's selected leaf. The guest verifies its bounded inclusion path. `CreateStepProof` requires equality with the checkpoint root, a valid step index and canonical empty instruction path. Kind2 retains its original leaf/path check. Production resolution rechecks candidate count/root/index/readonly/settlement bindings.

Session context remains the exact existing Poseidon commitment over 149 canonical bytes. Candidate `readonly_l1_root` binds ER readonly/program/sysvar/profile/runtime context, not L1 provenance. Candidate settlement commits to no cross-layer settlement. The development checkpoint builder must populate these same candidate meanings; the profile is not silently salted into session context.

Normal session/owner/authority/checkpoint/challenge/public-hash/length/sealing checks remain. Invalid cryptographic proof follows the existing invalid-proof/slashing outcome; structural preconditions return without state changes. No test-only verifier is used.

## Deterministic key generation

Run genuine SP1 **6.8.0** SDK setup for the exact reviewed ELF. Retain the serialized public SDK verifying-key artifact and a `northstar-sp1-setup-v1` receipt with SP1 version, ELF SHA-256, actual SDK `bytes32()` program key, verifying-key SHA-256, deployment-file `profile_sha256`, separately compiled `policy_profile_sha256`, and `production_acceptance:false`.

The deployment JSON uses schema `northstar.bounded-vrf-candidate-deployment.v1`, `guest_status:"authenticated-envelope"`, kind3/version1, integer `checkpoint_steps` in 1..16 matching the guest policy, exact public field order, SP1 version, ELF and policy identities, and `production_acceptance:false`. Its complete bytes are hash-bound. The checked-in JSON contains current runtime/program/source identities.

Independently reproduce SDK setup, review the actual source/profile and pin the expected guest hash separately:

```sh
python3 -B northstar/programs/portal/tools/bounded_vrf_key.py generate \
  --elf guest.elf --setup-receipt setup.json \
  --sdk-verifying-key sdk-vk.bin --profile deployment.json \
  --reviewed-program-vkey-hash 0xREVIEWED_HASH \
  --output-dir generated-candidate
```

Use identical arguments with `check` to verify deterministic output. Generation refuses an existing directory and never overwrites installed constants. Review the Rust/JSON pair together before installation. Receipt/artifact consistency is **not authenticated SDK execution or human sign-off**; actual proof verification remains mandatory. Never replace the typed policy hash with the ELF-containing deployment JSON hash.

The tool independently decompresses all six pinned Gnark IC points, validates curve/subgroup math, compares used points with the unchanged outer-key source and computes `IC[0] + guest_hash * IC[1] + recursion_root * IC[4]`. Exit code is strictly zero. The unused legacy exit-code IC has a retained y-coordinate sign bit; canonical full-key test references come from the compressed key without changing legacy runtime bytes. The two variable inputs remain public-values digest and proof nonce. Rust tests independently rederive the fold using Solana BN254.

## Qualification and immutable SBF identity

For every selected guest/key/artifact revision, run Python generation checks, zk-types/native Portal tests with both feature choices, configured full/reduced-key equivalence, source/receipt/profile/cardinality checks and strict clippy/fmt. The actual v11 proof must accept, historical001a candidate proof must reject, and legacy kind2 acceptance must remain unchanged.

Build an actual new SBF with the selected key; never substitute the historical001a artifact. Pin its immutable SHA before testing/deployment. `sbf_candidate_verifier_accepts_actual_proof_and_rejects_mutations` is non-ignored and consumes the genuine integrated-v11 fixture. It verifies through real BPF and rejects eight changed proof positions, all eight changed public fields and legacy-domain reuse. A separate SBF regression rejects the historical v7c candidate proof. Whole `VerifyErStepProofV1` CU is **not complete ResolveChallenge CU**.

The ignored `sbf_verifier_accepts_checkpoint_bound_sp1_proof` remains an explicit external-artifact override. With an authenticated actual resolver snapshot, `resolver::production_resolver_accepts_actual_bounded_vrf_fixture` requires kind3/version1 and exercises the full Production rejection matrix and a 130,000 CU limit. Retained injected snapshots are regressions, never fresh lifecycle acceptance.

The actual configured v11 SBF is SHA-256 `174001333ad0126a676953f52f244fb6755355eeab0d181391e20509b2fd728e`; standalone verification measured **84,262 CU**. Three genuinely new local sessions now have matching captures, SDK-verified proofs and valid Production resolutions at **111,253 / 109,753 / 111,253 CU**. Prove+wrap took **245.493 / 204.670 / 210.837 s**, all above 120 s. A first-batch CUDA backend fault and the explicit recovery window remain recorded. See the [versioned three-case evidence](../../../zkvm-vrf/evidence/g5-a10g-v11/README.md); these are actual local results, not human or production acceptance.

Measurements target **g5.xlarge/A10G**, not L40S. No hosted/delegated handoff, cross-layer settlement, production rollout or human sign-off is implied.
