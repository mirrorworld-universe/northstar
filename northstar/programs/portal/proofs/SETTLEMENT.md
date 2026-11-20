# SOL receipt and token bridge verification

**Local result, 2026-11-20.** Verus with Z3 checked three more modules used by the production programs. The source and SBF regression revision is `a58b9f88e4639d20e397a7c8c44aa5a9f9d3f380`. The proofs cover decisions over decoded account state. Program tests separately exercise decoding, persistence and cross-program invocation (CPI). Neither is a proof of a deployed binary.

| Suite | Production code | Whole proof crate | Negative controls rejected |
| --- | --- | ---: | ---: |
| [SOL receipt](sol_receipt.rs) | [Receipt binding, delta and payment](../src/receipt.rs) | 9 verified, 0 errors | 6/6 |
| [Token withdrawal authorization](token_authorization.rs) | [Mutable consumption bit](../src/withdrawal_auth.rs) | 3 verified, 0 errors | 3/3 |
| [Token bridge](../../token-bridge/proofs/token_accounting.rs) | [ER transfer and vault counter](../../token-bridge/src/accounting.rs) | 5 verified, 0 errors | 6/6 |

The counts are Verus results, not counts of financial guarantees. Each proof crate imports the production Rust file by path; none uses a copy of the algorithm or a token-program model. Verification attributes do not select different executable logic. The earlier [bond suite](README.md) still gives 8 verified, 0 errors with all three of its own controls rejected.

## Claims and boundaries

**Portal SOL receipt.** The [handler](../src/instructions/settle_deposit_receipt.rs) authenticates the validator and active settlement, checks slot/checksum and the requested L1 recipient, derives the receipt PDA, and decodes its [frame](../src/continuity.rs). On these decoded inputs the verifier established:

- Receipt identity comparison succeeds exactly when the session, ER recipient and bump match. The ER recipient in this comparison is the **source of the withdrawal**, not necessarily the L1 payout destination.
- The same balance and an equal or older withdrawal counter yield a no-op, irrespective of the claimed payout. Otherwise a permissible update requires `next.withdrawn == old.withdrawn + claimed_payout` over mathematical integers. A changed balance with an unchanged counter can be an update with zero payout; it is not a replay.
- An update's payment plan exists exactly when `rent + next.balance + payout <= receipt_lamports`, the recipient credit fits in `u64`, any positive payout uses distinct receipt and recipient accounts, and the frame's optional cap holds. Both sides move by the same amount. The receipt retains rent **and** the new recorded balance. V2 additionally requires `next.balance + next.withdrawn <= total_deposited` without wraparound; legacy receipts have no `total_deposited` field.
- Once the accepted counters are persisted, resubmitting them cannot produce a second payment. The proof includes reachable nonzero payout, zero-payout update and stale no-op cases. The adapter returns on no-op before the lamport writes, receipt write and checksum update.

**Portal token authorization.** The [consume handler](../src/instructions/accumulate_token_withdrawal.rs) validates account ownership, the validator and signing vault, settled checkpoint, bridge, authorization PDA and discriminator. It computes the digest of the withdrawal tuple and passes the actual decoded `consumed` field by mutable reference. The verified transition accepts exactly an unconsumed authorization with matching checkpoint bytes, tuple digest and bump. Success sets the bit; rejection preserves it. For every key, the matching first use succeeds and every retry against the resulting bit fails. This binds to a **digest**: the proof does not establish cryptographic collision resistance, PDA derivation or the correctness of the tuple encoder.

**Token bridge.** The [handlers](../../token-bridge/src/processor.rs) use the same functions as the verifier:

- For an ER `Transfer`, the access check requires a signer and distinct addresses. For the decoded holdings, balances are produced exactly when the authority matches the source owner, both holdings share the same bridge and mint, the debit is available and the credit fits. The exact debit and credit preserve the two-account sum, even when that sum exceeds `u64::MAX` as a mathematical integer. This is an internal ER balance transfer, not an SPL CPI.
- For `SettleWithdrawal`, an equal or older target returns no work. An advancing target is admitted exactly when `target == stored + amount` and `target <= deposited`; overflow cannot advance the counter. After a persisted advance, retrying that target cannot request another transfer. Returning no work does not attest that previously stored counters were valid.
- In the handler the no-work branch precedes both CPIs. A new payment requires successful Portal consumption and SPL `TransferChecked` before the vault counter is stored. This ordering and transaction rollback are **execution-tested**, not SMT-proved. The legacy direct `Withdraw` handler and deposit-side solvency are not covered by the counter proof.

