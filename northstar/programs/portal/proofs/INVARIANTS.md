# Portal and SPL bridge: verification map

**Local run, 2026-11-24.** The evidence set covers fifteen suites with zero verifier errors and 86 controls that each failed their intended production postcondition. The [optional CPI auditor](CPI-AUDIT.md) checks a withdrawal step against a fresh SVM execution on an independently pinned pre-state. The [withdrawal provenance proof](PROVENANCE.md) binds recorded token effects to retained calls and distinguishes signed message data from CPI trace evidence. The [recipient entitlement proof](ENTITLEMENT.md) connects a payout to one owner-authorized V2 withdrawal. Earlier suites cover [what identifies a payout and what happens after a partial payout](PAYOUTS.md). The [aggregate liability proof and V2 journal gate](LIABILITIES.md) cover backing per vault. The earlier [bond](README.md) and [settlement](SETTLEMENT.md) records describe the component proofs. Suite counts include rechecked imports and are not counts of independent guarantees.

## Verification scope

| Suite | Property | Verus / controls |
| --- | --- | ---: |
| [CPI audit comparison](CPI-AUDIT.md) | A match requires identical execution, write and effect bytes and at least one observed CPI withdrawal. Re-execution and [offline cut reconstruction](CPI-AUDIT.md#reconstruct-the-bank-from-an-earlier-full-snapshot) are execution-tested, not part of the theorem. | 5 / 4 |
| [V2 withdrawal provenance](PROVENANCE.md) | Replay reconstructs the signed or invoked withdrawal. Each occurrence preserves its call fields; CPI account visibility descends from its outer instruction. | 65 / 9 |
| [V2 recipient entitlement](ENTITLEMENT.md) | A sealed plan preserves every authorized request's destination, mint, amount and multiplicity. A matched posting observes exact movement and cannot pay its ticket twice. | 50 / 10 |
| [Payout wire encoding](PAYOUTS.md#which-payment-do-the-bytes-identify) | The pre-hash bytes distinguish every withdrawal tuple. Authorization and settlement use distinct messages; the bridge CPI preserves the tag and every argument. | 15 / 8 |
| [V2 payout schedule](PAYOUTS.md#what-if-only-some-transfers-land) | From any paid prefix of a funded, ordered plan for one vault, retry pays no prefix item and posts each remaining counter exactly once in order. | 15 / 4 |
| [Aggregate liabilities](LIABILITIES.md) | From a complete, balanced starting inventory, deposits, V2 ER execution and matched payouts preserve the sum of spendable, uncredited and unpaid claims. V2 recording and replay reject an unbalanced transaction per vault. | 50 / 10 |
| [Vault custody](../../token-bridge/proofs/vault_custody.rs) | Deposits and payouts preserve backing and the exact excess of SPL custody over net recorded deposits. Posting requires both observed SPL balances to move by exactly the requested amount. | 11 / 5 |
| [Deposit credit](../../token-bridge/proofs/delegated_credit.rs) | Immediate and deferred credit agree. Successive cursor advances credit the same total as one advance; stale cursors cannot credit again. ER debits cannot exceed the holding. | 9 / 5 |
| [SOL batch settlement](batch_settlement.rs) | For up to seven accounts, totals fit in `u128`, every target respects its rent floor, and the batch neither creates nor loses lamports. A guarded retry does not post a surplus twice. | 11 / 4 |
| [Dispute intervals](dispute_geometry.rs) | Halves meet without a gap, shrink strictly, and differ in width by at most one. Either choice in each of four rounds isolates one of 16 steps. Live turns and timeouts have disjoint slot ranges with no gap at the deadline. | 11 / 5 |
| [V2 ingress prefixes](continuity_prefix.rs) | A checkpoint consumes a bounded prefix starting at the persisted count and root. It cannot skip, rewind, exceed available ingress or exceed its step budget. An advancing prefix cannot be consumed twice from the resulting head. | 6 / 4 |

### Custody: what backs what?

Let `N = deposited − withdrawn`, over mathematical integers, and `V` be the token **amount** in the registered SPL vault account. Initialization sets `N = 0`. From `0 ≤ N ≤ V`, every confirmed bridge deposit or payout preserves `0 ≤ N′ ≤ V′` and `V′ − N′ = V − N`. A direct SPL donation can raise the surplus without increasing `N`. No positive posting accepts an unchanged vault or unchanged counterparty balance. Counter wraparound, insufficient funds and credits that would overflow cannot produce a payment plan.

The [SBF adapter](../../token-bridge/src/processor/custody_adapter.rs) reads both accounts using Pinocchio's SPL-owner- and length-checked account view. [Deposit, direct withdrawal and settled withdrawal](../../token-bridge/src/processor.rs) reread the same accounts after real `TransferChecked` and expose the proposed counters only on an exact match. No CPI-success assumption substitutes for this observation in the theorem. The instructions otherwise keep their signer, mint, PDA and authorization checks. The unchanged-counter replay branch does not transfer or write a vault. These changes add SPL balance reads and make exact two-sided movement a condition of posting; they do not introduce an underfunded-vault recovery policy.

**Limit:** The custody component alone covers `N`, not the holders. The [composed proof](LIABILITIES.md) adds the holder, uncredited and unpaid terms and explains the assumptions needed to identify them. Neither theorem establishes custody against outside reductions, permanent vault control, Token-2022 behavior, or that all holders can redeem. They also do not prove correctness of SPL, account decoding or SVM rollback.

### Credits, batch writes and checkpoint decisions

- A deposit advances the cumulative receipt. It produces an immediate ER holding only when the account is not delegated. On the deferred path, `origin ≤ cursor < target ≤ available` and sufficient `u64` capacity are necessary and sufficient for an arithmetic credit. Both the new cursor and the holding delta use the same target. The [ER adapter](../../token-bridge/src/processor/token_deposit.rs) separately validates the account, origin and cursor PDAs; V2 also checks the ingress tuple. Replay safety assumes the cursor is for this origin and is persisted. Direct withdrawal and `StartWithdrawal` use the same verified ER debit.
- [SOL batch settlement](../src/instructions/settlement.rs) authenticates accounts and checks distinct addresses before passing amounts and rent floors to the verified tally. Unguarded writes require equal totals. Guarded writes credit only the surplus to the fee vault; an overflowing fee-vault credit is rejected. An already-applied accumulator is a no-op even if a donation subsequently changes balances. Comparing the digest is proved; hashing, account bindings and bank storage are not.
- [Disputes](../src/instructions/checkpoint.rs) use the same interval arithmetic in response, selection and proof entry, and the same strict deadline predicate in response and timeout. This is interval and timing safety for the supported handlers, not a proof of DA, trace roots, challenger entitlement, an honest response arriving, or V2 dispute resolution. V2's separate challenge-entry restriction is unchanged.
- [V2 proposal and commit](../src/instructions/continuity.rs) both check the prefix against the current session and checkpoint step budget. A zero-length prefix must preserve its root; an endpoint at the known tip must match the tip root. Commit rechecks these facts rather than relying only on proposal-time checks. Intermediate prefix authentication, owner/PDA checks, record serialization and hash commitments remain adapter obligations.

## Independent reproduction

Use the [pinned Verus setup](README.md#reproduce) with bundled Z3. From the repository root:

```bash
set -eu
for suite in custody credit batch range prefix liability wire schedule entitlement provenance audit; do
  python3 northstar/programs/portal/proofs/check.py --suite "$suite" \
    --verus "$VERUS" --logs "target/proofs/$suite"
  cmp "target/proofs/$suite/summary.json" \
      "northstar/programs/portal/proofs/evidence/extended/$suite.json"
done
```

The [CI matrix](../../../../.github/workflows/verify-portal-bonds.yml) runs all fifteen suites under `--no-cheating`. It checks whole-crate results and named functions and compares the proof input hashes and output counts with the recorded summaries. A negative control is accepted only for a specific failed production postcondition, never a compilation error or timeout. The [custody](evidence/extended/run.txt), [payout](evidence/extended/payouts.txt) and [entitlement](evidence/extended/entitlement-run.txt) records identify the respective program revisions and local SBF artifacts.

For execution, use the default and V2 [SBF build and test commands](SETTLEMENT.md#reproduction-and-independent-review). To run the additional 36 V2 bank regressions, first build the test-only CPI fixture into the **same V2 output directory**:

```bash
out="$PWD/target/accounting-v2"  # directory containing the separately built V2 Portal and bridge
cargo build-sbf --tools-version v1.56 --arch v0 --sbf-out-dir "$out" \
  --manifest-path northstar/programs/continuity-fixture/Cargo.toml -- --locked
BPF_OUT_DIR="$out" TOKEN_BRIDGE_BPF_OUT_DIR="$out" cargo test --locked \
  -p northstar --features checkpoint-continuity-v2 --lib -- regression_v2_ --test-threads=2
```

Execution included a full seven-account batch and replay, a vault payout with a real SPL donation left over, failed-token-CPI rollback, two authorized V2 deposits with unchanged counters on rejection, and an ER CPI withdrawal settled on L1 in a test bank. These are local regressions, not live-validator, production-deployment or independent-auditor results.
