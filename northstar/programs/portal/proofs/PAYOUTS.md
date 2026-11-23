# Which withdrawal was paid, and can a partial plan resume?

The [aggregate liability theorem](LIABILITIES.md) counts an ER withdrawal as outstanding until L1 posts its target and consumes the matching Portal authorization. Two implementation-linked proofs cover how Portal, the bridge and the validator identify that payment, and how the next target is calculated. Neither changes any instruction, account, PDA seed or stored digest format.

## Which payment do the bytes identify?

Portal and the validator now use the [same encoder](../src/payout_wire.rs) and [hash adapter](../src/payout_hash.rs). The [wire proof](payout_transcript.rs) establishes that equal 273-byte payloads imply equality of all eleven fields: eight account keys, the amount and cumulative target as little-endian `u64`s, and the decimal byte. The authorization message is 316 bytes; the settlement checksum message, including the previous accumulator, is 321. The two preimages cannot be equal, even for different payouts. The bridge's 58-byte Portal consume instruction round-trips its tag, slot, checksum, amount, target and decimals.

These are properties of the bytes fed to SHA-256, **not** a proof of collision resistance or of which accounts a caller may supply. [Compatibility tests](../tests/payout_wire.rs) use separately computed SHA-256 golden vectors and compare the CPI encoder with Portal's actual Borsh serialization and deserialization in both build profiles.

## What if only some transfers land?

The [V2 planner](../../../src/ephemeral_runtime/token_settlement_v2.rs) starts each vault at its posted withdrawal counter. The shared [reservation function](../../token-bridge/src/payout_schedule.rs) advances it by exactly one positive amount, without overflow or exceeding recorded deposits. Rejection leaves the planning cursor unchanged.

The [schedule proof](../../token-bridge/proofs/payout_prefix.rs) composes that function with the actual on-chain withdrawal counter check. For any finite ordered sequence of positive amounts whose total fits the available capacity, every planned step posts exactly its amount from the preceding counter. Replaying the **same sealed plan** from any already-paid prefix yields no new posting for its prefix and posts each remaining target in order. This is conditional on the specified counter frontier; it does not assume all CPIs succeed or claim that a validator eventually delivers a payment.

A [bank regression](../../../src/ephemeral_runtime/tests/token_settlement_v2.rs) starts with 42 tokens and requests 7 and 11 for two different recipients. It stops after the first real SPL transfer, compares the posted vault counter, remaining custody and both authorization tickets against the **persisted** plan, and restarts the manager. Previously the manager resubmitted the already-paid transaction with the same blockhash; the bank returned `AlreadyProcessed` before the second payout. The [V2 dispatcher](../../../src/manager_continuity_v2.rs) now signs only outstanding payouts, in retained order, using the same payment evidence as final reconciliation. Both destinations end with their exact amounts; custody and the active ER holding each end at 24. The test covers restart, not recovery from arbitrary conflicting L1 state.

## Evidence and boundaries

The pinned whole-crate Verus runs reported **15 verified, 0 errors** for each suite; eight [wire controls](evidence/extended/wire.json) and four [schedule controls](evidence/extended/schedule.json) each failed the expected postcondition. Counts include verifier obligations and rechecked component code, not 30 independent financial properties. [Run record and SBF hashes](evidence/extended/payouts.txt). To reproduce, use [the pinned tools and runner](INVARIANTS.md#independent-reproduction) with suite names `wire` and `schedule`.

Trusted boundaries include the mapping from bank accounts into typed fields, durable plan identity and order, the SHA-256 syscall, Borsh, SPL token semantics, SVM atomicity, the compiler and deployed binary identity. The proofs do not certify signer entitlement, a live inventory or liveness.
