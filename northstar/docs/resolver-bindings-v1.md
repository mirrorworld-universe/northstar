# Real-proof resolver binding checks

The live harness retains the seven Portal accounts immediately before successful production-mode resolution, plus the captured L1 slot and ER slot. A fresh real GPU run resolved in 124.787s at 127,976 CU. Its proof, witness, measurements and account snapshot are under [`evidence/resolver-v1`](../zkvm-replay/evidence/resolver-v1/).

The binary snapshot is bincode's encoding of `(u64, u64, Vec<(Pubkey, Account)>)`, with accounts ordered as session, checkpoint, challenge, DA proof, step proof, bond recipient and checkpoint cursor. It contains public account state, not signing keys. Decoder dependencies are pinned by Cargo.lock.

Two CPU-only tests execute real Portal SBF:

- **31 resolver cases:** an unchanged real-proof control; session/proof PDA checks; account owner checks; checkpoint/challenge/authority binding; seal, length and proof/public-input hashes; proof kind/version, session context, transaction effect, readonly/settlement roots, challenge pre/post roots, turn/range, DA status and active cursor. Guard failures must return the specified Portal error and preserve every protocol account exactly. Changed step index and a changed proof with refreshed byte hash must take the invalid-proof outcome, with exact bond transfer, terminal states and cursor clearing.
- **10 creation cases:** an authenticated transaction path control and changed sibling, length, effect root, step, context, readonly/settlement root, PDA and authority. Rejected creation must not allocate the proof account or change the other protocol accounts.

The creation test binds the challenge to its fresh test signer; its successful control must reproduce the retained public-input hash. Resolver tests keep captured identities unchanged and use a fresh, separately funded submitter. Transactions have distinct signatures so a prior result cannot satisfy a later case through deduplication.

ProgramTest sets its clock to the captured slot. These are deterministic state-invariant tests, not new wall-clock recovery measurements or live slot-warp claims. They do not enumerate every possible byte mutation.

Run after building the prototype Portal SBF:

```sh
BPF_OUT_DIR="$PWD/target/deploy" cargo test --locked -p northstar-portal \
  --features zk-verifier-prototype --test zk_verifier resolver::
```

Both tests are included in the existing CPU-only SBF CI step. No new verifier default, on-chain ABI or production behavior change is introduced.
