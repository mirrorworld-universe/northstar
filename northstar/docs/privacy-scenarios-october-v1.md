# Privacy scenarios: October technical contribution v1

**Assessment date:** 2026-10-23. **Status:** draft for Baixiong, Bilal, and Jonas; not published, team-reviewed, or implementation acceptance. Baseline: Northstar `34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52`.

## Recommendation and scope

Start with **temporary secrecy for simultaneous game moves**, using client-side salted commitments followed by public reveal. This tests a concrete privacy requirement without adding hardware trust, secret custody, confidential settlement, or a new proof relation. Persistent hidden state is a different project.

The following are distinct promises:

- **Confidential execution:** designated inputs/state remain secret while computation uses them. Specify whether the operator can read them.
- **Transaction privacy:** hide transaction content, sender/recipient linkage, or submission timing. Hiding account reads alone does not provide these properties.
- **Data availability (DA):** authorized reconstructors can retrieve authenticated inputs through the dispute/recovery period. Available ciphertext is not necessarily an available replay witness.
- **Confidential settlement:** the public L1 transition does not disclose protected balances, amounts, or state. Ordinary plaintext settlement loses confidentiality, even if execution was private.

Attestation authenticates a hardware/software identity under its trust policy. **Attestation is not execution validity.** Even a ZK proof of quote verification does not prove an SVM state transition.

## Northstar-specific constraints

1. **Delegation cannot erase earlier disclosure.** Previously public L1 account bytes remain known after delegation. Secrets must originate privately, for example on the client; moving public accounts into a TEE cannot make their history secret.
2. **Current executable proof coverage is narrow.** [Replay admission][N1] accepts a legacy transaction with three keys, one signature, and one instruction. [The checker][N2] requires two mutable accounts, the fixed account write, and one allowed memcpy syscall. Neither generic games nor the privacy scenarios below inherit execution proofs. The broader proof specification is not evidence of implemented coverage.
3. **Commitments do not hide every export.** [DA pages][N3] include transaction/effect bytes; account entries include public metadata and data hashes. [Recovery records][N4] serialize account data. [Replay witnesses][N5] include account bytes, VM memory, and instructions. Hashing a small unsalted secret permits enumeration; hashing does not fix plaintext elsewhere.
4. **Settlement is another disclosure boundary.** [Portal writes supplied chunks into L1 accounts][N6]. Retaining secret plaintext in those chunks exposes it. Encrypting chunks requires application semantics and an authenticated private-state lifecycle, not a transport flag.
5. **Private disputes need a contract.** If only the sequencer has plaintext/keys, an independent challenger may be unable to replay. Releasing keys during disputes changes the privacy promise. Any future private profile must specify who reconstructs what, by when, and what happens on withheld data—without weakening today's checks.

## Six concrete scenarios

The “first users” below are recruitment hypotheses, not interviews or integration commitments.

### 1. Simultaneous game moves: proceed to a bounded PoC

- **Confidential data:** each player's next move, only until the round's commitment phase closes.
- **Why plaintext is insufficient / commitments suffice:** an early public move gives the later player extra information. A domain-bound commitment with a fresh 32-byte random salt hides a small move space until reveal. All moves may become public afterward.
- **First users:** a Northstar game team with short simultaneous rounds and no need to hide completed rounds.
- **Feasibility and primitives:** high for an isolated, no-funds runtime PoC: client randomness, canonical hash encoding, signed commits/reveals, phase guards, and deterministic public replay. No confidential executor is needed.
- **Trust assumptions:** client entropy and secret retention; operator inclusion/ordering and runtime correctness remain trusted. Public metadata remains visible. The last revealer can withhold; commit/reveal is not fair guaranteed completion.
- **Proceed/reject reasoning:** proceed only when eventual full disclosure and explicit aborted rounds are acceptable. Reject this design for cash wagers, permanent secrecy, or claims of operator-independent finality. This PoC does not fit the current accepted replay relation.

### 2. Hidden cards or persistent fog-of-war: defer

- **Confidential data:** deck/order, unrevealed hands, private positions, and role-specific observations over multiple turns.
- **Why plaintext is insufficient:** disclosure before a later turn changes the game. Revealing all secrets after every transition is not acceptable, so ordinary commit/reveal does not implement the required game.
- **First users:** multiplayer card/strategy teams already operating ER sessions.
- **Feasibility and primitives:** a TEE could run familiar game logic with attested client channels and role-based reads; MPC or a dedicated ZK game relation may avoid one executor seeing all secrets. All require fresh-state binding, encrypted recovery, controlled outputs, and a defined challenge witness.
- **Trust assumptions:** TEE hardware/firmware, measured application, key-release policy, and availability; or an explicit MPC non-collusion/correctness model. Players can always disclose their own observations.
- **Proceed/reject reasoning:** useful future candidate only after a team supplies an actual game and accepted disclosure policy. Defer November implementation: persistent secret recovery and independent disputes are unresolved, not solved by an RPC permission list.

