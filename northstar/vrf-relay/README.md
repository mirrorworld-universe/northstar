# Remote-only ER VRF relay

This is the explicitly approved public, relay-only snapshot of the reviewed oracle sources in `source-lock.json`. It contains request decoding, public proof verification, bounded RPC access, remote signer transport and the delivery journal. It does not contain `OracleKey`, a VRF seed loader, the request signer, the signer policy implementation, or an HTTP proof server. There is no feature that enables local VRF proving.

`../vrf-interface` contains the public versioned checkpoint-membership wire types. The snapshot tool at `ci/vrf-relay-snapshot` removes local-prover items through a Rust syntax tree and records upstream and published file hashes. CI verifies those hashes without accessing the private repository. Updates require a reviewed immutable source revision and an explicit regeneration; do not edit snapshot files independently.

## Native integration

Build with `solana-core/vrf-relay` and explicitly supply `--vrf-relay-config FILE` together with `--portal`. With no configuration, no relay starts and no delivery credential is read.

The module discovers candidates only from accepted continuity state. It produces an exact native typed-Merkle witness only when that state matches its accepted L1 checkpoint head. ER slot callbacks are bounded wake-ups, not authentication. The independently configured signer must verify finalized Portal checkpoint membership through its pinned L1 authority before producing a proof. The canonical alpha and on-chain Verify CPI ABI are unchanged.

The validator receives only its dedicated fulfillment wallet and a transport bearer credential. The separate signer receives the VRF seed, has no fulfillment wallet, and keeps its own durable admission journal. A missing witness, unavailable signer, mismatched policy or changed proof input never enables a local fallback. Relay shutdown is cancellable and joined before the ER runtime releases its lineage; the commitment source holds only a weak lineage-store reference.

## Guarded activation

The release selector `NORTHSTAR_VRF_RELAY_PROFILE_SHA256` freezes the exact private profile handoff before builds. It is allowed only for an ordinary retained native upgrade, not fresh-stack or failed-L1 recovery. The controller binds the generated configuration and service arguments to its immutable plan, checks the independent signer before stopping the old runtime, and carries the profile forward on later upgrades. Delivery credentials use distinct systemd credentials, materialized as owner-only `0400` files in a private runtime directory before the relay starts. This preserves strict file checks even when systemd exposes delivered credentials with group-readable mode. The Native unit forbids privilege elevation and cannot access the signer credential directories.

Native-upgrade readiness does **not** qualify a VRF callback. Its report explicitly leaves callback qualification false. A separate fresh request, valid on-chain proof verification, callback receipt and closed request are required. Program upload and funding are outside the Native upgrade controller.

## Scope

Development-only. A finalized optimistic checkpoint is an L1-accepted commitment, not a ZK execution proof. The current devnet signer trusts the explicitly pinned independent official L1 RPC authority; this is not a light client. Process credential isolation assumes trusted host administration and is not an independent-operator threshold VRF. The underlying RFC9381 implementation remains an unaudited development candidate. No mainnet or production approval is implied.
