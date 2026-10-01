# Checkpoint outcome matrix v1

Status: frozen for Settlement v1.

## Outcomes

| Path | Trigger | Checkpoint | Challenge | Bond | Cursor | Next action |
|---|---|---|---|---|---|---|
| Normal finalization | Challenge deadline passes without a challenge, then `CommitCheckpoint` | `Committed`, then `Settled` after exactly one complete settlement | None | Released once at commit | Finalized slot/root advance at commit; active pointer clears at settlement | Propose the next ER slot |
| Respondent timeout | Respondent misses its turn or DA remains missing | `Invalid` | `ChallengerWon` | Slashed once to challenger | Finalized slot/root unchanged; active pointer clears | Replacement may reuse the same ER slot and previous finalized root |
| Challenger timeout | Challenger misses selection/proof turn | `Pending`, resolution flag set | `ValidatorWon` | Remains locked | Active checkpoint remains | Commit after the original hard deadline; no second challenge |
| Valid step proof | Production verifier accepts the isolated transition | `Pending`, resolution flag set | `ValidatorWon` | Remains locked | Active checkpoint remains | Commit after the original hard deadline; no second challenge |
| Invalid step proof | Production verifier rejects the isolated transition | `Invalid` | `ChallengerWon` | Slashed once to challenger | Finalized slot/root unchanged; active pointer clears | Replacement may reuse the same ER slot |
| Replacement | New proposal follows invalidation/cancellation | `Pending` with fresh deadline and bond | Fresh challenge state | New bond locked | Same ER slot becomes active; finalized root unchanged | Run the full challenge window again |

Terminal checkpoint states are `Settled`, `Cancelled`, and `Invalid`. Repeating commit, timeout, proof resolution, settlement finish, or bond release after a terminal transition must fail without changing balances, roots, or settlement state.

## Timing contract

- Checkpoint challenge window: caller value clamped to the protocol default and capped at 9,000 L1 slots.
- Challenge hard deadline: the checkpoint's original challenge deadline; opening a late challenge never extends it.
- Turn deadline: `min(current_l1_slot + 750, hard_deadline_l1_slot)`.
- Deadline checks are strict: an action is live only while `current_l1_slot < deadline`; timeout is available at equality.
- Runtime logs emit `elapsed_slots` from challenge opening for timeout and proof outcomes, plus numeric turn/outcome values suitable for CI extraction.

## Executable coverage

`programs/portal/tests/integration.rs` freezes the matrix with:

- `checkpoint_proposal_commit_deadline_flow`
- `challenge_bisects_to_one_step_and_challenger_timeout_restores_checkpoint`
- `checkpoint_da_timeout_slashes_and_allows_recovery`
- `submit_step_proof_invalid_slashes_and_blocks_settlement`
- `valid_step_proof_prevents_second_challenge`
- `committed_checkpoint_blocks_next_proposal_until_settled`

The manager-level `validator_checkpoint_flow_waits_then_settles` test additionally verifies that all canonical checkpoint roots reach Portal, settlement uses the checkpoint-bound plan rather than later live ER state, tampered durable plans fail closed, and settlement is not repeated.
