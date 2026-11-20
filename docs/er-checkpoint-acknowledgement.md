# ER confirmations during checkpoint settlement

SessionV2 may record one provisional successor batch while its predecessor settles on Solana. The predecessor's artifact and settlement plan must both be durable before the successor can accept writes. Each batch retains the 16-transition limit. The successor must start after the sealed ER slot and cannot be sealed while the predecessor remains unsettled locally.

An ER confirmation identifies a transaction in the local rollup. It does not establish L1 settlement, make the transaction irreversible, or authorize a VRF proof. The relay still derives witnesses from the accepted checkpoint snapshot, not from the provisional successor. The isolated signer still verifies finalized, settled L1 membership before signing.

Restart recovers both batches from the same lineage capsule. Only the sealed prefix can match the next authenticated L1 head. A mismatched or invalidated predecessor still requires reconciliation; the runtime never promotes the successor to replace it. A previous binary cannot resume a capsule with both batches populated, so do not roll back to it.

Measure acknowledgement from the first SDK call on the saved signed bytes to the first successful confirmed ER receipt, including any settlement refusals. Measure the finalized callback separately. An unavailable or full successor still applies backpressure. A smoke test below three seconds is not a tail-latency or settlement guarantee.
