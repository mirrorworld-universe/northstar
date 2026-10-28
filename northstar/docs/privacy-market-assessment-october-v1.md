# Privacy directions: October market/engineering assessment v1

**Assessment date:** 2026-10-23. **Status:** technical review draft for Baixiong, Bilal, and Jonas. This is not procurement advice, deployment approval, customer validation, or completed shared OKR acceptance.

## Decision in one paragraph

Northstar should first establish **how long a real application needs secrecy**, not select a privacy provider. A simultaneous-round game that permits public reveal can test demand and disclosure boundaries without changing the proof or custody model. If users require persistent secrets, compare one TEE application and one bounded MPC computation against a written recovery/dispute policy. Do not turn either result into a claim of confidential, provable SVM settlement. The [scenario assessment](privacy-scenarios-october-v1.md) proposes the November commitment/reveal PoC and its go/no-go tests.

## Evidence labels and limits

- **Deployed evidence:** an identified deployment plus observed behavior/artifacts. No live deployment was independently exercised in this review.
- **Source implementation:** inspected, version-pinned code or example. This does not establish what is deployed or production-ready.
- **Vendor claim:** provider-authored capability, endpoint, availability, or performance statement not independently reproduced here.
- **Proposal:** our suggested direction, not built or accepted behavior.

All external sources below were fetched on **2026-10-23**. Where a publication date was unavailable, the access date is explicit; commit dates are not deployment dates. No paid search, benchmark, public-cluster transaction, tool installation, or customer outreach was performed. A published devnet endpoint or “mainnet live” sentence remains a vendor claim without independent observation.

## Focused capability comparison

### MagicBlock PER: closest runtime shape, different trust boundary

**Vendor claim, documentation accessed 2026-10-23:** [PER introduction][M1] describes Intel TDX confidential ER execution and a devnet endpoint. [Authorization][M2] describes public-by-default accounts, read-access permissions, and authenticated tokens. [Client integration][M3] describes a challenge/quote verification flow separately from wallet authentication.

**Source implementation, commit dated 2026-10-15:** the SDK's [permission structure][M4] binds a permission account to an account pubkey and members. Its [quote helper][M5] verifies quote material and challenge binding. The inspected helper does not receive an expected Northstar application-measurement policy; genuine hardware alone is not an approved runtime. These are concrete integration surfaces, not a reproduced confidential validator deployment.

**Fit:** persistent game state with selective reads, if application users accept the hardware and key-release trust model. Similar SVM vocabulary reduces learning cost, not protocol-integration cost.

**Open boundary:** independently verify approved measurements, fresh quote/channel-key binding, revocation/TCB policy, key recovery, output visibility, and state rollback protection. RPC read permissions do not automatically hide transactions, logs, settlement, or challenge inputs. No latency or confidential-settlement acceptance is inferred from the documentation's “verifiable privacy” wording. A TDX quote is not a Northstar execution proof.

### Arcium: bounded encrypted functions, asynchronous results

**Source implementation, commit dated 2026-07-22:** [the examples][A1] show client encryption, queued computation, MPC, and callback patterns. [The sealed-bid example][A2] keeps running auction state encrypted and reveals the winner and clearing price. Its documented limitations include no escrow/settlement, missing signer binding for the encrypted bidder identity, and sequential state-version requirements. Treat it as a learning example, not an executable financial specification.

**Vendor claim, documentation accessed 2026-10-23:** [core concepts][A3] describes cluster secret shares and privacy with at least one honest node. That privacy statement does not establish availability, accepted output correctness, or an independent Northstar execution proof. [Current limitations][A4] specifies fixed circuit shape and callback transactions constrained by Solana's 1,232-byte transaction limit. [The introduction][A5] says the network is live on mainnet; this review did not verify a deployment or run a computation.

**Fit:** one small secret tally or comparison whose result can arrive asynchronously, rather than arbitrary unmodified SVM execution.

**Open boundary:** document the selected cluster, output authentication/correctness model, non-collusion assumptions, request-to-callback identity, circuit upgrades, nonce/state versioning, and timeout/recovery. A Solana callback coordinator is not automatically an ER-delegatable lifecycle. Local queue-to-callback time and concurrent update behavior need measurement before any real-time game or RFQ commitment.

### dstack / Phala direction: confidential VM tooling, not an SVM protocol

**Source implementation/documentation, commit dated 2026-10-23:** [the repository][D1] supplies confidential-VM tooling, workload attestation, and KMS components. [Its security model][D2] explicitly separates hardware trust, measured OS/application identity, denial of service, and persistent-storage freshness. [The TDX guide][D3] requires measurement and event-log checks, not quote signature verification alone. “Available” platform/service claims in the README are **vendor claims**, not independently tested support here.

**Fit:** reference material for an eventual confidential application service and key-release policy. It is not a drop-in Portal/delegation/settlement adapter, nor evidence that an ordinary GPU proving instance has confidential-computing hardware.

**Open boundary:** encrypted disks do not establish that recovered state is the latest state. Key release must bind an approved workload and state policy; a signed response still needs the transaction, inputs, session, and result bound together. The November PoC should not acquire a confidential VM or introduce a new KMS.

### Token-2022 confidential balances: asset semantics, not general execution privacy

