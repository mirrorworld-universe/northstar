# Continuity runtime integration

This is an opt-in development implementation, enabled with
`checkpoint-continuity-v2`. It is not hosted deployment qualification or a new
execution-proof acceptance claim. The validator and its Portal SBF artifact must
use the same feature selection.

## Committed state boundary

A continuity session starts from the protocol-defined empty state. Portal-owned,
canonical ingress records authorize SOL credits and delegation materialization.
The runtime consumes an ordered prefix from the finalized L1 root, persists its
transitions, and only then publishes the resulting ER accounts. Legacy event
notifications are discovery hints; they cannot credit or hydrate the v2 mutable
state directly.

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
by a second proposal with the first checkpoint's final root.

## Remaining deployment gates

- Token ingress requires its authenticated execution adapter; it fails closed.
- CLI, consumer, and operator bindings must select the same protocol and session.
- Configured proof coordination is rejected until the new relation is qualified.
  Historical guest keys and proofs are not accepted as continuity evidence.
- Hosted rollout requires fresh isolated session identities, retained old state,
  native lifecycle validation, and separate ordinary VRF callback evidence.
