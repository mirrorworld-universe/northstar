# Fundless SP1 network transport evidence

A fresh frozen-baseline checkpoint proof was generated on an NVIDIA A10G through
SP1 SDK 6.8.0's actual hosted/reserved `NetworkProver` path. The endpoint was the
loopback-only test gateway in `northstar/scripts/network-test-gateway.py`, backed
by a warmed Northstar CUDA worker. This was not an official Succinct node or a
public-network request. No PROVE funds or paid authorization were used.

The client completed in **54,981 ms**, including local setup/simulation, network
submission, polling, download, and SDK verification. SDK verification took
**488 ms**. The gateway's worker call took **42,025 ms**. These are one warm sample,
not a latency guarantee or a cold-start result. An earlier cold one-shot backend
exhausted the remaining request deadline, so the successful run used a prewarmed
worker without extending the 120-second limit.

Request ID:
`0x905ac08fcd854ca18780229570124871f143474260bf4a2dba8e323fcb0046a8`.

`rpc-calls.json` records two artifact uploads, one program registration, one proof
request, 22 status polls, and one proof download. The gateway's balance response
was zero. Program registration checked the frozen ELF digest and converted SP1's
network key encoding to its BN254 on-chain key encoding before binding the job.

Artifacts:

- `northstar-sp1-groth16.bin`: fresh SDK proof artifact.
- `northstar-sp1-groth16-onchain.bin`: verified 356-byte envelope.
- `northstar-sp1-public-inputs.bin`: canonical 256-byte inputs for the existing
  `fixture-v1.bin`; no new witness relation is introduced.
- `measurements.json`: network client measurements and request ID.
- `backend-measurements.json`: CUDA worker measurements.
- `gateway-outcome.json`: backend call outcome.
- `preflight.json`: local-hosted readiness result without a paid submission.
- `rpc-calls.json`: observed transport operations.
- `cache-reuse-measurements.json`: a second invocation after both gateway and GPU
  worker were stopped. The adapter reverified the cached proof locally, preserved
  its original request ID and timing, and returned identical on-chain proof bytes.

The test gateway does not implement signature authentication, auctions, payment
settlement, production privacy, or durable server-side requests. Client-side
signing and serialization were exercised, not public-network authentication.
The proof is verified by the network client and independently checked on CPU.
Changed proof bytes and changed public inputs are rejected by the CPU regression.
All test services were stopped afterward; GPU memory returned to zero.
Live Portal resolution and public Succinct compatibility still require separate
validation. Production acceptance remains disabled.