### 3. Sealed RFQ or auction with losing bids kept secret: conditional research

- **Confidential data:** quote sizes/prices and losing bids after clearing; identities may still be public.
- **Why plaintext is insufficient:** reveal-all auctions expose trading intent and losing quotes. Commit/reveal suffices only if the product accepts eventual disclosure. Winner-only output needs computation over undisclosed bids.
- **First users:** an ER application with recurring auctions and at least two independent quoting participants; none has been recruited here.
- **Feasibility and primitives:** fixed-size encrypted comparison via MPC or an attested matching service; bind signer, auction id, order version, input cutoff, tie rule, and callback to the same request. Funding, escrow, and asset delivery are separate work. [Arcium's example][A1] implements an encrypted comparison/callback pattern, not funded settlement.
- **Trust assumptions:** cluster privacy/correctness/availability or TEE policy; authenticated callback origin; output leakage. In a second-price auction the clearing price itself reveals one losing bid.
- **Proceed/reject reasoning:** retain as a demand-dependent follow-up, not the first PoC. Require explicit settlement semantics and measured queue-to-callback time. Do not assume an L1 coordinator can be delegated to ER or that one confidential result proves Northstar execution.

### 4. Protected trading submission: reject broad privacy claims

- **Confidential data:** pending order contents before the execution decision; sometimes wallet linkage or timing.
- **Why plaintext is insufficient / commitments are limited:** a public pending order exposes intent. A hash alone cannot execute an unknown order; later reveal can reintroduce exposure at the decision boundary.
- **First users:** ER exchanges and quoting firms with a measurable pending-intent problem, rather than users seeking generic anonymity.
- **Feasibility and primitives:** encrypted authenticated ingress terminating inside an approved executor, private admission/queues, restricted simulation/logs, and a specified disclosure point. Operator-blind submission needs more than TLS to an ordinary RPC. Sender anonymity additionally needs a distinct routing/relaying model.
- **Trust assumptions:** executor and channel binding, metadata handling, client software, inclusion and ordering policy. Confidentiality does not prove fair ordering or prevent omission.
- **Proceed/reject reasoning:** a clearly labeled operator-trusted private endpoint could be a separate product, but reject “operator-blind,” “anonymous,” or “MEV-free” claims from endpoint access control alone. No November dependency on a private trading/settlement service.

### 5. Tournament ballots or cohort analytics: conditional, outside the fast path

- **Confidential data:** individual votes or player statistics; publish only an aggregate for an agreed cohort.
- **Why plaintext is insufficient:** a public per-user transcript defeats individual confidentiality. Commitments alone cannot compute a secret tally. If participants accept reveal-all, ordinary signed aggregation is simpler.
- **First users:** tournament organizers or game teams that can identify a decision requiring an aggregate, not hypothetical sensitive-data warehouses.
- **Feasibility and primitives:** bounded MPC sum/count or a dedicated private-witness proof; participant authorization, deduplication, fixed query policy, and minimum cohort size. Require authenticated input-to-output binding; proof of a sum does not prove that self-reported inputs are true.
- **Trust assumptions:** MPC policy or TEE trust; cohort and output design. Repeated overlapping queries can reveal individual values despite encrypted computation.
- **Proceed/reject reasoning:** promising low-frequency comparison workload if a user exists; defer as a second experiment. Reject tiny cohorts, arbitrary repeated queries, and real personal records in an initial test. This is confidential computation, not confidential asset settlement.

### 6. Private rewards/payroll: reject as a near-term ER feature

- **Confidential data:** recipient amounts/balances; some users also require recipient identity and payer-recipient unlinkability.
- **Why plaintext is insufficient:** public SOL/token payouts reveal amounts. Amount commitments alone cannot authorize conserved private spends without a compatible asset protocol.
- **First users:** game reward distributors or treasury operators with explicit amount-privacy requirements. Regulatory and wallet constraints need their review.
- **Feasibility and primitives:** [Token-2022 confidential balances][T1] provides an amount/balance direction using encrypted balances and proofs; public addresses remain visible. Integration still needs mint/extension compatibility, proof generation, account approval, optional auditor policy, wallet recovery, and separately reviewed bridge/withdrawal behavior.
- **Trust assumptions:** the chosen token protocol, key holders, client correctness, and any configured auditor. Deposits/withdrawals and timing can disclose or correlate value.
- **Proceed/reject reasoning:** private settlement is a separate asset project. Reject a Portal wrapper or hidden RPC balance as sufficient. No current confidential-token ER bridge or accepted Northstar proof coverage is established by this research.

## Proposed November PoC: disclosure-bounded simultaneous rounds

**Proposal only; requires human scope approval.** One engineer for **6–9 working days**, plus **1–2 reviewer-days**; an estimate, not measured delivery. Use two synthetic clients and an isolated local ER with no delegated user funds. Start with an offline reference state machine. Do not use hosted services, TEE hardware, new proof keys, private settlement, or real wagers.

Dependencies: one game team confirms eventual disclosure is acceptable; a reviewer approves the phase/timeout rules; a local ER fixture can expose RPC/history and recovery artifacts for inspection. If a secret-bearing export cannot be inspected, record that surface as unvalidated rather than claiming confidentiality.

Suggested fixed encoding: `SHA-256(domain || unique_session_id || round_u64 || player_pubkey || move_u8 || salt_32)`. Fix domain bytes, field widths, byte order, and session uniqueness in a reviewed test vector. Salt stays client-side until reveal; never log it early. Lock both commitments before accepting reveals. Abort incomplete rounds under an explicit timeout; do not describe this as fair exchange. A stopped ER requires a wall-clock test-run abort, not a claim that ER slots keep advancing.

| Work | Estimate | Required result |
| --- | ---: | --- |
| Freeze threat model, encoding, reference vectors | 1 day | Named observers, allowed metadata, exact reveal boundary |
| Client/reference harness and isolated ER adapter | 3–4 days | Explicit commit, reveal, completed, and aborted phases |
| Export inspection, invariant checks, restart exercise | 1–2 days | Reproducible artifact bundle, including failed cases |
| Independent replay and review handoff | 1–2 days | Reviewer can reconstruct disclosed rounds without sequencer files |

**Go only if all tests pass:**

1. Run 100 completed synthetic rounds; exactly one terminal result per round; independent public replay matches every result.
2. Before reveal, no field in RPC/history, transaction bytes, logs, checkpoint pages, journals, or any generated trace carries the unrevealed move or salt. Use unique salt canaries plus decoded-field inspection; a one-byte move can occur coincidentally, and absence of string matches alone is not a confidentiality proof. The client holds the only intended secret copy.
3. Wrong move/salt, wrong player/session/round, duplicate submissions, late commits, early reveals, and malformed encodings reject without changing the committed phase. Post-lock commitments cannot be replaced.
4. A missing reveal reaches a labeled aborted result; restart preserves phase/commitments without premature disclosure. Report the selective-abort limitation, not a fairness pass.
5. Disclosed transcripts stay retrievable through the agreed replay window. No proof is emitted or accepted for this unsupported game profile; retain fail-closed regression evidence.
6. Report submit-to-observed-commit and reveal-to-result p50/p95 separately with machine/runtime identities. A game owner must set an acceptable latency budget before evaluating those numbers; no target is claimed here.

**No-go:** a user needs persistent secrets, sealed losers, private recipients, fair completion against withholding, or provable settlement. Stop and return a new architectural decision; do not escalate this PoC into a TEE/MPC/Portal project implicitly.

## Source references

Northstar references are pinned implementation evidence, not deployment evidence. External sources were accessed 2026-10-23. Provider evidence classifications and source dates appear in [the companion assessment](privacy-market-assessment-october-v1.md).

[N1]: https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/runtime/src/bank/er_replay.rs#L33-L58
[N2]: https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/transaction-proof/src/lib.rs#L235-L379
[N3]: https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/src/checkpoint.rs#L55-L147
[N4]: https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/src/unsettled_state.rs#L91-L111
[N5]: https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/transaction-proof/src/lib.rs#L123-L157
[N6]: https://github.com/mirrorworld-universe/northstar/blob/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52/northstar/programs/portal/src/instructions/settlement.rs#L335-L392
[A1]: https://github.com/arcium-hq/examples/blob/fe15384c239ca90fbbcd8259b60b332275641e57/sealed_bid_auction/README.md
[T1]: https://solana.com/docs/tokens/extensions/confidential-transfer
