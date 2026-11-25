# Single-replica ER shadow replay

One replica can check captured slots and recover its local replay history. It does not vote, certify a slot, contact the VRF signer, submit transactions, or change when a callback is released. A matching replay is an observation, not an attestation. No committee enrollment or second instance is required.

## Run the local pilot

From the validator workspace root on Linux:

```sh
cargo test --locked -p northstar --features certified-er-replay --lib certified_replay::tests::single_replica
```

The fixture uses disposable source and replay banks. It captures an explicitly empty slot and a signed credit, drops the observer, rebuilds the anchor on a fresh bank and reopens the same store. A spend that depends on the recovered credit must then agree with the source's frozen bank. Other cases check exclusive ownership, missing input, mismatched bank history, damaged storage and rollback below a saved cursor. This command starts no validator or hosted service.

## Inputs for a hosted pilot

- Use `ReplayBootstrap::from_root` with an independently rooted L1 bank, the pinned session, settled checkpoint, opening and program images. Keep the exact bootstrap material: the current L1 tip can move past that checkpoint.
- Independently reproduce the **full** frozen ER bank at the pinned ER slot, including its bank hash, `SlotHashes`, fee configuration and recent-blockhash queue. Use dedicated storage and caches. A child of the serving validator, an account dump or an RPC block is not a substitute. Pass the owned bank and reviewed pin to `ReplayBootstrap::shadow_anchor`.
- At each physical slot freeze, before the source parent is disconnected, derive `FrozenSlotCapture::from_bank` from the frozen source bank and its finalized ER history. Preserve signed packet order and the actual completed tick hash. A missing RPC block does not prove the slot was empty. Missing nonempty history, failed transactions, unsupported inputs, or a freeze missed by the collector stop this bounded profile. Do not synthesize ticks, packets or empty slots to fill gaps.
- Provision an owned `0700` parent **separate** from validator, signer and relay state. `ShadowReplica::create` takes a new child directory; `resume` takes an existing one. The single writer publishes the private replay image with file sync, atomic rename and directory sync. After a successful observation, retain `ShadowCursor { slot, digest }` outside the store's rollback domain. The file checksum is not a substitute for that external cursor.
- To recover, stop using the old object, reconstruct the original anchor and call `ShadowReplica::resume` with the externally retained floor. Recovery re-executes the saved bytes and checks bank pins and receipt roots. Resume subsequent capture only from the next contiguous slot. On any inconsistency, preserve the store and input; do not reset or advance the cursor.

The pilot covers at most 256 physical slots per window and 16 successful packets per slot. A full window stops; it does not roll over automatically. There is no live collector, daemon or validator activation flag in this change. The next devnet step is a read-only capture and private full-bank handoff for **one** bounded window, followed by recovery from the separately saved cursor. The shared validator and its settled signing path stay unchanged.
