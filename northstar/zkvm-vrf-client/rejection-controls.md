# Actual guest rejection-control protocol

**Protocol/request, not completed-run evidence.** Run only against the genuine authenticated candidate ELF and its actual SDK setup receipt. Keep each original input, mutated input, manifest, stdout/stderr, process exit and measurements. No new proving submission is needed for these CPU controls.

## Canonical inputs and generator

Start with the engine-owned canonical producer's actual session-bound `witness.borsh` and `public-inputs.bin`. Deserialize with the current witness-owned `CandidateEnvelopeV1`, require exact Borsh round-trip equality, then clone and mutate typed fields. Serialize with `borsh::to_vec`. Do not use guessed byte offsets, fabricated output objects, a fixture engine, or a hand-authored JSON result.

The engine's real `verify-envelope` command supplies the native comparison. It is not guest execution; retain its status separately. If the current engine CLI lacks a typed mutation command, have its owner add a narrow generator using the same data types and canonical helpers. This document does not claim that generator or the guest controls already ran.

| Case | Deterministic typed mutation | Required invariant |
| --- | --- | --- |
| signed message | Decode the real signed transaction through existing canonical wire/transaction tooling; change one byte of its first signature, without signing again or changing the message | Every message signature is checked |
| envelope version/domain | Change `CandidateEnvelopeV1.version` or `proof_version` to an unsupported value; preserve canonical Borsh serialization | Unknown envelope selectors reject |
| compiled profile | Flip one byte of `input.profile.vm_config_sha256` | Witness-selected profiles cannot replace the compiled policy |
| program/account owner | Select the pinned ProgramData account by its real program mapping and change its `state.owner` | Authenticated program/loader owner binding is required |
| claimed poststate | Change one data byte in an actual writable consumer-owned `expected_output.accounts` entry, preserving key/order/owner/lamports | Actual execution, not a claimed write, determines poststate |
| claimed return producer | On a real fulfillment input with return data, change only `expected_output.return_data.program_id` to a different program ID | Return bytes are bound to their actual producer |

The inspected real preview fulfillment sidecars have **empty final return bytes but a recorded final producer**; the engine preserves this as `Some(CandidateReturnDataV1)` even when data is empty. Change that actual producer while leaving the empty bytes unchanged. Do not confuse this final-buffer field with the verifier's earlier 64-byte return consumed during CPI. Reexecution of that consumption remains guest/interpreter-owned.

For poststate and return-producer cases, also build a **self-consistent claimed checkpoint** with the witness library's `build_candidate_checkpoint` / canonical transaction/context-root helpers, using the unchanged genuine execution input and original authenticated prestate. This prevents a stale-root rejection from being mistaken for the stronger actual-reexecution check. The generator must record if a mutation cannot be constructed; do not turn a generation error into a passed guest control. If the chosen real input has no return data, select an applicable genuine input or record that case as unavailable; never invent a captured return.

Do not repair the bad signature, alter the guest policy/ELF, update the setup receipt to a different key, relax checks, or submit controls as public-cluster transactions. No Portal or external VRF ABI changes are involved.

## Parent-run execution

For each canonical variant, use a new output job and the same actual baseline public bytes:

```sh
bash northstar/zkvm-vrf-client/remote-run.sh \
  "$GPU_PREFIX" "$ACTUAL_CLIENT" "$ACTUAL_CLIENT_SHA" "$RESULT_PARENT" \
  execute --elf "$GENUINE_ELF" --profile "$DEPLOYMENT_PROFILE" \
  --setup-receipt "$GENUINE_SETUP_RECEIPT" \
  --input "$VARIANT_BORSH" --public "$BASELINE_NATIVE_PUBLIC" --expect-rejection
```

The baseline public file keeps client-side canonical-field checks valid. Rejection mode does **not** pass merely because output differs from those expected bytes. It requires a returned guest report with full-width **nonzero exit and zero public bytes**. An SDK/host exception, ISA-loader error, timeout, OOM, process crash or absent report is an incomplete/failed control, never an observed guest rejection. A returned zero exit or any public output also fails the control and remains retained.

Client/header/backend rejections are useful separate controls, but do not count as the six actual-guest checks above. Record which layer rejected each input. SDK proof-verification mutations likewise are not guest-execution mutations.

## Positive application-failure control

A valid witness of a **supported failed PROGRAM instruction** is not an invalid witness. Obtain a freshly captured, correctly signed failure/rollback window associated with the actual Portal session, and run the canonical producer plus real interpreter validator. Do not relabel an unmatched historical capture.

The guest should accept that correct execution witness with exit **0**, exact 256 native public bytes and complete rollback/effect accounting. Use ordinary `execute`, **not** `--expect-rejection`. If the bounded profile does not support that error variant, report unsupported instead. A genuine wrapped rollback proof may then be generated in a separately identified development corpus run; it is not one of the three successful request/fulfillment/callback resolution cases.

## Final series remains separate

The real three-case proof window uses distinct session/checkpoint/challenge identities and genuine prepare records. Retain warmup separately, including the observed slow v7c warmup. Lifecycle code checks real deadlines; no slot/deadline bypass is authorized. Each final wrapped proof still targets <=120 s on g5/A10G; each complete Production resolution still targets <=130,000 CU. None of these controls supplies fixture-owner, integration or independent human sign-off.
