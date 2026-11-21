# Do assets cover the bridge’s outstanding claims?

**For a complete, initially balanced inventory, the verified transitions preserve** `H + U + P = D − W ≤ V`, separately for each bridge, session registration and mint. Here `D` and `W` are the L1 vault's posted cumulative deposits and withdrawals; `V` is its actual SPL token balance. The three liabilities are:

- **H — spendable holdings.** Count the canonical active copy: the L1 holding when not delegated, the ER holding when delegated. Do not count the locked mirror, delegation buffer or checkpoint snapshot again.
- **U — funded but uncredited deposits.** For a delegated holding, count the cumulative L1 deposit receipt less its applied ER cursor (the delegation origin before the first credit). An immediate deposit is already in H.
- **P — debited but unpaid withdrawals.** Count one outstanding effect per withdrawal. Creating its authorization does not create a second claim. Retire the effect only for a posted vault counter and the consumed authorization for this checkpoint and tuple. During partial settlement the journal can still contain already-paid events; its raw sum is not P.

For example, a 42-token credit followed by a 20-token ER withdrawal leaves 22 spendable and 20 unpaid. Posting that payout leaves 22 in both custody and liabilities. A later deposit of 7 increases liabilities to 29 **before** the ER sees it: 22 spendable plus 7 uncredited. Applying the credit changes only the split.

## What is enforced

[RecordedTransitionV2::apply](../../../src/continuity_v2.rs) requires each ordinary ER execution and authenticated token-credit execution to satisfy:

```
sum(touched holdings before) + authenticated ingress
    = sum(touched holdings after) + newly recorded withdrawal effects
```

The [journal adapter](../../../src/continuity_v2/token_liability.rs) groups by bridge program, session bridge and mint. It recognizes live holdings by layout and both canonical PDAs, distinguishes copies at other addresses, and binds every token effect to a touched holding and its registration. The existing state transition checks sort order, unique write keys, committed prestate, ingress continuity and cumulative withdrawals. Untouched holdings cancel on the two sides. The [lineage store](../../../src/lineage_store_v2.rs) uses the same check before accepting a batch and when validating retained history. A failure does not publish the candidate state or change the journal head. This is a validator/replay check, not a new L1 instruction or ZK constraint.

The [payout reconciliation path](../../../src/ephemeral_runtime/token_settlement_v2.rs) uses the verified counter and authorization predicates. A missing or fresh authorization remains unpaid; a mismatched one is invalid evidence even if the counter has advanced. Authentication of the physical account, decoding and tuple hashing remain in the adapter.

**Compatibility:** No wire format, PDA seed, instruction or account layout changes. An inconsistent retained V2 transition that an older validator accepted can now fail replay. An already-finalized base snapshot is not retrospectively re-executed. Preserve existing journals and reconcile them against an authenticated checkpoint before upgrading; this change does not repair or discard history.

## What was proved and tested

The [composed Verus crate](../../token-bridge/proofs/aggregate_liability.rs) imports the production journal arithmetic and payout predicates, the bridge custody and credit planners, the withdrawal counter, and Portal's one-use authorization. It proves the empty-inventory base case and preservation steps for confirmed deposits, applying deferred credit, accepted ER transitions, direct withdrawals, retiring one selected unpaid effect, and value-preserving handoffs or donations. The finite-sum lemma proves that paying one effect removes only its amount; all other effects remain outstanding. The gate accepts exactly the balance equation over any finite input slice; checked arithmetic does not silently treat two overflows as equality.

**Local result (2026-11-21):** 50 verified, 0 errors in the composed crate (28 functions also checked in the component crates). All ten new mutations failed their named production postcondition. The [recorded summary](evidence/extended/liability.json) pins the solver, verifier, flags, ten imported input files and individual outcomes. Reproduce with:

```bash
python3 northstar/programs/portal/proofs/check.py --suite liability \
  --verus "$VERUS" --logs target/proofs/liability
cmp target/proofs/liability/summary.json northstar/programs/portal/proofs/evidence/extended/liability.json
```

Production and test revision: `ed5e843c71c708b96d93cc848db23fec54e292db`. [Journal regressions](../../../src/continuity_v2/tests/liability_tests.rs) first showed that an unmatched debit was accepted and persisted; they now check rejection, unchanged state, successful reopen, mixed transfers/withdrawals, independent vaults, and the authenticated credit budget. The [SBF-backed bank regression](../../../src/ephemeral_runtime/tests/token_settlement_v2.rs) reads the physical receipt, cursor, live ER holding, outstanding events, L1 counters and SPL custody before and after payout, restart and top-up. It checks all three terms against independently observed balances at cuts without a partially paid checkpoint. The separate reconciliation regression checks that the vault counter alone cannot confirm a payment.

Rebuild and select the V2 tests as in the [SBF instructions](INVARIANTS.md#independent-reproduction); use `cargo test --locked -p northstar --lib --features checkpoint-continuity-v2 v2` with the V2 `BPF_OUT_DIR`. Local results: **54 V2 tests passed** (208 filtered); the default Northstar library had **227 passed, six ignored**. Workspace and V2 Clippy passed with `-D warnings`. The rebuilt SBF programs matched the [recorded artifact digests](evidence/extended/run.txt); the new enforcement runs in the validator.

## What this does not establish

A preservation proof is not a census of a deployed cluster. It starts at verified initialization or an independently reconciled cut, requires complete distinct holdings/effects, authentic deposit and handoff inputs, correct correspondence between the selected unpaid effect and its L1 authorization, atomic SPL execution and no independent reduction in vault custody. The PDA/registry and journal adapters, bank, compiler and hash binding are not formally verified here. An aggregate balance also does not prove a particular recipient's entitlement or timely payout. No deployed program or running validator binary was compared; independent deployment and inventory attestation remain open.
