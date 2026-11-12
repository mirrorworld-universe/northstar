# Reconcile the reviewed fresh settlement resume

**Reconcile reviewed fresh settlement resume** is a manual, master-only path for the specifically reviewed fresh activation that completed bootstrap/SOL preparation, reached native readiness on its SOL restart, and then received canonical-settlement-pending admission for its retained withdrawal.

First run read-only native runtime diagnostics for the original artifact revision. Admission requires `fresh_resume_review.status=reviewable`, the exact returned review digest, and the retained new context. The digest binds the plan, native activation, failure/progress records, original native start/ready barriers, SDK receipts/binding/config, retained SOL keys, original signed withdrawal, and exact inactive timestamp. The verified target must remain quarantined, inactive with PID zero, and have no restart policy. Any changed evidence or prior poststart reconciliation blocks admission.

The controller preserves all earlier records, program/session identities, signed bytes, journals, and archived prior state. It verifies the original staged artifacts and finalized programs without building, funding, submitting programs, or regenerating keys. It durably permits one additional reconciliation start, retains independent L1 catch-up and ER/session identity checks, then resumes the existing journal. The original remaining SPL restart and SOL/SPL qualification still run; neither restart coverage nor protocol checks are skipped.

Only the exact ER canonical-settlement-pending response is retried within the journal's existing receipt deadline, using identical signed bytes. Writes with different errors and L1 submissions are not retried by this rule. Expired intent, unavailable history, or changed identity fails closed; no replacement signature is created.

Success requires the complete original fresh-stack qualification plus qualified reconciliation/progress records. Any interruption, readiness failure, or SDK failure quarantines only the verified new runtime and requires operator reconciliation. There is no automatic repeat, rollback, state reset, or fresh identity replacement. Release selectors are not involved.
