# Continue a fully uploaded prepared fresh activation

Use **Continue prepared fresh native activation** from `master` only for a reviewed fresh plan whose programs are already finalized but whose validator has never been stopped or started by that plan. Supply the exact artifact source revision, previous SessionV2 context, and retained plan digest.

The workflow loads the original private reservation and staging manifest; it does not rebuild artifacts, regenerate program keys, redeploy programs, or fund a wallet. It verifies completed program observations and unchanged previous process/session/genesis using the recorded canonical L1 endpoint. It records the current controller revision separately from the original artifact source revision.

Admission rejects missing/changed plans, unresolved uploads, native activation/runtime/start records, and changed old process identity. Before entering native activation, a durable continuation intent binds the plan, controller, original artifact revision, program observations, and previous service/PID/start time.

The original guarded fresh lifecycle then runs, including its independent upstream catch-up checks, snapshot/replay fences, and SOL/SPL qualification. Those readiness checks are not replaced by a self-comparison. Old on-chain programs and archived local state remain preserved. Post-start failure requests quarantine of only the verified new runtime; it never automatically rolls back or resets state.

A successful public report requires matching `qualified` progress and a passed `fresh-stack` report for the retained plan, programs, profile, and distinct new context. Failed or uncertain continuation records require operator reconciliation and cannot be repeated automatically. For the specifically reviewed prestart controller failure, the operator may acknowledge its exact record digest after read-only evidence proves the same old process and no native activation/start state. One separate linked continuation record preserves the original failed record; any existing additional record or quarantine evidence blocks another pass. Release mode selectors are not used by this explicit workflow.

Independent public HTTPS reads are paced to one request per second. Only read methods may recover from HTTP 429: at most four attempts within a 45-second call budget, respecting bounded numeric `Retry-After` and otherwise using bounded backoff. Writes, unrelated HTTP failures, loopback requests, and excessive/invalid retry delays are not replayed. Independent genesis/slot readiness is checked before recording a native continuation attempt; unavailable data never qualifies the runtime.

For already completed program uploads, the deployment controller validates their finalized observations and returns before unnecessary rent/funding queries or worker/submission actions. A public rent RPC outage must not block this completed-upload path.
