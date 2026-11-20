# Portal checkpoint-bond proof

Start with the [nine-suite verification map](INVARIANTS.md) for Portal and the SPL bridge. The [settlement record](SETTLEMENT.md) covers SOL receipts, token authorizations and ER transfers; this page covers checkpoint bonds.

**Result, 2026-11-19:** Verus with Z3 verified the executable accounting used by Portal’s release and slash paths. The verifier reported **8 verified, 0 errors**; this is its count of verified functions, not eight independent financial guarantees. Three intentionally incorrect variants each failed the planner’s postcondition. See the [recorded run](evidence/run.txt) and [source hashes and control results](evidence/summary.json). Local reproduction is complete; independent review is pending.

## What was proved

The [proof crate](checkpoint_bond.rs) imports the [production planner](../src/bond.rs) by path. There is no second implementation. For all `u64` inputs, both actions, every stored bond status, and both account-alias cases:

- A payment plan exists **if and only if** the accounts are distinct, the status is `Locked`, the bond is positive, the checkpoint covers `rent + bond`, and the recipient credit fits in `u64`. The specification uses mathematical integers, not wrapping arithmetic.
- On success, exactly the bond is debited and credited; the total is unchanged, and the checkpoint retains at least the supplied rent floor. Release yields `Released`; slash yields `Slashed`.
- Reusing either resulting status cannot yield another payment, even if the second call changes action, rent, bond, or recipient balance. This is **at most once**, not a claim that every bond eventually resolves.
- Every rejected input returns an error, not an effect plan. The contract checks the error category, including overflow in `rent + bond` and in the recipient credit. Both successful terminal paths have a concrete reachability witness.

The [instruction adapter](../src/instructions/checkpoint.rs) reads balances, compares actual account addresses, obtains rent for the actual data length, calls the planner, then applies the two balances and terminal status on success. The release caller checks the proposer’s address. The two callers can request only release or slash. Rust verification attributes and Borsh derives differ between verifier and ordinary builds; **no executable branch** in the planner differs. No proof dependency is linked into Portal or its SBF artifact. Existing on-wire bond tags are unchanged.

## Reproduce

From the validator repository root in an approved development environment, with Rust **1.99.1** available via rustup. For x86-64 Linux, use the upstream [Verus release](https://github.com/verus-lang/verus/releases/tag/release/0.2026.11.02.8bf68ee) with its bundled **Z3 4.16.0**:

```bash
version=0.2026.11.02.8bf68ee
tools=$(mktemp -d)
curl -fsSL --retry 2 "https://github.com/verus-lang/verus/releases/download/release/$version/verus-$version-x86-linux.zip" -o "$tools/verus.zip"
(cd "$tools" && echo 'f3a0119292df8855e4435da8130bee1873b0e6a60c306accf81a08ea687bba3c  verus.zip' | sha256sum --check --strict - && unzip -q verus.zip)
python3 northstar/programs/portal/proofs/check.py \
  --verus "$tools/verus-x86-linux/verus" --logs target/bond-proofs
```

The [runner](check.py) checks the verifier commit and solver version, uses `--no-cheating`, verifies the entire proof crate, then runs three controls against temporary copies. None writes into the production source tree. A control passes only if verification **fails on the planner’s postcondition**; compiler failures, timeouts, or missing results do not count. Compare `target/bond-proofs/summary.json` with the recorded summary. Raw Verus JSON and stderr are retained there. The [CI workflow](../../../../.github/workflows/verify-portal-bonds.yml) runs the same command on Portal changes.

Build and test the ordinary program separately (Bash):

```bash
set -e
for profile in default v2; do
  features=()
  if [[ $profile == v2 ]]; then features=(--features checkpoint-continuity-v2); fi
  out="$PWD/target/bond-$profile"
  cargo build-sbf --arch v0 --tools-version v1.56 --manifest-path northstar/programs/portal/Cargo.toml \
    --sbf-out-dir "$out" "${features[@]}" -- --locked
  BPF_OUT_DIR="$out" cargo test --locked -p northstar-portal "${features[@]}" -- --test-threads=2
done
```

Builder: `cargo-build-sbf` **4.5.0**. If using already mounted platform-tools rather than rustup’s custom toolchain link, prepend the v1.56 `rust/bin` to `PATH`, set `RUSTC` to its `rustc`, and add `--no-rustup-override --skip-tools-install` **to the build command only**. This was the local build route; the program tests use the ordinary workspace compiler.

## What remains outside the proof

The trusted boundary includes Verus/vstd’s specifications for Rust primitives, Z3, the compiler, the Pinocchio account views, the address/alias comparison, runtime rent and transaction atomicity, Borsh, instruction validation, and persistence of the returned status. The theorem assumes a retry for the same bond starts from its persisted status. It does not prove that the chosen slash recipient is entitled to the bond, that dispute adjudication is correct, or that the code deployed at any address matches this build. It does not cover SOL receipt settlement or the SPL bridge.

[Program tests](../tests/integration.rs) cover release on commit/cancel, slashing on a timeout, and retry. The strengthened release test checks the caller rejects an aliased proposer account; this tests the binding at the wrapper, not which inner guard fires. Raising the Rent sysvar makes the previously sufficient checkpoint balance insufficient. Both rejected transactions preserve checkpoint, cursor, proposer and committer state, roll back a preceding instruction, and charge the fee payer exactly the quoted fee. Restoring rent allows the same bond to be paid. The [wire test](../src/state.rs) checks all three Borsh tags and rejects an unknown one.

**Local program results:** 62 tests passed in each of the default and `checkpoint-continuity-v2` builds. The same external live-validator test was already marked ignored in each; no live deployment was tested. These are regression results across two build profiles, not 124 distinct properties. The [subsequent receipt proof](SETTLEMENT.md) covers the second accounting component; independent reproduction and review remain outstanding.
