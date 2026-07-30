# Official CSD Node Mining Adapter

This directory turns the pinned official `compute-substrate` node into the
template, candidate submission, and block status backend required by the pool.
The patch is pinned to commit
`d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c` and was compiled against that exact
source revision.

The build applies one self-contained, checksummed adapter patch against the
exact upstream bytes. It includes the independently reviewed P2P backoff
change; the standalone backoff patch remains as provenance but is not applied
a second time. The backoff change fixes an upstream retry-state bug that
discarded an address's failure count as soon as its delay expired, effectively
pinning unreachable peers to a 30-second retry loop. Failure history now
survives expired retry deadlines for 24 hours, successful connections clear
it, and repeated failures progress through `30s`, `120s`, `300s`, `900s`,
`1800s`, and `3600s`.
Only one outbound dial per peer address may remain in flight, preventing the
15-second peer discovery loop from queuing duplicate TCP attempts behind a
long timeout. Expected transport failures use compact `dial_failed` records;
invalid peer identity, malformed addresses, and sync request failures retain
detailed diagnostics.

## Build

```bash
git clone https://github.com/compute-substrate/compute-substrate.git
git -C compute-substrate checkout d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c
ops/csd-node-adapter/apply-and-build.sh "$PWD/compute-substrate"
```

The build runs pool candidate, header receipt, full-block delivery, P2P
backoff, and idle-connection tests before checking or compiling the node. Set
`CSD_NODE_ADAPTER_SKIP_BUILD=1` only for patch applicability checks, never for
a release artifact.

Run the resulting official node with a fresh token of at least 32 characters:

```bash
export CSD_POOL_ADAPTER_TOKEN='<same value as pool CSD_POOL_NODE_TOKEN>'
compute-substrate/target/release/csd node --help
```

The adapter adds authenticated endpoints at `/api/network`,
`/api/rpc/mining/template`, `/api/rpc/block/submit`, and
`/api/rpc/block/status`. It also exposes bounded, authenticated delivery
telemetry at `/api/rpc/block/full-delivery-status`. Existing official
endpoints, including `/health` and `/tx/submit`, remain available. Keep the
adapter RPC on a private network; do not expose it directly to miners or the
public internet.

Receipt evidence is deliberately tiered. A signed A/B signal proves only that
the remote process handled the observation protocol. A full-block external
request followed by a completed local response flush proves a delivery
attempt. The current sync protocol has no remote application ACK, so neither
event proves that an external peer validated, relayed, or adopted the block.
The API therefore reports `remote_application_ack_supported=false` and never
promotes local publish, canonical acceptance, or relay ACK into a remote
receipt. Correlation data is bounded and hash-blinded; peer identities,
addresses, raw hashes, blocks, and credentials are not persisted.

Pool templates use a dedicated non-blocking timestamp selector. It chooses the
later of wall clock and the parent's minimum consensus time, then enforces the
official future-drift bound. The endpoint must never call the regular miner's
blocking `choose_block_time`, because waiting there can return a template whose
parent is already stale.

The release also includes `ops/bin/csd-pool-node-adapter-run.sh` and
`ops/systemd/csd-pool-node-adapter.service`. Install the compiled official node
at `/opt/csd-node/bin/csd`, provision the canonical network genesis at
`/etc/csd-node/genesis.bin`, and set node data/P2P overrides in the pool env as
needed. Copy `ops/env/csd-pool-node.env.example` to
`/etc/csd-pool/node.env`, set a fresh adapter token, and restrict it to
`root:csd-node` mode `0640`. Keeping the node environment separate prevents the
node service account from reading database, operator, or signer credentials.
The launcher maps `CSD_POOL_NODE_TOKEN` to the adapter process.
The RPC defaults to `127.0.0.1:8789`; use a private RFC1918 address only when a
separate pool host must connect, and never bind the adapter directly to a public
interface.

The patch also repairs a one-variable compile error present in the pinned
upstream commit (`_target_hi` versus `hi`). The mining implementation itself
uses the official node's template construction, consensus codec, PoW check,
header index, reorg path, mempool removal, and mined-header broadcast.
