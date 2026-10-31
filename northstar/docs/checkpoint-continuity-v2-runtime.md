# Continuity runtime integration

This is an opt-in development implementation, enabled with
`checkpoint-continuity-v2`. It is not hosted deployment qualification or a new
execution-proof acceptance claim. The validator, Portal SBF artifact and bridge
SBF artifact must use the same feature selection.

A CLI built with this feature opens sessions with explicit `--checkpoint-profile`
and `--l1-genesis` identities and the versioned nonce account. Online opening
checks the actual RPC genesis before signing. Online deposit and keypair delegation
validate the Portal-owned session and derive nonce-bound receipt/inbox accounts.
Offline ingress and other unbound commands are refused rather than silently
emitting legacy instructions.

## Committed state boundary

A continuity session starts from the protocol-defined empty state. Portal-owned,
canonical ingress records authorize SOL credits and delegation materialization.
The runtime consumes an ordered prefix from the finalized L1 root, persists its
transitions, and only then publishes the resulting ER accounts. Legacy event
notifications are discovery hints; they cannot credit or hydrate the v2 mutable
state directly.

Bridge registrations are immutable, session-context-bound discovery metadata and
require the session authority's signature. Registration does not itself authorize
or apply a token credit. The v2 bridge artifact validates the live session context
and stages its own PDA delegation through the versioned Portal CPI. L1 token
deposits atomically update custody/receipts and append vault-PDA-authorized input
records; the delegated L1 account stays unchanged. The manager executes each token
input through the real bridge SBF instruction and durably records its input plus
SVM result before commit. The payer needs committed ER SOL funding for cursor rent.
Legacy hints and ordinary top-level/CPI credit calls cannot bypass input capture.
Token withdrawals capture both top-level and CPI effects. The selected bridge ABI
requires a readonly mint on withdrawal so decimals are checked before debit. Token
plans retain cumulative vault counters, and reconciliation requires matching consumed
Portal authorizations plus the vault's paid counter, not a deposit receipt counter.

SVM execution records account pre/post values, execution results, readonly input
observations, replay data, and supported withdrawal effects at the pre-commit
boundary. A failed persistence operation cancels SVM commit and quarantines the
runtime. This capture does not itself verify SVM execution or independently
authenticate arbitrary readonly L1 inputs.

## Durable recovery

The checksummed lineage capsule now uses storage format **3**. This is distinct
from checkpoint protocol version 2. Existing capsule formats are not silently
migrated.

The capsule retains the accepted state, ordered transition tail, sealed artifact,
processed transaction wires, and immutable settlement plan. A plan is bound before
any proposal is signed. Missing or changed plans cannot be reconstructed from a
partially settled L1 view. Plan persistence failures prevent publication.

New-session activation explicitly initializes a new store. Process restart uses
resume mode only; missing established state does not authorize initialization.
Recovery raises the working ER slot above every retained transition and checkpoint,
not just the on-chain accepted head. Deactivation preserves the capsule.

## Checkpoint lifecycle

The manager proposes and commits the versioned Portal instruction surface. It
checks the retained artifact, endpoint openings, cursor, and checkpoint binding
before signing. Settlement uses the bound plan. Accepted and settled lineage heads
advance only after reconciliation against finalized L1 state and supported receipt
effects.

The regression suite covers real Portal SBF session/deposit creation, ordered
credit consumption, pre-commit persistence rejection, restart, missing-state
rejection, immutable plan recovery, late delegation, and a later deposit followed
by a second proposal with the first checkpoint's final root. Real SPL/bridge SBF
coverage also checks custody, token-input execution, cumulative targets, restart
without duplicate credits, and a later top-up after L1 reanchoring. SPL payout tests
cover two linked checkpoints, actual CPI withdrawal, partial-metadata restart/retry,
and malformed decimals, registry data and authorization tuples.

## Remaining deployment gates

- Full validator/RPC SOL/SPL lifecycle qualification is still required in addition to local bank/SBF evidence.
- CLI, consumer, and operator bindings must select the same protocol and session.
- Configured proof coordination is rejected until the new relation is qualified.
  Historical guest keys and proofs are not accepted as continuity evidence.
- Hosted rollout requires fresh isolated session identities, retained old state,
  native lifecycle validation, and separate ordinary VRF callback evidence.