## Evidence from program execution

- [Receipt SBF tests](../tests/state_invariants/receipts.rs) use the bank rent value and a fee payer separate from the observed payment accounts. They assert exact balances, serialized receipt/frame and session accumulator on payment. Wrong deltas, insufficient backing, payment to the receipt itself and an over-cap V2 update preserve the observed accounts. Exact and stale retries leave the accumulator and balances unchanged. Both legacy and scoped frames run under both program builds.
- [Bridge SBF tests](../../token-bridge/tests/core/accounting.rs) compare both complete ER accounts on transfer and rejection. The withdrawal test begins with seeded committed checkpoint, registration and vault state. Real Portal instructions create the authorization and finish settlement. Changing the authorized destination is rejected by Portal before SPL is invoked. A frozen SPL destination then makes the token CPI fail **after Portal consumption succeeds**; the authorization, vault and both SPL accounts roll back. After a normal SPL thaw, the same withdrawal moves exactly six base units and consumes the authorization. A fresh signed retry invokes neither program and changes none of the four accounts. The fee payer is not included in rollback assertions.
- This uses `solana-program-test` and the [repository's P-Token 1.0.0 binary at the SPL Token address](../../../../program-binaries/src/lib.rs), not a successful-CPI stub. The fixture starts after deposits and checkpoint adjudication, so those steps are not evidence from this test.

See the [build and test record](evidence/settlement/run.txt) and the runner's summaries for [receipts](evidence/settlement/receipt.json), [authorizations](evidence/settlement/authorization.json) and the [bridge](evidence/settlement/bridge.json). Each summary includes the source hashes, tool versions and results of isolated mutations. Controls change only temporary copies. A control must fail the intended production postcondition, with exactly one verification error. A timeout, missing result, build error or failed witness is not an accepted control.

## Reproduction and independent review

Use the SHA-256-checked Verus release and bundled Z3 described in the [tool setup](README.md#reproduce). Run from the validator repository root, in an approved environment:

```bash
set -eu
# VERUS points to the release's verus executable, next to its bundled z3.
for suite in receipt authorization bridge; do
  python3 northstar/programs/portal/proofs/check.py --suite "$suite" \
    --verus "$VERUS" --logs "target/proofs/$suite"
  cmp "target/proofs/$suite/summary.json" \
    "northstar/programs/portal/proofs/evidence/settlement/$suite.json"
done
```

The [CI matrix](../../../../.github/workflows/verify-portal-bonds.yml) runs these suites and the previous bond suite. Always inspect source and specification together when regenerating evidence. The runner checks whole-crate success under `--no-cheating`, the expected named functions and verifier counts. Raw JSON and diagnostics are retained in the selected log directory.

To run the same ordinary build and test matrix, use Bash with cargo-build-sbf 4.5.0 and platform-tools v1.56:

```bash
set -eu
for profile in default v2; do
  features=(); filter=()
  if [[ $profile == v2 ]]; then features=(--features checkpoint-continuity-v2); filter=(accounting::); fi
  out="$PWD/target/accounting-$profile"
  for program in portal token-bridge; do
    cargo build-sbf --tools-version v1.56 --arch v0 --sbf-out-dir "$out" \
      --manifest-path "northstar/programs/$program/Cargo.toml" "${features[@]}" -- --locked
  done
  BPF_OUT_DIR="$out" cargo test --locked -p northstar-portal "${features[@]}" -- --test-threads=2
  BPF_OUT_DIR="$out" cargo test --locked -p northstar-token-bridge "${features[@]}" --lib
  BPF_OUT_DIR="$out" cargo test --locked -p northstar-token-bridge "${features[@]}" --test core -- "${filter[@]}" --test-threads=2
done
```

For pre-mounted platform-tools, use the build-only PATH/RUSTC settings and flags in the [bond reproduction notes](README.md#reproduce). Keep the two builds in separate directories and build Portal separately, not as a `no-entrypoint` library in the bridge build.

**Remaining trust boundary.** These are safety properties, not payout liveness or proof of entitlement. The adapters, account aliasing and privileges, Borsh, runtime rent and rollback, compiler, Verus/vstd and Z3 remain trusted to the extent described above. The tests exercise specific inputs, not every possible CPI or account layout. No live deployment was compared, and independent reproduction and review are pending. The next useful extension is a cross-instruction vault-backing invariant relating actual SPL custody to net deposits and withdrawals, not just the stored counters.
