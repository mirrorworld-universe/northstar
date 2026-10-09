# Delegated lamport surplus reconciliation

Portal deposits are receipt-backed credits. `DepositFee` therefore accepts only
non-executable, data-empty, System-owned recipients. In particular, a delegated
Portal-owned account cannot receive a new receipt-backed deposit while its SOL
is also held directly on L1.

Direct L1 transfers into delegated accounts do not update ER balances. They can
arrive after a checkpoint has fixed its settlement targets. New validator
settlements reconcile positive excess into the canonical FeeVault while keeping
the checkpoint's account balances and checksum unchanged. Shortages still fail.
This is a conservative liveness fallback, not automatic ER deposit ingestion.

FeeVault is bound to the session authority. Its lamports, including reconciled
surplus, are refunded to that authority on session close. The original sender or
delegated account owner does not receive an automatic refund or ER credit.

## Optional instruction extension

The existing `SettleAccountLamports` tag (12) and Borsh payload remain unchanged.
The legacy payload is 97 bytes, excluding the one-byte enum tag. The enhanced
form appends the 32-byte settlement accumulator expected immediately before the
lamport operation, and appends a writable canonical FeeVault after all delegated
account/record pairs. Its owner, discriminator, bump and authority are validated.

Without this extension, Portal retains the legacy exact-conservation behavior
and account list. Instruction tags and persistent account layouts do not change.
Consumers decoding the enhanced form must deserialize the legacy payload first,
then read the guard; strict `PortalInstruction::try_from_slice` cannot decode the
extra bytes by itself.

The expected accumulator is derived from the immutable plan's data chunks and
owner changes. Portal computes the next accumulator from the requested lamport
targets. A retry matching that next accumulator is a no-op even if another L1
transfer has changed the balances. A different accumulator is rejected. The
validator also skips an acknowledged lamport operation when resuming after later
receipt or token-withdrawal operations, including after loading a durable plan.
The durable plan format remains unchanged.

All balance, rent and arithmetic checks precede writes. The combined lamports of
the delegated accounts and FeeVault are conserved. The entrypoint accepts 17
accounts to accommodate seven account/record pairs plus validator, session and
FeeVault; the existing seven-account settlement limit is unchanged.

Receipt payouts happen after delegated lamport settlement. The planner subtracts
those payouts from the intermediate targets so that the final balances still
match the ER checkpoint.

## Rollout

Deploy the updated Portal before the validator. Older Portal versions ignore the
optional guard and still reject positive surplus. Existing callers using the
legacy encoding continue to work with the updated Portal. Positive-surplus
handling requires the enhanced encoding and canonical FeeVault account.

No delegated-transfer watcher or synthetic receipt event is added. Automatic ER
credits would require distinct custody accounting, persistent deduplication,
fork handling, and reconciliation of transfers arriving after checkpoint creation.
