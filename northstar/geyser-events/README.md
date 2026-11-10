# Northstar Geyser events

A Geyser shared library that extracts raw program events and serves SSE
streams. The public L1 RPC project uses the validator's standard L1 Geyser
service and its existing native Solana JSON-RPC. Staging follows Solana devnet;
production follows Solana mainnet-beta. Both services reuse the Northstar node
in that environment, not another RPC node or an ER.

The independent ER Geyser service remains available as an explicit, separate
configuration. Public authentication, IDL decoding, persistence, RPC proxying,
and downstream fan-out belong to HyperFuse, not this plugin.

## Build

From the validator workspace:

```sh
cargo build --release -p northstar-geyser-events
```

The output on Linux is `target/release/libnorthstar_geyser_events.so`. Build the
validator and plugin from the same checkout, pinned Rust toolchain, and Geyser
interface version. Geyser passes Rust trait objects; these libraries are not
ABI-compatible across arbitrary validator/compiler versions.

## Configure

Use `example-devnet.json` for staging and `example-mainnet.json` for production.
Load either through **`--geyser-plugin-config`**, never
`--er-geyser-plugin-config`. `libpath` is resolved relative to the config file
by the Geyser loader. Change it if the shared library is deployed elsewhere.

The validator's cluster configuration selects the L1 network, not the plugin
name or an upstream RPC URL. Pin `--expected-genesis-hash` to:

- Solana devnet: `EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG`.
- Solana mainnet-beta: `5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d`.

Use that environment's existing Northstar cluster, identity and ledger
configuration. A single native validator follows one Solana cluster; do not
reuse a devnet ledger as a mainnet ledger. No Portal or ER configuration is
required for L1 RPC or L1 Geyser.

For a normal non-voting mainnet data node, add:

```sh
--no-voting \
--full-rpc-api \
--private-rpc \
--rpc-bind-address 127.0.0.1 \
--rpc-port 8899 \
--expected-genesis-hash 5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d \
--enable-rpc-transaction-history \
--enable-extended-tx-metadata-storage \
--geyser-plugin-config northstar/geyser-events/example-mainnet.json
```

For staging, substitute the devnet genesis and `example-devnet.json` while
retaining the staging node's cluster configuration. Native JSON-RPC on port
8899 and native PubSub on port 8900 serve the same Solana L1 as the SSE plugin.
Do not route these public APIs to the ER RPC port. Put TLS, authentication and
rate limits in front of the native RPC and SSE listeners.

The existing native JSON-RPC handles transaction details and backfill; no
third-party RPC proxy is introduced. Its history is limited by local ledger
retention unless archival storage is enabled. Program-account indexing and
retention must be sized for the desired workload; enabling the API does not
create an archive or provision indexes.

To subscribe to ER events on a Northstar validator with Portal enabled, add:

```sh
--portal "$PORTAL_PROGRAM_ID" \
--er-geyser-plugin-config northstar/geyser-events/example-er.json
```

`solana-test-validator` also accepts `--er-geyser-plugin-config`. Its Portal is
enabled by default. Both flags are repeatable. L1 and ER plugins can run in the
same validator, but must use different listen addresses/ports: the examples
use `127.0.0.1:9100` and `127.0.0.1:9101`. They do not share ordering, cursors,
watch lists, or replay buffers. On a release-managed node, L1 already owns
port 9101; select a different ER listen port before adding an ER plugin.

**ER commitment is local:** an ER bank freezing makes its events eligible for
both `confirmed` and `finalized` configuration. This is the existing ER RPC
commitment model, not proof of settlement or finality on L1. Empty ER slots do
not produce history notifications. ER transaction indices come from ER history,
including Northstar deposit records, rather than batch-local indices or L1
Blockstore.

The service binds loopback by default and has no authentication or TLS. For a
private HyperFuse deployment, bind a private interface and restrict access with
network policy. Keep native JSON-RPC private.

New release plans expose the **L1 stream on `0.0.0.0:9101`**, with confirmed
delivery and the default connection/resource limits. Anyone with network access
can subscribe; traffic is plaintext until protected by the ingress. CD does not
change firewall rules, TLS, DNS or native RPC bind addresses.

The [release controller](../../.github/scripts/continuity-v2-rollout.md) stages
and hashes the plugin from the same checkout as the validator, installs a
service-owned read-only copy, and adds `--geyser-plugin-config` pointing to
`runtime/geyser-events.json`. Native-only upgrades replace only the previously
managed plugin argument; Portal, session identity, journals and other plugins
are preserved. A new plan records `event_source: "l1"`. Older records without
that field retain their original ER configuration and hashes on retries; they
are not silently reinterpreted. `--event-source er` is an explicit separate ER
choice, not the public L1 default.

Activation and smoke-test restarts wait for `http://127.0.0.1:9101/health`.
This checks local listener readiness, not external reachability, L1 catch-up or
historical event coverage. These devnet deployment controls do not provision a
production mainnet node.

## HTTP and SSE

