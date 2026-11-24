# Did the SVM actually authorize the CPI withdrawal?

A retained inner instruction is not a signature. The optional [step auditor](../../../src/withdrawal_audit_v2.rs) runs the **original signed transaction** against an independently pinned pre-execution ER bank. The SVM executes the caller and the reviewed bridge again, including their signer checks. A match is evidence for that step only. No checkpoint, capsule, instruction format, or admission rule changes.

## Inputs and verdict

Call `northstar::withdrawal_audit_v2::audit(bank, artifact, &pins)` with the `checkpoint-continuity-v2` feature. Supply these independently:

- The checkpoint binding and selected step index from the checkpoint under review, and the V2 state root at that step's **pre-state**, including the withdrawal counters.
- A frozen, isolated bank at precisely that transaction boundary. Pin its slot and bank hash from a trusted reconstruction. Restore the full account set with account-hash verification enabled and **no slot-load limit**, or replay it independently from trusted inputs. Use the correct runtime, features, program cache and historical sysvars. A matching slot alone, an end-of-slot snapshot after the transaction, a current RPC bank, or pins copied from the submitted capsule are not substitutes. The API does not acquire or attest this bank; it rejects the explicit skip-hash configuration.
- The reviewed bridge program ID and SHA-256 of its loader-visible code. For the immutable BPF loader, the auditor hashes the program bytes. For the upgradeable loader, it hashes the bytes after the canonical ProgramData header, including padding. An unavailable image or a deployment in the selected slot cannot establish the upgradeable image in use.

`Ok(VerifiedStep)` identifies the signature, step, checkpoint binding, bank hash, and numbers of withdrawals and CPI withdrawals. `Unavailable` means no verdict: for example, a missing or mismatched pin, unresolved historical table, or no observed CPI withdrawal. `Mismatch` means the pinned artifact fails validation or disagrees with the observations under those inputs. Neither failure is a positive audit result or an automatic dispute decision.

## What the comparison covers

The auditor verifies the canonical transaction signatures and uses the bank's historical address loader. It loads and executes without committing, replacing the recent blockhash, overriding accounts or seeding the SVM with a recorded trace. It compares:

1. The execution result, including the ordered inner instructions, account indices and stack heights, compute units, logs and return data.
2. The complete sorted set of **touched, non-infrastructure** account pre/post images. It independently checks the checkpoint's committed account pre-state and the owner, balance and data hash of supplied readonly observations. This does not certify `observed_l1_slot` as a finalized L1 observation.
3. The ordered settlement effects derived using the fresh trace, freshly observed writes, bank registrations and pinned pre-state counters. Amounts, recipients, cumulative counters and duplicate occurrences must all match. The resulting state opening must equal the recorded post-state.

Success requires a zero-fee execution with at least one recognized inner withdrawal and only token withdrawal effects for the pinned bridge. It does not certify a later L1 payout, general SVM correctness, other steps, or the availability of a trustworthy bank for every historical transaction.

## Evidence and reproduction

The [Verus proof](../../token-bridge/proofs/cpi_audit.rs) covers the [executable comparator](../../token-bridge/src/audit_match.rs), **not the SVM**. A positive comparison requires byte-for-byte equality in all three components and a positive observed CPI count. The [reviewed summary](evidence/extended/audit.json) records five verified functions, zero errors and four rejected controls. Reproduce with suite `audit` in the [pinned runner](INVARIANTS.md#independent-reproduction).

The [bank regression](../../../src/withdrawal_audit_v2/tests.rs) constructs two separate banks from the same genesis inputs. The producer commits a signed transaction containing two calls. The test controller signs; the holding owner is a PDA and does **not** sign the outer transaction. Its SBF caller uses `invoke_signed`, and the real V2 bridge debits the holding from 42 to 36. The auditor reproduces both occurrences, including cumulative targets 3 and 6, while its balance remains 42 and the signature stays uncommitted. Missing pins yield no verdict; a changed recorded compute count in a structurally valid artifact yields a mismatch.

Build the V2 Portal, bridge and continuity fixture SBF artifacts as in the [CI execution job](../../../../.github/workflows/ci.yml), into the same output directory. Then run:

```sh
BPF_OUT_DIR="$out" TOKEN_BRIDGE_BPF_OUT_DIR="$out" cargo test --locked -p northstar \
  --features checkpoint-continuity-v2 --lib invariant_v2_audit_reexecutes_pda_signed_calls
```

See the [local run record](evidence/extended/cpi-audit-run.txt) for artifact hashes and test scope. No production transaction or third-party snapshot was attested in that run.
