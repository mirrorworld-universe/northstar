# Checkpoint CPU timing evidence

Measured September 13, 2026 on a local CPU host, using a fresh `solana-test-validator`
ledger and the default Portal SBF program. This is GPU-free preparation for the
September bonded-finality acceptance, not a completed finality measurement.

## Reproduction

Follow `settlement-v1-acceptance.md` for validator startup, readiness, and cleanup.
Run the live test with output capture enabled:

```bash
NORTHSTAR_LIVE_RPC_URL=http://127.0.0.1:18999 \
  cargo test -p northstar real_checkpoint_bisects_to_captured_transaction \
  -- --ignored --nocapture
```

Each `NORTHSTAR_TIMING ` line contains a JSON object with `schema_version: 1`.
Transaction durations include blockhash acquisition, signing, RPC submission,
and confirmation. They are wall-clock measurements, not Portal compute time.
The four response/select pairs are followed by the final DA reveal; the
`respond_or_reveal` events carry the claimed step to preserve that sequence.
No slot warps are used in this live harness.

## Observed run

| Phase | Wall time (ms) |
|---|---:|
| Open session | 557.115 |
| Propose checkpoint | 517.874 |
| Open challenge | 525.934 |
| Round 1 response / selection (step 8) | 517.872 / 524.829 |
| Round 2 response / selection (step 12) | 519.053 / 525.334 |
| Round 3 response / selection (step 10) | 519.302 / 526.570 |
| Round 4 response / selection (step 11) | 516.535 / 526.894 |
| Final DA reveal (step 10) | 516.710 |
| History-derived witness extraction and replay validation | 133.405 |

The full test passed in 8.91 seconds, including test setup and negative checks.
It executed 16 gasless SBF account-write transactions, reached interval `[10, 11]`,
and matched all eight public inputs. The checkpoint remained unresolved.

`proving_ms`, `verification_ms`, and `recovery_ms` are explicitly `null` and
`resolved` is `false`. Missing phases must never be treated as zero-duration
successes or used to claim three completed challenges within ten minutes.
The independent maintainer still needs to reproduce this run.

## Timeout retry invariants

The Portal SBF tests `challenge_bisects_to_one_step_and_challenger_timeout_restores_checkpoint`
and `checkpoint_da_timeout_slashes_and_allows_recovery` now each submit three distinct
terminal timeout retries. Each must reach Portal and return `CheckpointStateInvalid`.
The session, checkpoint, cursor, challenge, DA proof, and bond recipient accounts
must remain byte-for-byte and lamport-for-lamport unchanged.

Each retry prepends a distinct transfer that must roll back, avoiding cached
signature rejection as false evidence. The transaction fee payer is deliberately
excluded from unchanged-balance assertions. These tests use slot warps to test
protocol deadlines and provide no wall-clock finality evidence.