```sh
curl -N 'http://127.0.0.1:9100/v1/events/PROGRAM_PUBKEY?from_slot=289345000'
curl -N -H 'Last-Event-ID: 289345123:15:0' \
  'http://127.0.0.1:9100/v1/events/PROGRAM_PUBKEY'
curl 'http://127.0.0.1:9100/health'
```

Each connection watches one program. `from_slot` is inclusive; `Last-Event-ID`
is exclusive and takes precedence. A connection without either starts live.
Invalid program IDs/cursors and unsupported query parameters return HTTP 400.
Capacity exhaustion returns HTTP 503. `/health` is listener liveness, not a
claim that the validator is caught up.

```text
id: 289345123:15:0
event: program_event
data: {"program_id":"...","slot":289345123,"block_time":1761450000,"signature":"...","tx_index":15,"log_index":0,"source":"log","commitment":"confirmed","data":"..."}

```

- `data` is standard base64 and includes the Anchor event discriminator.
- `emit!` payloads come from `Program data:` logs attributed to the current
  invocation frame. Events in failed transactions or known failed nested calls
  are excluded. Unfinished log frames are not published.
- `emit_cpi!` payloads come from recorded inner instructions, including programs
  resolved through address lookup tables. Only Anchor's 8-byte event instruction
  tag is stripped; the event discriminator remains. Known failed call frames
  are excluded. CPI data can still be extracted when logs are absent/truncated;
  without the complete trace, nested rollback attribution is necessarily limited.
- `log_index` is a transaction-wide event ordinal, **not** a raw log-line index.
  Log payloads are numbered first, followed by CPI payloads in recorded inner
  instruction order. Ordinals include unwatched payloads and can have gaps.
  Changing subscriptions does not renumber another program's events. Mixed
  log/CPI ordinals do not claim chronological execution order.
- `block_time` comes from block metadata and may be `null`.
- `: ping` is sent every 15 seconds.

Events wait for the configured commitment and the complete transaction count
from block metadata. Bank identity separates different executions of one slot.
Out-of-order transaction callbacks are sorted before publication; descendants
wait for their pending parent. Dead-slot buffers are discarded.

## Replay and gaps

Replay is in memory, only for watched programs and the unsubscribe grace
period. A new watch does not scan old transactions. Reconnect with the last
received ID; clients must deduplicate repeated delivery.

If a cursor predates coverage, replay expires, or pending capacity is exceeded:

```text
event: resync_required
data: {"program_id":"...","last_available_id":"289345123:15:0","reason":"replay_unavailable"}

```

`last_available_id` is the **oldest retained** event, or `null` if none remains.
A slow live consumer receives `reason: "slow_consumer"` and its stream closes.
Other gap notices can be followed by available/live events. Backfill through
native RPC and deduplicate before resuming normal processing. Do not treat a
connected stream as proof that historical coverage is complete.

Unloading/restarting the plugin drops all replay and disconnects clients.
When replacing an ER endpoint with L1, clients must discard ER cursors and
backfill against that environment's native L1 RPC. Never reuse cursors between
devnet and mainnet. L1 IDs are stable across nodes following the same chain. ER cursors are
scoped to one runtime; do not combine independent ERs or assume durable IDs
across a runtime restart without a persisted ER slot floor. ER backfill is
limited to its in-memory history, not the L1 ledger. A durable ER archive and
cross-runtime cursor namespace are outside this plugin's scope.

## Limits

All numeric limits must be positive, except the unsubscribe grace may be zero.
Retention and grace cannot exceed one day. Defaults:

| Setting | Default |
| --- | ---: |
| `commitment` | `confirmed` (`finalized` also supported; never `processed`) |
| `replay_seconds` | 600 |
| `unsubscribe_grace_seconds` | 30 |
| `max_programs` | 1024 |
| `max_connections` | 256 |
| `live_capacity` | 1024 messages per program |
| `max_events_per_program` | 10000 |
| `max_replay_bytes` | 67108864 |
| `max_pending_slots` | 512 bank buffers |
| `max_pending_transactions` | 200000 |
| `max_pending_bytes` | 67108864 |

Byte limits account for payloads and a fixed envelope allowance; they are not a
hard process-RSS limit. Channel references, transaction-index sets, connections,
and replay snapshots have additional bounded overhead. Parsing is skipped for
transactions whose resolved account keys do not intersect the watch list, but
all transaction callbacks still count toward each bank's completion barrier.

## Tests

```sh
cargo test -p northstar-geyser-events
cargo test -p solana-rpc --lib er_history
cargo test -p northstar --lib test_history_observer_receives_executed_transaction_before_slot_freeze

cargo build -p northstar-geyser-events
NORTHSTAR_EVENTS_PLUGIN_LIB="$PWD/target/debug/libnorthstar_geyser_events.so" \
  cargo test -p solana-core --lib test_er_history_dynamic_geyser_sse_roundtrip -- --ignored
```

The opt-in test loads the real shared library, routes ER history through Geyser,
and checks HTTP SSE ordering, the freeze boundary, and plugin shutdown.
