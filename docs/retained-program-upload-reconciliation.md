# Retained fresh-program upload reconciliation

A fresh cutover can stop during program upload before any validator stop or start. The ordinary controller deliberately rejects an unresolved upload intent instead of generating another key or buffer.

Use the **Reconcile retained fresh program uploads** workflow from `master` only after reviewing bounded pending-revision diagnostics. Supply the exact retained fresh source revision and its reviewed previous SessionV2 context. This is a manual, one-pass upload operation, not fresh-stack qualification.

Admission requires:

- Existing matching plan, reservation, pinned artifacts, private keys, and `prepared` progress; no activation, runtime directory, or native-start barrier.
- Classified native uploader write/confirmation retry exhaustion, without a classified funds, transaction, ELF, or size rejection.
- The same active previous validator process, effective service binding, session identity, and ER genesis.
- Recorded loopback L1 RPC with the expected devnet genesis and validator identity.
- Every unresolved buffer bound to its original key, loader owner, authority, length, and frozen ELF bytes. Unwritten zero bytes are allowed; other mismatches and unrecorded buffers are rejected.

The controller verifies the recorded L1 RPC and uses it for all artifact/account/rent/finalization reads as well as submission. It durably records the plan hash, controller revision, original service binding, read/submission endpoint, pending roles, and one-pass budget. Each CLI submission gets its own durable role barrier before invocation. It retains the original program and buffer keys. It never starts, stops, or reconfigures the validator or workers, redeploys old programs, funds a wallet, or resets journals.

A failed or interrupted pass retains its attempt barrier and requires operator reconciliation; a default rerun cannot submit again. For the specifically reviewed failed controller revision, an operator may acknowledge the exact failed-record SHA-256 from bound read-only diagnostics. This creates one separate linked reconciliation record and leaves the prior record/report unchanged. Attempted/completed records, other controller revisions, mismatched acknowledgements, and any existing additional-pass record are rejected. There is no automatic or cascading retry.

A successful report means only that both planned programs were finalized against their frozen artifacts and the old service remained unchanged. It is **not** permission to claim a fresh cutover qualified.

After success, the original fresh controller must independently adopt the finalized upload observations and complete its guarded native activation and lifecycle qualification. Preserve the previous stack and all upload evidence throughout. Remove release selectors when they are no longer needed.

Raw deployment CLI output may contain sensitive recovery material. Keep it private; diagnostics and uploaded reports contain only allowlisted metadata and fixed error classifications.
