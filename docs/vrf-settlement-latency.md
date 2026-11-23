# Trace and reduce ER VRF settlement latency

This change keeps the Portal challenge window, SessionV2 checkpoint ordering and
limits, VRF input and Verify CPI, and isolated signer's finalized-settlement gate.
It applies to the existing ER path; it does not add a direct-L1 signer mode.

## What changed

- A Portal transaction can reach local banking before the shared forwarding queue
  rejects it. Keep the pending transaction before using either path. On forwarding
  backpressure, retry its identical signed bytes at the next distinct L1 slot.
  After forwarding is queued, use the existing normal interval. Confirmation,
  expiry, and stage progression keep their existing checks. The retry state is
  in-memory; this does not change durable settlement or replay semantics.
- Wake the VRF relay once a checkpoint settles and local finalized-L1 reconciliation
  succeeds, even if no ER transaction produces a slot notification. The wake is
  coalesced and is not signing authority; polling and independent verification remain.

## Identify the actual wait

`Portal settlement dispatch` records the stage, ER slot, L1 dispatch slot, first
attempt slot, signature, and enqueue outcome. `local_and_forward_queued` does not
prove transport delivery. `Portal settlement observed` reports frozen-bank success
or blockhash expiry, not finalized inclusion. `Portal checkpoint settlement
reconciled` records durable local reconciliation against a finalized L1 bank.
Use the signature to check finalized L1 inclusion separately. Do not infer a
historical cause from the local forwarding-pressure reproduction alone.

Measure the signed request's first submission, ER acknowledgement, finalized
checkpoint observation, and verified callback on a monotonic observer clock.
Report failures as well as latency. Compare equivalent load, network placement,
and request deadlines. One hosted callback can verify lifecycle and proof
correctness but cannot establish a p95 or an end-to-end latency guarantee.