**Source implementation, commit dated 2026-10-23:** [the confidential-transfer interface][T1] defines encrypted balances, ElGamal keys, and optional mint auditor configuration. **Vendor/project claim, primary documentation accessed 2026-10-23:** [Solana's guide][T2] says amounts and balances are private, while token account addresses remain public. That capability was not locally exercised. It also distinguishes public-to-confidential deposits and confidential-to-public withdrawals.

**Fit:** an amount-private rewards/payment product with compatible issuers and wallets. It does not supply hidden game state, sender anonymity, or a confidential Northstar bridge.

**Open boundary:** identify deployed program/features, mint extensions, approval/auditor authority policy, proof contexts, keys, wallet UX, and exit disclosure. No public-cluster feature availability or compatibility with Northstar's bridge was tested; a repository containing this code is not proof that a chosen cluster accepts every instruction.

### Commitment/reveal and a dedicated ZK relation: separate directions

**Proposal:** commitment/reveal handles temporary secrecy where eventual disclosure is acceptable. It avoids provider dependency but exposes participants/timing, requires client entropy, and permits selective non-reveal. It is the recommended November learning exercise, not confidential execution.

**Proposal:** a dedicated private-witness proof could eventually prove a specified small function while hiding inputs from public verifiers. The prover still sees those inputs unless another mechanism changes that boundary. Its public outputs, commitments, input authorization, and disclosure policy must be designed explicitly. Existing Northstar replay/Portal keys do not certify such a function.

**Attestation subproblem, source implementation dated 2026-10-19:** [Intel's quote-verification API][I1] exposes collateral expiration and quote-verification results separately from the API's return status. A policy must appraise all relevant outputs and approved measurements. A proposed zk-DCAP verifier would prove the specified quote-appraisal computation; it would not prove the confidential application's execution, canonical L1 inputs, private-state freshness, or DA. No zk-DCAP implementation or performance estimate is supplied here.

## Market implication: qualify the requirement, not the acronym

These are proposed discovery questions, not reported feedback:

1. **Game team:** must secrets survive a round, a session, or forever? Who may see them during disputes? Can an incomplete round simply abort?
2. **Auction application:** must losing bids remain secret after clearing? Are winner and price disclosure acceptable? Are there two independent recurring participants and a funded execution path?
3. **Rewards issuer:** is amount privacy enough when addresses and boundary deposits/withdrawals remain public? Who may audit and recover keys?

Existing provider examples establish that developers are building adjacent mechanisms. They do **not** establish Northstar willingness to pay, integration acceptance, comparative latency, or recurring demand. Northstar's plausible distinction is a session-scoped disclosure policy plus clear execution/recovery evidence—not “privacy with Solana speed” without measurements.

**Proceed:** recruit one game team for the no-funds commitment/reveal PoC; estimate 6–9 engineer-days and 1–2 reviewer-days under its stated dependencies. **Hold:** persistent secrets, funded auctions, confidential settlement, or new quote/proof verifiers until requirements and protocol choices receive separate review. Baixiong/Bilal/Jonas review, final ownership, and any substantive team responses remain pending; this draft substitutes for none of them.

## Dated primary sources

Pinned sources are implementation evidence only. Provider web documentation is mutable; access date for every link is 2026-10-23. No link below represents a measured Northstar integration.

[M1]: https://docs.magicblock.gg/pages/tools/tee/introduction
[M2]: https://docs.magicblock.gg/pages/tools/tee/authorization
[M3]: https://docs.magicblock.gg/pages/tools/tee/client-implementation
[M4]: https://github.com/magicblock-labs/ephemeral-rollups-sdk/blob/659d2b0d26c38584bfca6afae68b2b2198b7368c/rust/sdk/src/access_control/structs/permission.rs
[M5]: https://github.com/magicblock-labs/ephemeral-rollups-sdk/blob/659d2b0d26c38584bfca6afae68b2b2198b7368c/ts/web3js/src/access-control/verify.ts
[A1]: https://github.com/arcium-hq/examples/blob/fe15384c239ca90fbbcd8259b60b332275641e57/README.md
[A2]: https://github.com/arcium-hq/examples/blob/fe15384c239ca90fbbcd8259b60b332275641e57/sealed_bid_auction/README.md
[A3]: https://docs.arcium.com/developers/core-concepts
[A4]: https://docs.arcium.com/developers/limitations
[A5]: https://docs.arcium.com/developers
[D1]: https://github.com/Dstack-TEE/dstack/blob/4e7456bc0e0eaaf83c0530d7701e197019003515/README.md
[D2]: https://github.com/Dstack-TEE/dstack/blob/4e7456bc0e0eaaf83c0530d7701e197019003515/docs/security/security-model.md
[D3]: https://github.com/Dstack-TEE/dstack/blob/4e7456bc0e0eaaf83c0530d7701e197019003515/docs/attestation-tdx.md
[T1]: https://github.com/solana-program/token-2022/blob/3d2422bfe5cb5b21682dc7e53b940367de27461a/interface/src/extension/confidential_transfer/mod.rs
[T2]: https://solana.com/docs/tokens/extensions/confidential-transfer
[I1]: https://github.com/intel/confidential-computing.tee.dcap/blob/72f800e9696f382464bc8ef243311edf8bd7dc13/QuoteVerification/dcap_quoteverify/inc/sgx_dcap_quoteverify.h
