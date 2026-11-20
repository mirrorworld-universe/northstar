# Northstar devnet ER reference stack

This stack uses the deployed **ER callback path**: Native discovers requests in its
accepted continuity-v2 lineage. The separate request signer checks finalized
Portal checkpoint membership against the independent Solana devnet L1 RPC and
returns a VRF proof. The Native relay submits fulfillment to ER; the Coordinator
verifies the proof and calls the allowed consumer on ER. This is neither the
standalone L1 coin-flip worker nor the public HTTP VRF API.

Reference evidence: the [Native upgrade](https://github.com/mirrorworld-universe/northstar/actions/runs/43982895796)
retained the relay configuration, and the separate [fresh callback run](https://github.com/mirrorworld-universe/vrf-oracle/actions/runs/43994020478)
verified the on-chain verifier CPI, callback output, and request closure. Those
reports do not qualify this container setup, callback-result L1 settlement, or
SP1 verification.

## Services and custody

| Compose service | Purpose | Private inputs and durable state |
| --- | --- | --- |
| `validator` (UID/GID 10001) | Devnet Agave RPC and Native ER, with the remote-only relay | Devnet identity, dedicated fulfiller wallet, relay bearer; ledger, checkpoints, unsettled state, relay journal |
| `vrf-oracle` (10002) | `vrf-request-signer`, authenticated loopback only | VRF seed and a copy of the bearer; separate admission journal. No fulfillment wallet |
| `prover` (10003) | Pinned SP1 CUDA execution prover in the optional `proof-lab` profile | One assigned GPU, warmed worker socket and proof workspace. No network or signing credentials |

**Do not connect this prover to the deployed continuity-v2 validator.** The
[continuity-v2 runtime](../../docs/checkpoint-continuity-v2-runtime.md#remaining-deployment-gates)
rejects the experimental proof coordinator until its own proof relation is
qualified. `proof-lab` can exercise the existing CUDA worker, but it cannot
resolve a continuity-v2 dispute and is not part of VRF fulfillment. Do not set
`NORTHSTAR_PROOF_*` on the validator. The relay has no local VRF fallback.

## Prepare a reference host

Use a dedicated x86-64 Linux host with rootful Docker Engine and the Compose
plugin, without user-namespace remapping. Provision CPU, RAM and durable SSD
storage for a full devnet RPC node and its program/token indexes. The GPU lab
additionally needs an approved NVIDIA device, driver and Container Toolkit.

**Set the host firewall before starting anything.** The validator and signer
share the host network to preserve the enforced literal-loopback HTTP signer
contract. L1 RPC/PubSub bind `127.0.0.1:8899/8900`; the signer binds
`127.0.0.1:9030`. Native ER binds wildcard addresses: restrict TCP `8910/8911`
and UDP `8912` to the approved ingress path (deny external traffic by default).
Compose does not publish or filter host-network ports. Configure devnet peer
reachability for the approved validator gossip and transport ports separately.
Do not start a second validator instance on the deployed host or with the same
identity/state. This example does not replace the retained systemd units, SSE
Geyser setup, or their guarded rollout procedure.

### Package reviewed artifacts

Preload approved images on the host. For a local reference build, stage only the
release artifacts under this directory's ignored `artifacts/` and build the desired
targets. There are no public image assumptions or private Git credentials in the
build context. Build the binaries for the runtime glibc and verify the artifact
and image digests before staging them on a managed host.

- `artifacts/validator/agave-validator`: use the reviewed Native bundle, built
  with `northstar/checkpoint-continuity-v2,solana-core/vrf-relay`. The repository
  has a [release build recipe](../../../.github/scripts/build-continuity-v2.sh).
- `artifacts/signer/vrf-request-signer`: from the reviewed `vrf-oracle` revision,
  `cargo build --locked --release --bin vrf-request-signer` keeps the default
  local-prover feature inside the signer only. Do not use the offline
  `northstar-vrf-oracle` crate in this repository.
- For `prover`, put the pinned CUDA `northstar-zkvm-replay-script` in
  `artifacts/prover/bin/`. Copy this revision's `gpu-worker.py` and
  `gpu-prover.py` from [`northstar/scripts`](../../scripts) into
  `artifacts/prover/`. Copy `partial-candidate-v1.json` and `fixture-v1.bin` from the
  matching [`northstar/zkvm-replay`](../../zkvm-replay) into
  `artifacts/prover/replay/`. Stage the runtime-compatible CUDA/Icicle library
  closure in `artifacts/prover/lib/`, including Icicle's `backend/`. Use the
  [GPU userspace recipe](../../docs/gpu-userspace-provisioning-v1.md) and the
  [pinned server build](../../scripts/provision-gpu-server.sh). Stage the matching
  SP1 server (`.sp1/bin/sp1-gpu-server`) and wrapping artifacts under the prover
  state directory (the prover's `$HOME`). The container has no external network:
  complete an offline compatibility proof on the target GPU before relying on it.

From this directory, build whichever images you did not preload:

```sh
docker build --target validator --tag northstar/er-validator:reference .
docker build --target signer --tag northstar/vrf-request-signer:reference .
# Optional; on the approved GPU setup, use the matching userspace base if needed.
docker build --target prover --tag northstar/er-prover:reference .
```

The Dockerfile defaults to Ubuntu 24.04. Supply a digest-pinned, compatible
Ubuntu base via `--build-arg BASE_IMAGE=...` for managed builds. Check dynamic
linkage against the selected base and driver, not just on the build host.

### Stage configuration, credentials and state

Use a **devnet continuity-v2 profile approved for this host**: Portal and
deployed ProgramData hashes, SessionV2 and context, Coordinator, Bai verifier,
allowed consumer, VRF public key, and the dedicated fulfiller.
`config/*.example.json` shows the config shapes and container paths. Fill
`relay.json` and `signer.json` from the same approved handoff, not from guessed
policy values.
Copy its signer policy digest from the handoff. Preserve any retained/historical
policy bindings. Change only credential and state paths to the container paths;
do not change policy or reuse secrets from the live runner for a reference host.
The independent signer RPC stays `https://api.devnet.solana.com/`.

**With Ansible:** copy and edit `ansible/inventory.example.yml`. Put the reviewed
`relay.json` and `signer.json` in the inventory's `northstar_profiles_dir` on the
control node. Then run:

```sh
ansible-playbook -i /path/to/inventory.yml ansible/stage.yml
```

The playbook creates the mount directories with fixed ownership, stages Compose,
`.env`, and the two config files, then validates the Compose model. It refuses
any existing container in the reference project and does not start, upgrade, or
stop services. It does not provision Docker, pull images, or handle secret material.

**Without Ansible:** use `.env.example` to set `NORTHSTAR_STACK_DIR` and
`NORTHSTAR_PORTAL_ID`, then copy `compose.yaml` and your `.env` to the stack
directory. Place the two reviewed configs in `config/`, readable by their
container service (for example root-owned files with mode `0444` in a host-only
`0700` directory). Manually create the following directories and credential files.
The same credential and state requirements apply after staging with Ansible.

| Host path relative to `NORTHSTAR_STACK_DIR` | Owner | Mode |
| --- | --- | --- |
| `credentials/validator/identity.json`, `fulfiller.json`, `bearer` | `10001:10001` | `0400` |
| `credentials/signer/vrf.json`, `bearer` | `10002:10002` | `0400` |
| `credentials/validator/`, `state/validator/` | `10001:10001` | `0700` |
| `credentials/signer/`, `state/signer/` | `10002:10002` | `0700` |
| `state/prover/` | `10003:10003` | `0700` |

Materialize the credentials on the target from your secret store. The two bearer
files contain the same secret; the seed must only be in the signer's credential
file. Keep credentials out of images, Git, `.env`, and command arguments. Keep
the ledger/checkpoints and both journals on local durable storage. A retained
profile needs its matching original state; an empty journal is not a recovery
path. Do not use the playbook to migrate or reset a live stack.

## Start and check the reference

From `NORTHSTAR_STACK_DIR` on the dedicated host, after the operator approves the
firewall, identities, profile and initial state, run as the Docker operator:

```sh
docker compose --profile proof-lab config --quiet
docker compose up -d --no-recreate --wait --wait-timeout 900 validator
docker compose ps
docker compose logs --tail=50 validator vrf-oracle
```

Compose waits for the signer's authenticated `/v1/info` before starting Native.
Native readiness checks that same signer's pinned policy digest, L1 `getHealth`,
and ER `getSlot`. A startup wait timeout does not roll back containers; inspect
the existing processes and logs. These are readiness checks only. Follow the existing
[isolated ER qualification runbook](https://github.com/mirrorworld-universe/vrf-oracle/blob/main/deploy/ISOLATED-ER.md)
for an approved fresh request; check successful Coordinator/verifier/consumer
instructions on ER, matching output, and request closure. No program deployment,
funding, delegation or request submission is automated here.

Start and test the separate proof lab when the GPU and cache are ready:

```sh
docker compose --profile proof-lab up -d --no-recreate --wait --wait-timeout 600 prover
docker compose --profile proof-lab exec -T prover \
  python3 /opt/northstar/prover/gpu-worker.py preflight
```

For a compatibility proof use the existing `groth16 WITNESS MEASUREMENTS PROFILE`
client from a fresh directory under `/var/lib/northstar-prover`. Worker readiness
requires a successful warm-up against the pinned fixture; neither that readiness
nor a lab proof enables proof upload on v2.

Restart policy is deliberately manual, matching the guarded signer rollout.
For any interruption or replacement, reconcile with both journals and L1/ER
before resuming. Preserve persistent state across container stops; do not remove
it or create replacement credentials as a restart procedure.
