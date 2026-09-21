# L40S proof performance

Experimental, opt-in host changes. The guest ELF, program key, public inputs, proof
ABI, verification checks, and default one-shot runner remain unchanged.

## Bottleneck and measured reuse

Same frozen fixture, SP1 6.1.0, one L40S, existing circuit/download caches:

| Configuration | First proof | Subsequent proofs |
| --- | ---: | ---: |
| Default Go concurrency, persistent client | 67.945s | 44.957s / 44.478s |
| `GOMAXPROCS=8`, persistent client | 81.154s | 51.406s / 50.756s |

These are **prove + wrap**, excluding client initialization, key setup, and SDK
verification. Reusing the prover reduces this fixture's mean warm proof time by
34.2% versus its first sample. Eight Go threads were slower and are not adopted.
The first sample is process-cold, not download/cache-cold.

The default server spends approximately 18.6s reading Groth16 R1CS and 3.2s
reading the proving key, once per server process. Its subsequent Groth16 proof
generation still takes approximately 30s. Logs explicitly report
`acceleration=none`: linking Icicle into the SDK client does **not** accelerate
the separate server's wrapping stage.

The repetition argument exercises this reuse without a service:

```sh
# In northstar/zkvm-replay, with the preserved ELF and CUDA environment prepared:
SP1_PROVER=cuda cargo run --release --locked --features cuda \
  -p northstar-zkvm-replay-script -- \
  groth16 fixture-v1.bin measurements.json persistent-client 3
```

Samples receive distinct artifact names. Every sample checks public inputs against
native replay and performs SDK proof verification.

## Opt-in warm worker

`northstar/scripts/gpu-worker.py` maintains one prover/client/key and handles
successive witnesses. Three distinct retained live witnesses produced verified
proofs in **46.160s, 44.521s, 43.870s**. Complete local worker calls took
46.55s, 44.92s, 44.26s, respectively; these exclude SSH transport. The three proofs
also pass the CPU-only real Portal SBF verifier and changed-field rejection suite.

The worker:

- Binds only a private, owner-only Unix socket; rejects concurrent requests.
- Proves and verifies the frozen baseline fixture before advertising readiness.
- Checks the manifest key, native public inputs, SDK verification, envelope,
  request identity, witness hash, and 120s prove-and-wrap limit.
- Bounds each request, including restart/warm-up, to 180s plus bounded cleanup;
  callers may request a shorter integer deadline with
  `NORTHSTAR_GPU_REQUEST_TIMEOUT=1..180`.
- Limits request frames to 8KiB and witnesses to 4MiB. It refuses artifact
  overwrites and never substitutes CPU proving or silently falls back.
- Ends the prover after a failed request. The next request starts and warms a
  fresh prover. A pipe guardian also cleans up if the supervisor is killed.
- Retains private job directories and logs for diagnosis; operators own retention
  and disk monitoring. Run only one worker per GPU and stop it when unused.

On the authorized L40S, a one-second deadline returned rejection in **1.06s**;
GPU cleanup completed within the drill's ten-second bound. A fresh worker then
warmed and produced another verified proof. Supervisor SIGKILL cleanup and normal
SIGTERM shutdown were also exercised. CPU regression tests cover interruption
*during* cleanup, not just a normally exiting parent.

Prepare the CUDA environment and `NORTHSTAR_GPU_PROVER` as described in
[gpu-userspace-provisioning-v1.md](gpu-userspace-provisioning-v1.md), then launch
in a detached tmux session:

```sh
tmux new-session -d -s northstar-gpu-worker \
  'python3 northstar/scripts/gpu-worker.py serve "$HOME/northstar-gpu-worker/worker.sock" "$HOME/northstar-gpu-worker/state"'
export NORTHSTAR_GPU_WORKER_SOCKET="$HOME/northstar-gpu-worker/worker.sock"
python3 northstar/scripts/gpu-worker.py preflight
# In a fresh artifact directory:
python3 /path/to/northstar/scripts/gpu-worker.py \
  groth16 /path/to/witness.bin measurements.json warm-worker
```

The socket appears only after verified warm-up. While starting, poll readiness
rather than opening a challenge. Existing `gpu-prover-ssh.sh` can call a remote
wrapper which exports `NORTHSTAR_GPU_WORKER_SOCKET` and executes this adapter;
its `preflight` / `groth16 WITNESS MEASUREMENTS PROFILE` contract is unchanged.

## Wrapping acceleration investigation

The pinned SP1 source explicitly supports a server-side
[`groth16-cuda` feature](https://github.com/succinctlabs/sp1/blob/d454975ac7c1126097e36eceda9bce2cb9899da4/sp1-gpu/crates/server/Cargo.toml).
A CUDA 13.1, `sm_89` server build with that feature succeeded using the private
Icicle prefix and header overlay, without modifying the original server binary
or system headers. SDK-verified proof times were **49.806s / 24.242s / 23.604s**;
all three also pass the real Portal SBF verifier. Warm proofs average 46.5% faster
than the default server's warm samples. Groth16 generation fell from approximately
30s to 7.7s, with logs reporting `acceleration=icicle`.

`provision-gpu-server.sh ICICLE_PREFIX NEW_SERVER_PREFIX` reproduces the pinned
build without replacing the default server. Its `bin/with-gpu-server COMMAND`
wrapper selects the private server and libraries. The complete script succeeded
on the authorized host; its resulting server warmed the worker and completed
three fresh live Portal resolutions at **127,976 / 127,977 / 127,976 CU**.
A 21-second request deadline also stopped proving, released GPU resources within
ten seconds, and allowed restart followed by another verified proof.

The intended deployment colocates validator and GPU worker on one host, using
the local Unix socket. SSH is only the current two-machine test bridge, not the
production architecture. Those live runs took 95.6s / 87.4s / 84.0s to resolution,
including substantial SSH/SFTP overhead; do not treat these as colocated latency.
Full colocated end-to-end validation remains pending. No SSH transport changes
are included. `live-gpu-settlement.sh resolve` exercises proof/resolution without
waiting for settlement, and forwards the local worker socket configuration.

## Validation and boundaries

Evidence: `northstar/zkvm-replay/evidence/proof-performance-v1/`.
CPU CI exercises worker lifecycle tests and verifies the retained worker proofs.
The SP1 SDK host crate is separate from the validator workspace; CUDA build and
live proving validation run on the GPU host, not ordinary CI.

No rollout, cold-cache latency guarantee, automatic service settlement claim, or
independent production acceptance is implied. See the existing
[gpu-independent-review-v1.md](gpu-independent-review-v1.md) for unchanged
settlement and recovery boundaries.
