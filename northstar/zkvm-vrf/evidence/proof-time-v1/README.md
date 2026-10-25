# Proof-time diagnostics — target not met

See the [report](../../../docs/vrf-proof-time-followup.md). This package contains **old-input characterization**, not fresh configured-session acceptance.

- `summary.json`: hardware, identities, pending gates and the best diagnostic.
- `attempts.json`: all 16 follow-up diagnostic runs, 15 SDK-verified proofs, incomplete/failed attempts, separate phase costs and sampled resources.
- `guest-controls.json`: 17 actual guest controls, two valid rollback executions, 22 native differential inputs and 12 source-association checks.
- `timing.json`: non-additive timing-span unions for the best sample.
- `best-diagnostic.env`: recorded environment, not a launcher. Its generic element threshold is overridden by the pinned GPU builder; retain supported geometry and all verification.
- `best-proof/`: actual SDK-verified proof, onchain-format bytes and exact 256-byte output. Hashes are in `summary.json`.
- `checksums.json`: package byte inventory, excluding itself.

The selected diagnostic guest/key is [separately packaged](../../../programs/portal/keys/proof-time-wordwise-v3/README.md), **not installed**. Its program key differs from the historical session's configured key. The proof uses the existing [request witness](../g5-a10g-v11/cases/caseA-request/witness.borsh.gz); do not treat it as a new session, submit it to the old resolved session, or claim Portal acceptance from SDK verification.

Best measured prove+wrap: **142137 ms**, followed by **490 ms** SDK verification. Three fresh request/callback/hard-window proofs at <=120000 ms remain uncompleted. Cold means a new process, not an emptied filesystem cache. Full and key-only preload regressed from paging; larger GPU shards were rejected by the canonical recursion-key allowlist. No check was relaxed.

Run the read-only consistency check from the repository root:

```sh
python3 northstar/zkvm-vrf-client/tools/check-proof-time-evidence.py
```

It checks bytes, identities, source inventory and control counts; it does not perform new cryptographic verification or grant acceptance.

Raw operational logs, executable build-tree strings and private signers are excluded. Hashes and tests support artifact consistency; independent human review/reproduction remains pending.
