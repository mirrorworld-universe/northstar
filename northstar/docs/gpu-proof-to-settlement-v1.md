# GPU proof through settlement and recovery

The real Portal SBF live fixture now continues beyond `ResolveChallenge`:

1. Generate a fresh CUDA proof with the pinned guest/key and resolve the isolated transaction.
2. Wait for the unchanged checkpoint challenge deadline; commit and release the proposer bond.
3. Apply the first settlement batch, persisting the production `DurableSettlementPlan` encoding.
4. Optionally wait for a full L1 snapshot containing that batch, SIGKILL the validator, and restart the same ledger.
5. Reload the plan, replay already-applied chunks through the production retry builder, and finish settlement.
6. Assert all 16 account values, unchanged account lamports/ownership, terminal checkpoint/session/cursor state, and exact bond conservation including actual transaction fees.

## Measured results

| Run | Challenge → resolution | Resolver CU | Resolution → settlement |
| --- | ---: | ---: | ---: |
| Combined proof/settlement with batch replay | 123.915s | 127,976 | 200.582s |
| Combined proof/settlement with SIGKILL recovery | 123.997s | 127,976 | 258.197s |

Both passed without slot warps. The crash run used finalized fence **829**, full snapshot **900**, and distinct process PIDs. It completed the test in 400.24s, including GPU preflight and natural L1 deadline/snapshot waits. The retained proof, witness, measurements, serialized settlement plan, genesis fixtures and restart evidence are under [`evidence/combined-settlement-v1`](../zkvm-replay/evidence/combined-settlement-v1/). `SHA256SUMS` covers those files. CPU-only SBF verifier CI also accepts this proof and rejects changed envelope/public fields.

## Scope of the fixture

Genesis explicitly supplies already-delegated accounts and Portal delegation records matching the captured transaction pre-state. This is not a live delegation/CPI demonstration. The fixture's 16 program-owned ER accounts each receive a data update; settlement writes those exact captured post-values on L1, rather than substituting an empty settlement.

Settlement targets use deterministic byte values 128–143. The older proof-only fixture uses 64–79, which includes the harness Portal program address (`[66; 32]`) and therefore cannot also be loaded as L1 delegated accounts. The original frozen fixture remains unchanged; the new witness uses the same pinned guest executable/key.

The settlement fixture is intentionally limited to data changes with unchanged lamports and owner. It does not assert a new general-purpose mapping from arbitrary transaction effects to settlement actions. Genesis already has rent-exempt account metadata; rent-epoch bookkeeping in ER captures is not a settlement write.

The crash test uses the agreed **snapshot-fenced L1 boundary** and a serialized-plan reload in the surviving test driver. It SIGKILLs the validator, not the test driver. Arbitrary-crash L1 persistence, automatic service ownership of this fixture's checkpoint, fresh delegation, and independent production acceptance remain outside this evidence.

## Run

Build the default validator and an explicit prototype Portal SBF. Configure the GPU adapter as described in [runner setup](recovery-and-gpu-runner-v1.md), then run from the repository root:

```sh
export NORTHSTAR_LIVE_PROVER="$PWD/northstar/scripts/gpu-prover-ssh.sh"
export NORTHSTAR_LIVE_PORTAL_SBF="$PWD/target/deploy/northstar_portal.so"
# Set authorized SSH destination, remote runner and artifact directory first.
bash northstar/scripts/live-gpu-settlement.sh settle
bash northstar/scripts/live-gpu-settlement.sh crash-settling
```

The runner exports fresh trusted genesis fixtures before launch, refuses occupied RPC endpoints, passes environment explicitly to tmux, bounds readiness waits, retains private launch/log files only in its fresh evidence directory, and cleans up validator/test sessions. Only reviewed public artifacts should be copied into the repository. Do not deploy the prototype program or enable the production verifier as part of this test.
