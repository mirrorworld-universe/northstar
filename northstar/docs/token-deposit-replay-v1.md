# Receipt-backed ER token credits

## Invariant

A delegated token deposit must execute an ER transaction before its resulting
state can settle or receive undelegation approval. Direct bank injection did not
produce checkpoint steps, so a deposit followed by undelegation could stall with
no transaction available to seal.

`ApplyTokenDeposit` (Token Bridge instruction tag 10) consumes a cumulative L1
receipt balance. The registered session validator signs the transaction. The
program verifies the session, delegation, bridge, token account, receipt and
progress PDAs. A delegated token account is Portal-owned on L1 but Bridge-owned
in ER; requiring Bridge ownership and a live Portal delegation prevents this
credit instruction from minting balances on L1.

## Progress accounting

- L1 delegation creates or refreshes `token_deposit_origin`, seeded by the ER
  token account. Its balance is the receipt total already represented by the
  account at delegation. Earlier deposits are not credited again.
- An ER-only `token_deposit_cursor`, seeded by the token account and origin
  balance, records the cumulative credited total. The credit transaction creates
  it if necessary and applies only the difference, never more than the L1 receipt.
  Equal or older totals are rejected by the program.
- Both progress accounts encode `discriminator: u8`, `balance: u64`, `bump: u8`;
  origin/cursor discriminators are 4/5. Existing token and receipt layouts remain
  unchanged.
- The runtime rescans delegated receipts when polling settlement, so checkpoint
  admission backpressure does not discard a deposit event. Credits execute through
  the normal transaction client, history recording and checkpoint capture paths.
- The internal credit path can finish pre-request deposits while user writes are
  frozen. Undelegation approval also checks for unconsumed receipts. New L1
  deposits are rejected once an undelegation request exists, closing the race
  between approval and a later receipt increment.

## Client and deployment compatibility

`DelegateErTokenAccount` appends a readonly deposit receipt and writable origin
PDA after its existing accounts. `Deposit` appends the readonly undelegation
request PDA; it is required when the target is delegated. The checked-in example
and test builders use these accounts. Upgrade client builders and the Bridge
program together.

Old delegations without an origin are not silently assigned an inferred baseline.
They need an explicit migration or undelegation with the old deployment followed
by redelegation. Do not upgrade an active deployment expecting automatic migration.

The checkpoint's eight public inputs and preserved replay fixture are unchanged.
This does not expand the narrow ZK replay relation to prove Token Bridge CPI
execution. Cursor account writes use the existing ER journal; this change does
not establish additional process-crash or power-loss acceptance.

## Validation

- `receipt_credit_is_er_only_bound_and_applied_once`: real SBF execution, authority
  and PDA checks, receipt bounds, stale totals, duplicate rejection and unchanged
  cursor/account state after rejected calls.
- `test_token_deposit_requires_authenticated_origin`: missing origin cannot credit
  state, create a checkpoint or bypass pending-deposit approval checks.
- `undelegate_rejects_unsettled_token_deposit`: request-time deposit rejection
  preserves the custody vault and receipt.
- `live_validator_spl_token_bridge_round_trip`: withdrawal, mid-session deposit,
  automatic checkpoint settlement and undelegation; exactly one successful
  history-recorded credit for the final cumulative receipt total. Local run:
  48.30 seconds, without increasing timeouts or adding user ER transactions.

All validation commands from `.github/workflows/ci.yml` passed locally: format,
strict clippy, shell smoke guard, both SBF builds, validator build, smoke-state
example, Bridge core/E2E, Northstar/Portal, SVM and core-service tests. Deployment
jobs were not run.
